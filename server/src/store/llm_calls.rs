//! Persists provider call timing and usage.
use sqlx::Row;

use crate::{
    model::{LlmCallSummary, NewLlmCall, Usage},
    Result,
};

use super::{now_ms, Store};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ContextUsageAnchor {
    pub(crate) context_input_tokens: u64,
    pub(crate) message_count: usize,
}

impl Store {
    pub async fn start_llm_call(&self, call: &NewLlmCall) -> Result<()> {
        let _write = self.writes.lock().await;
        let now = now_ms();
        sqlx::query(
            r#"INSERT INTO llm_calls(
                call_id, run_id, conversation_id, provider_call_index, model_hash,
                provider_type, provider_url, request_type, request_url, model_id, display_name,
                reasoning_effort, fast, status,
                created_at_ms, request_started_at_ms, queue_ms, message_count, tool_count, detailed
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'running', ?, ?, 0, ?, ?, 0)"#,
        )
        .bind(&call.call_id)
        .bind(&call.run_id)
        .bind(&call.conversation_id)
        .bind(call.provider_call_index)
        .bind(&call.model_hash)
        .bind(call.provider_type.as_str())
        .bind(&call.provider_url)
        .bind(call.request_type.as_str())
        .bind(&call.request_url)
        .bind(&call.model_id)
        .bind(&call.display_name)
        .bind(&call.reasoning_effort)
        .bind(call.fast)
        .bind(now)
        .bind(now)
        .bind(call.message_count as i64)
        .bind(call.tool_count as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_llm_request(&self, call_id: &str, body: &serde_json::Value) -> Result<()> {
        let body_json = serde_json::to_string(body)?;
        let _write = self.writes.lock().await;
        sqlx::query("UPDATE llm_calls SET request_bytes = ? WHERE call_id = ?")
            .bind(body_json.len() as i64)
            .bind(call_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn record_llm_response_headers(
        &self,
        call_id: &str,
        elapsed_ms: i64,
        http_status: u16,
    ) -> Result<()> {
        let _write = self.writes.lock().await;
        sqlx::query("UPDATE llm_calls SET response_headers_at_ms = ?, ttfb_ms = ?, http_status = ? WHERE call_id = ?")
            .bind(now_ms())
            .bind(elapsed_ms)
            .bind(http_status as i64)
            .bind(call_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub(crate) async fn record_llm_stream_totals(
        &self,
        call_id: &str,
        byte_count: i64,
        event_count: i64,
    ) -> Result<()> {
        if event_count == 0 {
            return Ok(());
        }
        let _write = self.writes.lock().await;
        sqlx::query("UPDATE llm_calls SET first_event_at_ms = COALESCE(first_event_at_ms, ?), response_bytes = response_bytes + ?, stream_event_count = stream_event_count + ? WHERE call_id = ?")
            .bind(now_ms())
            .bind(byte_count)
            .bind(event_count)
            .bind(call_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn record_llm_first_valid_response(
        &self,
        call_id: &str,
        elapsed_ms: i64,
    ) -> Result<()> {
        let _write = self.writes.lock().await;
        sqlx::query("UPDATE llm_calls SET first_valid_response_at_ms = COALESCE(first_valid_response_at_ms, ?), ttfr_ms = COALESCE(ttfr_ms, ?) WHERE call_id = ?")
            .bind(now_ms())
            .bind(elapsed_ms)
            .bind(call_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn record_llm_first_text(&self, call_id: &str, elapsed_ms: i64) -> Result<()> {
        let _write = self.writes.lock().await;
        sqlx::query("UPDATE llm_calls SET first_text_at_ms = COALESCE(first_text_at_ms, ?), ttft_ms = COALESCE(ttft_ms, ?) WHERE call_id = ?")
            .bind(now_ms())
            .bind(elapsed_ms)
            .bind(call_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn record_llm_usage(&self, call_id: &str, usage: Usage) -> Result<()> {
        let usage_json = serde_json::to_string(&usage)?;
        let _write = self.writes.lock().await;
        sqlx::query("UPDATE llm_calls SET input_tokens = ?, output_tokens = ?, total_tokens = ?, cache_read_tokens = ?, cache_write_tokens = ?, reasoning_tokens = ?, usage_json = ? WHERE call_id = ?")
            .bind(as_i64(usage.input_tokens))
            .bind(as_i64(usage.output_tokens))
            .bind(as_i64(usage.total_tokens))
            .bind(as_i64(usage.cache_read_tokens))
            .bind(as_i64(usage.cache_write_tokens))
            .bind(as_i64(usage.reasoning_tokens))
            .bind(usage_json)
            .bind(call_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn finish_llm_call(
        &self,
        call_id: &str,
        status: &str,
        finish_reason: Option<&str>,
        elapsed_ms: i64,
        error_kind: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<()> {
        let _write = self.writes.lock().await;
        sqlx::query("UPDATE llm_calls SET status = ?, finish_reason = ?, finished_at_ms = ?, duration_ms = ?, error_kind = ?, error_message = ? WHERE call_id = ? AND status = 'running'")
            .bind(status)
            .bind(finish_reason)
            .bind(now_ms())
            .bind(elapsed_ms)
            .bind(error_kind)
            .bind(error_message)
            .bind(call_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub(crate) async fn latest_context_usage(
        &self,
        conversation_id: &str,
    ) -> Result<Option<ContextUsageAnchor>> {
        let row = sqlx::query(
            "SELECT usage_json, message_count FROM llm_calls
             WHERE conversation_id = ?
               AND json_extract(usage_json, '$.context_input_tokens') IS NOT NULL
             ORDER BY created_at_ms DESC, rowid DESC
             LIMIT 1",
        )
        .bind(conversation_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let usage: Usage = serde_json::from_str(row.try_get("usage_json")?)?;
        let Some(context_input_tokens) = usage.context_input_tokens else {
            return Ok(None);
        };
        let message_count = row.try_get::<i64, _>("message_count")?;
        let Ok(message_count) = usize::try_from(message_count) else {
            return Ok(None);
        };
        Ok(Some(ContextUsageAnchor {
            context_input_tokens,
            message_count,
        }))
    }

    pub async fn llm_calls(&self, limit: i64) -> Result<Vec<LlmCallSummary>> {
        let rows = sqlx::query("SELECT * FROM llm_calls ORDER BY created_at_ms DESC LIMIT ?")
            .bind(limit.clamp(1, 500))
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(summary_from_row).collect()
    }

    pub async fn llm_call(&self, call_id: &str) -> Result<Option<LlmCallSummary>> {
        sqlx::query("SELECT * FROM llm_calls WHERE call_id = ?")
            .bind(call_id)
            .fetch_optional(&self.pool)
            .await?
            .map(summary_from_row)
            .transpose()
    }
}

fn as_i64(value: Option<u64>) -> Option<i64> {
    value.map(|value| value.min(i64::MAX as u64) as i64)
}

fn summary_from_row(row: sqlx::sqlite::SqliteRow) -> Result<LlmCallSummary> {
    let usage = row.try_get::<Option<String>, _>("usage_json")?;
    Ok(LlmCallSummary {
        call_id: row.try_get("call_id")?,
        run_id: row.try_get("run_id")?,
        conversation_id: row.try_get("conversation_id")?,
        provider_call_index: row.try_get("provider_call_index")?,
        model_hash: row.try_get("model_hash")?,
        provider_type: row.try_get("provider_type")?,
        provider_url: row.try_get("provider_url")?,
        request_type: row.try_get("request_type")?,
        request_url: row.try_get("request_url")?,
        model_id: row.try_get("model_id")?,
        display_name: row.try_get("display_name")?,
        reasoning_effort: row.try_get("reasoning_effort")?,
        fast: Some(row.try_get("fast")?),
        status: row.try_get("status")?,
        finish_reason: row.try_get("finish_reason")?,
        created_at_ms: row.try_get("created_at_ms")?,
        request_started_at_ms: row.try_get("request_started_at_ms")?,
        response_headers_at_ms: row.try_get("response_headers_at_ms")?,
        first_event_at_ms: row.try_get("first_event_at_ms")?,
        first_text_at_ms: row.try_get("first_text_at_ms")?,
        first_valid_response_at_ms: row.try_get("first_valid_response_at_ms")?,
        finished_at_ms: row.try_get("finished_at_ms")?,
        queue_ms: row.try_get("queue_ms")?,
        ttfb_ms: row.try_get("ttfb_ms")?,
        ttft_ms: row.try_get("ttft_ms")?,
        ttfr_ms: row.try_get("ttfr_ms")?,
        duration_ms: row.try_get("duration_ms")?,
        input_tokens: row.try_get("input_tokens")?,
        output_tokens: row.try_get("output_tokens")?,
        total_tokens: row.try_get("total_tokens")?,
        cache_read_tokens: row.try_get("cache_read_tokens")?,
        cache_write_tokens: row.try_get("cache_write_tokens")?,
        reasoning_tokens: row.try_get("reasoning_tokens")?,
        usage: usage
            .map(|value| serde_json::from_str(&value))
            .transpose()?,
        message_count: row.try_get("message_count")?,
        tool_count: row.try_get("tool_count")?,
        request_bytes: row.try_get("request_bytes")?,
        response_bytes: row.try_get("response_bytes")?,
        stream_event_count: row.try_get("stream_event_count")?,
        http_status: row.try_get("http_status")?,
        error_kind: row.try_get("error_kind")?,
        error_message: row.try_get("error_message")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProviderType;

    /// 调用记录不依赖 model_configs 中的行,按稳定模型 ID 汇总。
    #[tokio::test]
    async fn calls_record_without_a_model_config_row() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::connect(&format!(
            "sqlite://{}",
            directory.path().join("test.db").display()
        ))
        .await
        .unwrap();
        let model_hash = "detached-model-hash";
        store
            .start_llm_call(&NewLlmCall {
                call_id: "detached-call".into(),
                run_id: "run".into(),
                conversation_id: "conversation".into(),
                provider_call_index: 0,
                model_hash: model_hash.into(),
                provider_type: ProviderType::OpenAiChat,
                provider_url: "https://provider.example".into(),
                request_type: ProviderType::OpenAiChat,
                request_url: "https://provider.example/v1/chat/completions".into(),
                model_id: "gpt-test".into(),
                display_name: "GPT Test".into(),
                reasoning_effort: None,
                fast: false,
                message_count: 1,
                tool_count: 0,
            })
            .await
            .unwrap();
        store
            .finish_llm_call("detached-call", "completed", None, 10, None, None)
            .await
            .unwrap();
        let overview = store
            .overview(None, None, Some(&format!("[\"{model_hash}\"]")), None)
            .await
            .unwrap();
        assert_eq!(overview.metrics.llm_calls, 1);
        assert_eq!(overview.metrics.successful_calls, 1);
    }

    #[tokio::test]
    async fn latest_context_usage_follows_conversation_chronology() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::connect(&format!(
            "sqlite://{}",
            directory.path().join("test.db").display()
        ))
        .await
        .unwrap();

        for (call_id, model_id, context_input_tokens, message_count) in [
            ("call-a-1", "model-a", 100_u64, 3_usize),
            ("call-b", "model-b", 200_u64, 5_usize),
            ("call-a-2", "model-a", 300_u64, 7_usize),
        ] {
            store
                .start_llm_call(&NewLlmCall {
                    call_id: call_id.into(),
                    run_id: format!("run-{call_id}"),
                    conversation_id: "conversation".into(),
                    provider_call_index: 0,
                    model_hash: model_id.into(),
                    provider_type: ProviderType::OpenAiChat,
                    provider_url: "https://provider.example".into(),
                    request_type: ProviderType::OpenAiChat,
                    request_url: "https://provider.example/v1/chat/completions".into(),
                    model_id: model_id.into(),
                    display_name: model_id.into(),
                    reasoning_effort: None,
                    fast: false,
                    message_count,
                    tool_count: 0,
                })
                .await
                .unwrap();
            store
                .record_llm_usage(
                    call_id,
                    Usage {
                        input_tokens: Some(context_input_tokens),
                        context_input_tokens: Some(context_input_tokens),
                        output_tokens: Some(10),
                        total_tokens: Some(context_input_tokens + 10),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }

        assert_eq!(
            store.latest_context_usage("conversation").await.unwrap(),
            Some(ContextUsageAnchor {
                context_input_tokens: 300,
                message_count: 7,
            })
        );
        assert_eq!(store.latest_context_usage("other").await.unwrap(), None);
    }
}
