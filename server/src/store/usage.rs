//! Completed provider calls. No request, response, headers or conversation content.
use std::{
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use futures_util::TryStreamExt;
use sqlx::{
    sqlite::{SqliteAutoVacuum, SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    Row, SqlitePool,
};
use tokio::io::{AsyncWriteExt, BufWriter};

use crate::{model::Usage, Result};

#[derive(Clone)]
pub struct UsageStore {
    pool: SqlitePool,
    writes: Arc<AtomicUsize>,
    retention_days: u32,
    max_rows: u32,
}

pub struct UsageRecord<'a> {
    pub model_hash: &'a str,
    pub model_id: &'a str,
    pub display_name: &'a str,
    pub provider_name: &'a str,
    pub tokens: Usage,
    pub ttft_ms: Option<i64>,
    pub duration_ms: i64,
    pub status: &'a str,
    pub error_summary: Option<&'a str>,
}

#[derive(Debug, serde::Serialize, PartialEq, Eq)]
pub struct UsageSummary {
    pub today_tokens: i64,
    pub month_tokens: i64,
}

impl UsageStore {
    pub async fn open(path: &Path, retention_days: u32, max_rows: u32) -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .auto_vacuum(SqliteAutoVacuum::Incremental)
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        sqlx::raw_sql("CREATE TABLE IF NOT EXISTS usage_records (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts_ms INTEGER NOT NULL, model_hash TEXT NOT NULL, model_id TEXT NOT NULL,
            display_name TEXT NOT NULL, provider_name TEXT NOT NULL,
            input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
            cache_read_tokens INTEGER NOT NULL DEFAULT 0, reasoning_tokens INTEGER NOT NULL DEFAULT 0,
            total_tokens INTEGER NOT NULL DEFAULT 0, ttft_ms INTEGER, duration_ms INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL CHECK(status IN ('ok','cancelled','error')), error_summary TEXT);
            CREATE INDEX IF NOT EXISTS idx_usage_ts ON usage_records(ts_ms DESC);").execute(&pool).await?;
        let store = Self {
            pool,
            retention_days,
            max_rows,
            writes: Arc::default(),
        };
        store.prune().await?;
        Ok(store)
    }

    pub async fn record(&self, row: UsageRecord<'_>) -> Result<()> {
        let tokens = row.tokens;
        let count = |value: Option<u64>| value.unwrap_or(0).min(i64::MAX as u64) as i64;
        sqlx::query("INSERT INTO usage_records(ts_ms,model_hash,model_id,display_name,provider_name,
            input_tokens,output_tokens,cache_read_tokens,reasoning_tokens,total_tokens,ttft_ms,duration_ms,status,error_summary)
            VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,substr(?,1,256))")
            .bind(chrono::Utc::now().timestamp_millis()).bind(row.model_hash).bind(row.model_id)
            .bind(row.display_name).bind(row.provider_name).bind(count(tokens.input_tokens)).bind(count(tokens.output_tokens))
            .bind(count(tokens.cache_read_tokens)).bind(count(tokens.reasoning_tokens))
            .bind(tokens.total_tokens.map_or_else(|| count(tokens.input_tokens).saturating_add(count(tokens.output_tokens)), |n| count(Some(n))))
            .bind(row.ttft_ms).bind(row.duration_ms).bind(row.status).bind(row.error_summary).execute(&self.pool).await?;
        if self.writes.fetch_add(1, Ordering::Relaxed) % 500 == 499 {
            self.prune().await?;
        }
        Ok(())
    }

    pub(crate) async fn count(&self) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM usage_records")
            .fetch_one(&self.pool)
            .await?)
    }
    pub(crate) async fn clear(&self) -> Result<()> {
        sqlx::query("DELETE FROM usage_records")
            .execute(&self.pool)
            .await?;
        self.prune().await
    }
    async fn prune(&self) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        if self.retention_days > 0 {
            sqlx::query("DELETE FROM usage_records WHERE ts_ms < ?")
                .bind(
                    chrono::Utc::now().timestamp_millis()
                        - i64::from(self.retention_days) * 86_400_000,
                )
                .execute(&mut *tx)
                .await?;
        }
        if self.max_rows > 0 {
            sqlx::query("DELETE FROM usage_records WHERE id IN (SELECT id FROM usage_records ORDER BY ts_ms DESC, id DESC LIMIT -1 OFFSET ?)")
                .bind(self.max_rows).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        sqlx::query("PRAGMA incremental_vacuum(256)")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn today_and_month_summary(&self) -> Result<UsageSummary> {
        let (today_tokens,month_tokens) = sqlx::query_as("SELECT COALESCE(SUM(CASE WHEN ts_ms >= unixepoch('now','localtime','start of day','utc') * 1000 THEN total_tokens END),0),
            COALESCE(SUM(total_tokens),0) FROM usage_records WHERE ts_ms >= unixepoch('now','localtime','start of month','utc') * 1000
            AND ts_ms < unixepoch('now','localtime','start of day','+1 day','utc') * 1000").fetch_one(&self.pool).await?;
        Ok(UsageSummary {
            today_tokens,
            month_tokens,
        })
    }

    pub async fn export_csv(&self, path: &Path) -> Result<()> {
        let mut out = BufWriter::new(
            tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .await?,
        );
        out.write_all(b"ts_iso,model_id,display_name,provider_name,input_tokens,output_tokens,cache_read_tokens,reasoning_tokens,total_tokens,ttft_ms,duration_ms,status,error_summary\r\n").await?;
        let mut rows = sqlx::query("SELECT strftime('%Y-%m-%dT%H:%M:%fZ',ts_ms/1000.0,'unixepoch'),model_id,display_name,provider_name,
            CAST(input_tokens AS TEXT),CAST(output_tokens AS TEXT),CAST(cache_read_tokens AS TEXT),CAST(reasoning_tokens AS TEXT),CAST(total_tokens AS TEXT),
            CAST(ttft_ms AS TEXT),CAST(duration_ms AS TEXT),status,error_summary FROM usage_records ORDER BY ts_ms,id").fetch(&self.pool);
        while let Some(row) = rows.try_next().await? {
            let mut fields = Vec::with_capacity(13);
            for i in 0..13 {
                let value = row.try_get::<Option<&str>, _>(i)?.unwrap_or("");
                fields.push(if value.contains([',', '"', '\r', '\n']) {
                    format!("\"{}\"", value.replace('"', "\"\""))
                } else {
                    value.to_owned()
                });
            }
            out.write_all(fields.join(",").as_bytes()).await?;
            out.write_all(b"\r\n").await?;
        }
        out.flush().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn summaries_export_and_cleanup_follow_finished_calls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.db");
        let store = UsageStore::open(&path, 1, 3).await.unwrap();
        for _ in 0..500 {
            store
                .record(UsageRecord {
                    model_hash: "hash",
                    model_id: "model",
                    display_name: "a,\"b\"",
                    provider_name: "provider",
                    tokens: Usage {
                        input_tokens: Some(8),
                        output_tokens: Some(2),
                        ..Default::default()
                    },
                    ttft_ms: None,
                    duration_ms: 1,
                    status: "ok",
                    error_summary: None,
                })
                .await
                .unwrap();
        }
        assert_eq!(
            store.today_and_month_summary().await.unwrap(),
            UsageSummary {
                today_tokens: 30,
                month_tokens: 30
            }
        );
        let csv = dir.path().join("stats.csv");
        store.export_csv(&csv).await.unwrap();
        let exported = std::fs::read_to_string(csv).unwrap();
        assert_eq!(exported.lines().count(), 4);
        assert!(exported.contains("\"a,\"\"b\"\"\""));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA auto_vacuum")
                .fetch_one(&store.pool)
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("PRAGMA journal_mode")
                .fetch_one(&store.pool)
                .await
                .unwrap(),
            "wal"
        );
        sqlx::query("UPDATE usage_records SET ts_ms = 0")
            .execute(&store.pool)
            .await
            .unwrap();
        store.pool.close().await;
        let reopened = UsageStore::open(&path, 1, 3).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM usage_records")
                .fetch_one(&reopened.pool)
                .await
                .unwrap(),
            0
        );
    }
}
