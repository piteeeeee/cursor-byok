//! One summary row per finished provider attempt. Never receives request or response bodies.
use super::{is_valid_response_event, ModelEvent};
use crate::{
    model::{ModelConfig, Usage},
    store::{UsageRecord, UsageStore},
    Result,
};
use std::{sync::Arc, time::Instant};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct CallRecorder(Arc<Inner>);
struct Inner {
    store: UsageStore,
    hash: String,
    id: String,
    display: String,
    provider: String,
    started: Instant,
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    finished: bool,
    ttft: Option<i64>,
    tokens: Usage,
}
pub(crate) struct CancelOnDrop(CallRecorder);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        let recorder = self.0.clone();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                if let Err(e) = recorder.cancelled().await {
                    tracing::warn!(%e,"record dropped provider call");
                }
            });
        }
    }
}
impl CallRecorder {
    pub(crate) fn new(store: UsageStore, model: &ModelConfig, provider: &str) -> Self {
        Self(Arc::new(Inner {
            store,
            hash: model.model_hash.clone(),
            id: model.model_id.clone(),
            display: model.display_name.clone(),
            provider: provider.into(),
            started: Instant::now(),
            state: Mutex::default(),
        }))
    }
    pub(crate) fn cancel_on_drop(&self) -> CancelOnDrop {
        CancelOnDrop(self.clone())
    }
    pub(crate) async fn is_finished(&self) -> bool {
        self.0.state.lock().await.finished
    }
    pub(crate) async fn event(&self, event: &ModelEvent) -> Result<()> {
        let mut s = self.0.state.lock().await;
        if is_valid_response_event(event) && s.ttft.is_none() {
            s.ttft = Some(self.0.started.elapsed().as_millis() as i64);
        }
        match event {
            ModelEvent::Usage(u) => s.tokens = *u,
            ModelEvent::Done(_) => {
                drop(s);
                self.finish("ok", None).await?;
            }
            _ => {}
        }
        Ok(())
    }
    pub(crate) async fn failed(&self, error: &crate::Error) -> Result<()> {
        let label = match error {
            crate::Error::Http(e) if e.is_timeout() => "upstream timeout",
            crate::Error::Http(_) => "upstream connection failed",
            crate::Error::Provider(_) => "upstream provider error",
            _ => "provider call failed",
        };
        self.finish("error", Some(label)).await
    }
    pub(crate) async fn cancelled(&self) -> Result<()> {
        self.finish("cancelled", None).await
    }
    async fn finish(&self, status: &str, error_summary: Option<&str>) -> Result<()> {
        let i = &self.0;
        let mut s = i.state.lock().await;
        if !s.finished {
            i.store
                .record(UsageRecord {
                    model_hash: &i.hash,
                    model_id: &i.id,
                    display_name: &i.display,
                    provider_name: &i.provider,
                    tokens: s.tokens,
                    ttft_ms: s.ttft,
                    duration_ms: i.started.elapsed().as_millis() as i64,
                    status,
                    error_summary,
                })
                .await?;
            s.finished = true;
        }
        Ok(())
    }
}
