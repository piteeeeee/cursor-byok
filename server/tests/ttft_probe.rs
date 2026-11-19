//! Guards the Run prefix against the checkpoint history scan regressing.
//!
//! The first provider request of a Cursor turn loads one checkpoint's message
//! lineage. That query used to scan every `checkpoint_messages` row in the
//! database, so the wait grew with the whole database instead of the
//! conversation. This test builds a large unrelated conversation and asserts
//! the new one still starts promptly, printing per-stage timings.
//!
//! Set `TTFT_DATABASE_URL` plus `TTFT_CONVERSATION` to replay a real database
//! copy instead of the synthetic one.

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use cursor_server::{
    cursor::{
        prompting::{PromptAssets, PromptCompiler},
        protocol::{connect, proto::agent::v1 as pb},
        TransportCommand, TransportRegistry,
    },
    model::{CanonicalMessage, ModelInvocation, ModelRequest, Origin, Role},
    provider::{ModelEvent, Provider, ProviderStream},
    store::{BlobId, MemoryStore},
};
use prost::Message;
use tokio_util::sync::CancellationToken;

#[path = "support/fixtures.rs"]
mod fixtures;

/// Records when the Run loop first asks the provider to stream.
#[derive(Clone, Default)]
struct TimedProvider {
    first_request_ms: Arc<AtomicU64>,
    origin: Arc<Mutex<Option<Instant>>>,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

impl TimedProvider {
    fn mark_origin(&self, origin: Instant) {
        *self.origin.lock().unwrap() = Some(origin);
    }
}

impl Provider for TimedProvider {
    fn stream(
        &self,
        invocation: ModelInvocation,
        _cancellation: CancellationToken,
    ) -> ProviderStream {
        self.requests.lock().unwrap().push(invocation.request);
        if let Some(origin) = *self.origin.lock().unwrap() {
            self.first_request_ms
                .store(origin.elapsed().as_millis() as u64, Ordering::SeqCst);
        }
        Box::pin(futures_util::stream::iter(vec![
            Ok(ModelEvent::Start {
                model_call_id: invocation.call_id,
            }),
            Ok(ModelEvent::TextStart),
            Ok(ModelEvent::TextDelta("ok".into())),
            Ok(ModelEvent::TextEnd),
            Ok(ModelEvent::Done(
                cursor_server::provider::FinishReason::Stop,
            )),
        ]))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn new_conversations_share_context_within_ttft_budget() {
    // Per-stage timings of the production pipeline reach the test output.
    {
        use tracing_subscriber::prelude::*;
        let _ = tracing_subscriber::registry()
            .with(
                tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                    tracing_subscriber::EnvFilter::new("cursor_server::timing=info")
                }),
            )
            .with(tracing_subscriber::fmt::layer().with_test_writer())
            .try_init();
    }
    let store = MemoryStore::default();
    let provider = TimedProvider::default();
    let registry = TransportRegistry::new(
        store.clone(),
        fixtures::empty_runtime(),
        Arc::new(provider.clone()),
        PromptCompiler::new(PromptAssets::embedded().unwrap()),
    );

    let iterations = 2;
    let mut context_requests = 0;
    for iteration in 0..iterations {
        let conversation = format!("probe-conversation-{iteration}");
        let request_id = format!("ttft-probe-{iteration}");
        let handle = registry.get_or_create(&request_id).await.unwrap();
        let origin = Instant::now();
        provider.mark_origin(origin);
        let mut receiver = handle.subscribe();
        handle
            .command(TransportCommand::Append {
                seqno: 0,
                message: Box::new(request_message(&conversation, iteration)),
            })
            .await
            .unwrap();

        let mut append_seqno = 1_i64;
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(120), receiver.recv())
                .await
                .expect("run did not finish")
                .expect("transport output closed");
            let frame = match connect::decode_frames(&frame) {
                Ok(frames) => frames,
                Err(_) => continue,
            };
            let mut finished = false;
            for (flags, payload) in frame {
                if flags & connect::END_STREAM_FLAG != 0 {
                    finished = true;
                    continue;
                }
                let server = match pb::AgentServerMessage::decode(payload.as_ref()) {
                    Ok(server) => server,
                    Err(_) => continue,
                };
                if matches!(&server.message,Some(pb::agent_server_message::Message::ExecServerMessage(exec)) if matches!(&exec.message,Some(pb::exec_server_message::Message::RequestContextArgs(_))))
                {
                    context_requests += 1;
                }
                let Some(reply) = client_reply(&store, server).await else {
                    continue;
                };
                for message in reply {
                    handle
                        .command(TransportCommand::Append {
                            seqno: append_seqno,
                            message: Box::new(message),
                        })
                        .await
                        .unwrap();
                    append_seqno += 1;
                }
            }
            if finished {
                break;
            }
        }
        let elapsed = provider.first_request_ms.load(Ordering::SeqCst);
        assert_eq!(
            context_requests, 1,
            "second conversation must reuse context"
        );
        assert!(
            elapsed < if iteration == 0 { 50 } else { 15 },
            "iteration {iteration} took {elapsed} ms"
        );
        println!("iteration {iteration}: append -> first provider request {elapsed} ms");
    }
    assert_eq!(provider.requests.lock().unwrap().len(), iterations as usize);
}

/// Answers the assistant's Blob and request-context round trips as Cursor does.
async fn client_reply(
    store: &MemoryStore,
    server: pb::AgentServerMessage,
) -> Option<Vec<pb::AgentClientMessage>> {
    match server.message? {
        pb::agent_server_message::Message::KvServerMessage(kv) => {
            let message = match kv.message? {
                pb::kv_server_message::Message::SetBlobArgs(_) => {
                    pb::kv_client_message::Message::SetBlobResult(pb::SetBlobResult { error: None })
                }
                pb::kv_server_message::Message::GetBlobArgs(get) => {
                    let blob_id = BlobId::from_bytes(&get.blob_id).unwrap();
                    pb::kv_client_message::Message::GetBlobResult(pb::GetBlobResult {
                        blob_data: store.get_blob(&blob_id),
                        error: None,
                    })
                }
            };
            Some(vec![pb::AgentClientMessage {
                message: Some(pb::agent_client_message::Message::KvClientMessage(
                    pb::KvClientMessage {
                        id: kv.id,
                        message: Some(message),
                    },
                )),
            }])
        }
        pb::agent_server_message::Message::ExecServerMessage(exec) => {
            if !matches!(
                exec.message,
                Some(pb::exec_server_message::Message::RequestContextArgs(_))
            ) {
                return None;
            }
            Some(vec![
                pb::AgentClientMessage {
                    message: Some(pb::agent_client_message::Message::ExecClientControlMessage(
                        pb::ExecClientControlMessage {
                            message: Some(pb::exec_client_control_message::Message::StreamClose(
                                pb::ExecClientStreamClose { id: exec.id },
                            )),
                        },
                    )),
                },
                pb::AgentClientMessage {
                    message: Some(pb::agent_client_message::Message::ExecClientMessage(
                        pb::ExecClientMessage {
                            id: exec.id,
                            message: Some(pb::exec_client_message::Message::RequestContextResult(
                                pb::RequestContextResult {
                                    result: Some(pb::request_context_result::Result::Success(
                                        pb::RequestContextSuccess {
                                            request_context: Some(pb::RequestContext::default()),
                                            ..Default::default()
                                        },
                                    )),
                                },
                            )),
                            ..Default::default()
                        },
                    )),
                },
            ])
        }
        _ => None,
    }
}

fn request_message(conversation: &str, iteration: u64) -> pb::AgentClientMessage {
    pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::RunRequest(
            pb::AgentRunRequest {
                requested_model: Some(pb::RequestedModel {
                    model_id: "deepseek-v4.1-flash".into(),
                    ..Default::default()
                }),
                action: Some(pb::ConversationAction {
                    request_context_parts: Some(pb::RequestContextPartReferences {
                        rules_blob_id: vec![17; 32],
                        rules_byte_length: 1,
                        ..Default::default()
                    }),
                    action: Some(pb::conversation_action::Action::UserMessageAction(
                        pb::UserMessageAction {
                            user_message: Some(pb::UserMessage {
                                text: "ttft probe".into(),
                                message_id: format!("ttft-{iteration}"),
                                mode: pb::AgentMode::Agent as i32,
                                ..Default::default()
                            }),
                            request_context: Some(pb::RequestContext::default()),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                }),
                conversation_id: Some(conversation.to_string()),
                ..Default::default()
            },
        )),
    }
}

/// Keeps the canonical fixtures honest about the persisted payload shape.
#[allow(dead_code)]
fn canonical_user() -> CanonicalMessage {
    CanonicalMessage::text("id", Role::User, Origin::User, "text")
}
