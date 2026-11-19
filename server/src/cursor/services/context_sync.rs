//! Hydrates request context blobs supplied by Cursor.
use std::{sync::Arc, time::Duration};

use tokio::sync::{oneshot, Mutex};

use crate::{
    cursor::{protocol::proto::agent::v1 as pb, transport::TransportHandle},
    store::{BlobId, MemoryStore},
    Error, Result,
};

type ContextSender = oneshot::Sender<Result<pb::RequestContext>>;

#[derive(Clone)]
pub(crate) struct RequestContextSynchronizer {
    handle: TransportHandle,
    store: MemoryStore,
    pending: Arc<Mutex<Option<ContextSender>>>,
    last: Arc<parking_lot::Mutex<Option<(String, pb::RequestContext)>>>,
}

impl RequestContextSynchronizer {
    pub(crate) fn new(handle: TransportHandle, store: MemoryStore) -> Self {
        Self {
            handle,
            store,
            pending: Arc::new(Mutex::new(None)),
            last: Default::default(),
        }
    }

    pub(crate) fn previous(&self, conversation: &str) -> Option<pb::RequestContext> {
        self.last
            .lock()
            .as_ref()
            .filter(|(id, _)| id == conversation)
            .map(|(_, ctx)| ctx.clone())
    }

    pub(crate) fn remember(&self, conversation: &str, context: &pb::RequestContext) {
        *self.last.lock() = Some((conversation.to_owned(), context.clone()));
    }

    pub(crate) async fn refresh_if_missing(
        &self,
        references: &pb::RequestContextPartReferences,
        conversation_id: &str,
    ) -> Result<Option<pb::RequestContext>> {
        let mut key = [None, None, None, None];
        for (slot, raw) in key.iter_mut().zip([
            &references.rules_blob_id,
            &references.skills_blob_id,
            &references.subagents_blob_id,
            &references.mcps_blob_id,
        ]) {
            if !raw.is_empty() {
                *slot = Some(BlobId::from_bytes(raw)?);
            }
        }
        if let Some(parts) = self.store.context(&key) {
            return Ok(Some((*parts).clone()));
        }
        if key
            .iter()
            .flatten()
            .all(|id| self.store.get_blob(id).is_some())
        {
            return Ok(None);
        }
        let full = self.load(conversation_id).await?;
        let parts = pb::RequestContext {
            rules: full.rules,
            non_file_rules: full.non_file_rules,
            cloud_rule: full.cloud_rule,
            agent_skills: full.agent_skills,
            skill_options: full.skill_options,
            custom_subagents: full.custom_subagents,
            tools: full.tools,
            mcp_instructions: full.mcp_instructions,
            mcp_file_system_options: full.mcp_file_system_options,
            mcp_meta_tool_options: full.mcp_meta_tool_options,
            ..Default::default()
        };
        self.store.cache_context(key, parts.clone());
        Ok(Some(parts))
    }
    pub(crate) async fn get(&self, id: &BlobId) -> Result<Option<Vec<u8>>> {
        Ok(self.store.get_blob(id))
    }
    pub(crate) async fn load(&self, conversation_id: &str) -> Result<pb::RequestContext> {
        let (sender, receiver) = oneshot::channel();
        let mut pending = self.pending.lock().await;
        if pending.is_some() {
            return Err(Error::Protocol(
                "Cursor request context is already being loaded".into(),
            ));
        }
        *pending = Some(sender);
        drop(pending);

        tracing::info!(
            request_id = self.handle.request_id(),
            conversation_id,
            "requesting uncached Cursor context"
        );

        if let Err(error) = self.handle.emit(&pb::AgentServerMessage {
            ttft_breakdown: None,
            message: Some(pb::agent_server_message::Message::ExecServerMessage(
                pb::ExecServerMessage {
                    id: 0,
                    message: Some(pb::exec_server_message::Message::RequestContextArgs(
                        pb::RequestContextArgs {
                            notes_session_id: Some(conversation_id.into()),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                },
            )),
        }) {
            self.pending.lock().await.take();
            return Err(error);
        }

        let cancellation = self.handle.disconnect_token();
        let result = tokio::select! {
            result = receiver => result.map_err(|_| Error::Protocol("request context response channel closed".into()))?,
            _ = cancellation.cancelled() => Err(Error::Cancelled),
            _ = tokio::time::sleep(Duration::from_secs(60)) => Err(Error::Protocol("request context timed out".into())),
        };
        if result.is_err() {
            self.pending.lock().await.take();
        }
        result
    }

    pub(crate) async fn handle_client(&self, message: &pb::ExecClientMessage) -> bool {
        if message.id != 0 {
            return false;
        }
        let Some(pb::exec_client_message::Message::RequestContextResult(result)) =
            message.message.as_ref()
        else {
            return false;
        };
        let Some(sender) = self.pending.lock().await.take() else {
            tracing::warn!(
                request_id = self.handle.request_id(),
                "unexpected Cursor request context result"
            );
            return true;
        };
        use pb::request_context_result::Result as ContextResult;
        let result = match result.result.as_ref() {
            Some(ContextResult::Success(success)) => success
                .request_context
                .clone()
                .ok_or_else(|| Error::Protocol("Cursor returned empty request context".into())),
            Some(ContextResult::Error(error)) => Err(Error::Protocol(format!(
                "Cursor request context failed: {}",
                error.error
            ))),
            Some(ContextResult::Rejected(rejected)) => Err(Error::Protocol(format!(
                "Cursor rejected request context: {}",
                rejected.reason
            ))),
            None => Err(Error::Protocol(
                "Cursor returned no request context result".into(),
            )),
        };
        let _ = sender.send(result);
        true
    }

    pub(crate) async fn handle_stream_close(&self, id: u32) -> bool {
        id == 0 && self.pending.lock().await.is_some()
    }

    pub(crate) async fn handle_throw(&self, id: u32, message: String) -> bool {
        let sender = if id == 0 {
            self.pending.lock().await.take()
        } else {
            None
        };
        let Some(sender) = sender else { return false };
        let _ = sender.send(Err(Error::Protocol(message)));
        true
    }
}
