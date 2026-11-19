//! Row accounting and cleanup for stored call statistics.

use serde::Serialize;

use crate::Result;

use super::Store;

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct StatisticsStorage {
    pub call_count: i64,
}

impl Store {
    pub async fn statistics_storage(&self) -> Result<StatisticsStorage> {
        let call_count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM llm_calls")
            .fetch_one(&self.pool)
            .await?;
        Ok(StatisticsStorage { call_count })
    }

    pub async fn clear_statistics_storage(&self) -> Result<StatisticsStorage> {
        let _write = self.writes.lock().await;
        sqlx::query("DELETE FROM llm_calls")
            .execute(&self.pool)
            .await?;
        self.statistics_storage().await
    }
}
