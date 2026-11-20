//! Verifies Conversation recovery and resumable checkpoint state.
#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/fixtures.rs"]
mod fixtures;

use std::{collections::HashSet, sync::Arc};

use cursor_server::{
    cursor::{
        prompting::{PromptAssets, PromptCompiler},
        protocol::{connect, proto::agent::v1 as pb},
        TransportCommand, TransportRegistry,
    },
    model::ToolRoundId,
    provider::{FinishReason, ModelEvent},
    store::BlobId,
};
use prost::Message;

#[tokio::test]
async fn eligible_pending_checkpoint_resumes_tools_before_the_next_model_call() {
    let (_directory, store) = fixtures::temp_store().await;
    let provider = fake_provider::FakeProvider::default();
    provider.push(vec![
        ModelEvent::Start {
            model_call_id: "model-1".into(),
        },
        ModelEvent::ToolCallStart {
            index: 0,
            call_id: "read-1".into(),
            name: "Read".into(),
        },
        ModelEvent::ToolCallArgumentsDelta {
            index: 0,
            delta: "{\"path\":\"/tmp/a\"}".into(),
        },
        ModelEvent::ToolCallEnd { index: 0 },
        ModelEvent::Done(FinishReason::ToolUse),
    ]);
    provider.push(vec![
        ModelEvent::Start {
            model_call_id: "model-2".into(),
        },
        ModelEvent::TextStart,
        ModelEvent::TextDelta("resumed".into()),
        ModelEvent::TextEnd,
        ModelEvent::Done(FinishReason::Stop),
    ]);
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let registry = TransportRegistry::new(
        store.clone(),
        fixtures::empty_config(),
        Arc::new(provider.clone()),
        PromptCompiler::new(assets),
    );

    let first = registry.get_or_create("first-run").await.unwrap();
    let mut first_output = first.subscribe();
    first
        .command(TransportCommand::Append {
            seqno: 0,
            message: Box::new(start_request()),
        })
        .await
        .unwrap();
    let mut first_seqno = 1;
    let mut sent_blob_ids = HashSet::new();
    let staged = loop {
        let server = next_message(&mut first_output).await;
        match server.message {
            Some(pb::agent_server_message::Message::KvServerMessage(kv)) => {
                if let Some(pb::kv_server_message::Message::SetBlobArgs(args)) = &kv.message {
                    sent_blob_ids.insert(args.blob_id.clone());
                }
                acknowledge(&first, &mut first_seqno, kv.id).await;
            }
            Some(pb::agent_server_message::Message::ConversationCheckpointUpdate(state))
                if state.pending_tool_calls.len() == 1 =>
            {
                break state;
            }
            _ => {}
        }
    };
    assert!(
        !sent_blob_ids.contains(
            BlobId::digest(&staged.encode_to_vec())
                .as_bytes()
                .as_slice()
        ),
        "ConversationStateStructure is inline and must not be sent as a Blob"
    );
    let staged_started_at_ms =
        serde_json::from_str::<serde_json::Value>(staged.pending_tool_calls.first().unwrap())
            .unwrap()["providerOptions"]["cursor"]["pendingToolCallStartedAtMs"]
            .as_u64()
            .unwrap();
    assert_eq!(provider.requests().len(), 1);
    first.disconnect().await;

    let resumed = registry.get_or_create("resumed-run").await.unwrap();
    let mut resumed_output = resumed.subscribe();
    let mut resumed_checkpoints = Vec::new();
    let mut resumed_set_blob_ids = HashSet::new();
    resumed
        .command(TransportCommand::Append {
            seqno: 0,
            message: Box::new(resume_request(staged.clone())),
        })
        .await
        .unwrap();
    let mut resumed_seqno = 1;
    let exec_id = loop {
        let server = next_message(&mut resumed_output).await;
        match server.message {
            Some(pb::agent_server_message::Message::KvServerMessage(kv)) => {
                if let Some(pb::kv_server_message::Message::SetBlobArgs(args)) = &kv.message {
                    resumed_set_blob_ids.insert(args.blob_id.clone());
                }
                acknowledge(&resumed, &mut resumed_seqno, kv.id).await;
            }
            Some(pb::agent_server_message::Message::ConversationCheckpointUpdate(state)) => {
                resumed_checkpoints.push(state);
            }
            Some(pb::agent_server_message::Message::ExecServerMessage(exec)) => break exec.id,
            _ => {}
        }
    };
    assert_eq!(
        provider.requests().len(),
        1,
        "resume must execute the pending batch before calling the model"
    );
    let resumed_run_id = store
        .active_run_for_cursor_request("resumed-run")
        .await
        .unwrap()
        .unwrap();
    let resumed_round = store
        .tool_round(&ToolRoundId::new(format!(
            "{}:round:resume",
            resumed_run_id.as_str()
        )))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resumed_round.created_at_ms, staged_started_at_ms);
    resumed
        .command(TransportCommand::Append {
            seqno: resumed_seqno,
            message: Box::new(read_result(exec_id)),
        })
        .await
        .unwrap();
    resumed_seqno += 1;

    let mut saw_settled_barrier_blob = false;
    let mut saw_settled_checkpoint = false;
    loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), resumed_output.recv())
            .await
            .unwrap()
            .unwrap();
        let (flags, payload) = connect::decode_frames(&frame).unwrap().pop().unwrap();
        if flags & connect::END_STREAM_FLAG != 0 {
            break;
        }
        let server = pb::AgentServerMessage::decode(payload).unwrap();
        match server.message {
            Some(pb::agent_server_message::Message::KvServerMessage(kv)) => {
                if let Some(pb::kv_server_message::Message::SetBlobArgs(args)) = &kv.message {
                    resumed_set_blob_ids.insert(args.blob_id.clone());
                }
                if !saw_settled_barrier_blob {
                    assert_eq!(
                        provider.requests().len(),
                        1,
                        "the next model call must wait for the settled Blob barrier"
                    );
                    saw_settled_barrier_blob = true;
                }
                acknowledge(&resumed, &mut resumed_seqno, kv.id).await;
            }
            Some(pb::agent_server_message::Message::ConversationCheckpointUpdate(state)) => {
                if state.pending_tool_calls.is_empty() {
                    saw_settled_checkpoint = true;
                }
                resumed_checkpoints.push(state);
            }
            Some(pb::agent_server_message::Message::InteractionUpdate(update))
                if matches!(
                    update.message,
                    Some(pb::interaction_update::Message::TextDelta(_))
                ) =>
            {
                assert!(
                    saw_settled_checkpoint,
                    "the next model round must not become visible before settled checkpoint"
                );
            }
            _ => {}
        }
    }
    assert!(saw_settled_barrier_blob);
    assert!(saw_settled_checkpoint);
    assert!(staged
        .root_prompt_messages_json
        .iter()
        .all(|id| !resumed_set_blob_ids.contains(id)));
    assert_eq!(provider.requests().len(), 2);
    assert!(resumed_checkpoints
        .last()
        .unwrap()
        .read_paths
        .iter()
        .any(|path| path == "/tmp/a"));

    let mut previous_steps = Vec::new();
    let mut saw_completed_read = false;
    for state in resumed_checkpoints {
        let Some(turn_id) = state.turns.last() else {
            continue;
        };
        let turn = pb::ConversationTurnStructure::decode(
            store
                .get_blob(&BlobId::from_bytes(turn_id).unwrap())
                .unwrap()
                .as_slice(),
        )
        .unwrap();
        let pb::conversation_turn_structure::Turn::AgentConversationTurn(turn) = turn.turn.unwrap()
        else {
            panic!("expected agent turn");
        };
        assert!(turn.steps.len() >= previous_steps.len());
        assert_eq!(
            previous_steps,
            turn.steps[..previous_steps.len()],
            "published Step BlobIDs must be an immutable prefix"
        );
        previous_steps = turn.steps.clone();
        for step_id in &turn.steps {
            let step = pb::ConversationStep::decode(
                store
                    .get_blob(&BlobId::from_bytes(step_id).unwrap())
                    .unwrap()
                    .as_slice(),
            )
            .unwrap();
            if let Some(pb::conversation_step::Message::ToolCall(call)) = step.message {
                if call.tool_call_id.as_deref() == Some("read-1") {
                    assert!(call.started_at_ms.is_some());
                    assert!(call.completed_at_ms.is_some());
                    saw_completed_read = true;
                }
            }
        }
    }
    assert!(
        saw_completed_read,
        "settled Turn must keep the typed result"
    );
}

#[tokio::test]
async fn recovery_rejects_a_kv_get_payload_whose_hash_does_not_match_the_blob_id() {
    let (_directory, store) = fixtures::temp_store().await;
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let registry = TransportRegistry::new(
        store,
        fixtures::empty_config(),
        Arc::new(fake_provider::FakeProvider::default()),
        PromptCompiler::new(assets),
    );
    let handle = registry.get_or_create("bad-blob-run").await.unwrap();
    let mut output = handle.subscribe();
    let expected = BlobId::digest(b"expected");
    handle
        .command(TransportCommand::Append {
            seqno: 0,
            message: Box::new(resume_request(pb::ConversationStateStructure {
                root_prompt_messages_json: vec![expected.as_bytes().to_vec()],
                mode: Some(pb::AgentMode::Agent as i32),
                ..Default::default()
            })),
        })
        .await
        .unwrap();

    let get_id = loop {
        let server = next_message(&mut output).await;
        if let Some(pb::agent_server_message::Message::KvServerMessage(kv)) = server.message {
            if matches!(
                kv.message,
                Some(pb::kv_server_message::Message::GetBlobArgs(_))
            ) {
                break kv.id;
            }
        }
    };
    handle
        .command(TransportCommand::Append {
            seqno: 1,
            message: Box::new(pb::AgentClientMessage {
                message: Some(pb::agent_client_message::Message::KvClientMessage(
                    pb::KvClientMessage {
                        id: get_id,
                        message: Some(pb::kv_client_message::Message::GetBlobResult(
                            pb::GetBlobResult {
                                blob_data: Some(b"corrupt".to_vec()),
                                error: None,
                            },
                        )),
                    },
                )),
            }),
        })
        .await
        .unwrap();

    let error = loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), output.recv())
            .await
            .unwrap()
            .unwrap();
        let (flags, payload) = connect::decode_frames(&frame).unwrap().pop().unwrap();
        if flags & connect::END_STREAM_FLAG != 0 {
            break serde_json::from_slice::<serde_json::Value>(&payload).unwrap();
        }
    };
    assert_eq!(error["error"]["code"], "invalid_argument");
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Blob hash mismatch"));
}

fn start_request() -> pb::AgentClientMessage {
    pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::RunRequest(
            pb::AgentRunRequest {
                action: Some(pb::ConversationAction {
                    action: Some(pb::conversation_action::Action::UserMessageAction(
                        pb::UserMessageAction {
                            user_message: Some(pb::UserMessage {
                                text: "read".into(),
                                message_id: "user-1".into(),
                                mode: pb::AgentMode::Agent as i32,
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                }),
                conversation_id: Some("conversation".into()),
                run_id: Some("first-run".into()),
                requested_model: Some(pb::RequestedModel {
                    model_id: "test-model".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )),
    }
}

#[tokio::test]
async fn cold_restart_recovers_completed_tool_history_and_rebuilds_usage_anchor() {
    use cursor_server::{model::Usage, store::MemoryStore};
    use std::collections::HashMap;
    let provider = fake_provider::FakeProvider::default();
    let usage = ModelEvent::Usage(Usage {
        context_input_tokens: Some(73),
        ..Default::default()
    });
    provider.push(vec![
        ModelEvent::Start {
            model_call_id: "tool-call".into(),
        },
        ModelEvent::ToolCallStart {
            index: 0,
            call_id: "read-1".into(),
            name: "Read".into(),
        },
        ModelEvent::ToolCallArgumentsDelta {
            index: 0,
            delta: r#"{"path":"/tmp/a"}"#.into(),
        },
        ModelEvent::ToolCallEnd { index: 0 },
        ModelEvent::Done(FinishReason::ToolUse),
    ]);
    for text in ["prior reply", "continued", "anchored"] {
        provider.push(vec![
            ModelEvent::Start {
                model_call_id: text.into(),
            },
            ModelEvent::TextStart,
            ModelEvent::TextDelta(text.into()),
            ModelEvent::TextEnd,
            usage.clone(),
            ModelEvent::Done(FinishReason::Stop),
        ]);
    }
    let mut blobs = HashMap::new();
    let mut state = None;
    let mut gets = 0;
    let mut store = MemoryStore::default();
    for turn in 0..3 {
        if turn == 1 {
            store = MemoryStore::default();
        }
        assert_eq!(
            store.latest_context_usage(&"conversation".into()).is_some(),
            turn == 2
        );
        let registry = TransportRegistry::new(
            store.clone(),
            fixtures::empty_config(),
            Arc::new(provider.clone()),
            PromptCompiler::new(PromptAssets::embedded().unwrap()),
        );
        let handle = registry
            .get_or_create(&format!("cold-{turn}"))
            .await
            .unwrap();
        let mut stream = handle.subscribe();
        let mut request = start_request();
        let Some(pb::agent_client_message::Message::RunRequest(ref mut req)) = request.message
        else {
            unreachable!()
        };
        req.conversation_state = state.take();
        let Some(pb::conversation_action::Action::UserMessageAction(action)) =
            req.action.as_mut().unwrap().action.as_mut()
        else {
            unreachable!()
        };
        let user = action.user_message.as_mut().unwrap();
        user.message_id = format!("user-{turn}");
        user.text = format!("question-{turn}");
        handle
            .command(TransportCommand::Append {
                seqno: 0,
                message: Box::new(request),
            })
            .await
            .unwrap();
        let mut seq = 1;
        loop {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(5), stream.recv())
                .await
                .unwrap()
                .unwrap();
            let (flags, payload) = connect::decode_frames(&frame).unwrap().pop().unwrap();
            if flags & connect::END_STREAM_FLAG != 0 {
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&payload).unwrap(),
                    serde_json::json!({})
                );
                break;
            }
            let server = pb::AgentServerMessage::decode(payload).unwrap();
            let reply = match server.message {
                Some(pb::agent_server_message::Message::KvServerMessage(kv)) => {
                    let msg = match kv.message.unwrap() {
                        pb::kv_server_message::Message::SetBlobArgs(a) => {
                            blobs.insert(a.blob_id, a.blob_data);
                            pb::kv_client_message::Message::SetBlobResult(pb::SetBlobResult {
                                error: None,
                            })
                        }
                        pb::kv_server_message::Message::GetBlobArgs(a) => {
                            gets += 1;
                            pb::kv_client_message::Message::GetBlobResult(pb::GetBlobResult {
                                blob_data: Some(blobs[&a.blob_id].clone()),
                                error: None,
                            })
                        }
                    };
                    pb::AgentClientMessage {
                        message: Some(pb::agent_client_message::Message::KvClientMessage(
                            pb::KvClientMessage {
                                id: kv.id,
                                message: Some(msg),
                            },
                        )),
                    }
                }
                Some(pb::agent_server_message::Message::ExecServerMessage(e)) => read_result(e.id),
                Some(pb::agent_server_message::Message::ConversationCheckpointUpdate(cp)) => {
                    state = Some(cp);
                    continue;
                }
                _ => continue,
            };
            handle
                .command(TransportCommand::Append {
                    seqno: seq,
                    message: Box::new(reply),
                })
                .await
                .unwrap();
            seq += 1;
        }
        assert!(state.is_some());
        assert_eq!(
            store
                .latest_context_usage(&"conversation".into())
                .unwrap()
                .context_input_tokens,
            73
        );
        registry.shutdown().await;
    }
    assert!(gets > 0);
    let requests = provider.requests();
    let recovered = serde_json::to_string(&requests[2].history).unwrap();
    for token in ["question-0", "read-1", "value", "prior reply", "question-1"] {
        assert!(recovered.contains(token), "missing {token}");
    }
}

fn resume_request(state: pb::ConversationStateStructure) -> pb::AgentClientMessage {
    pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::RunRequest(
            pb::AgentRunRequest {
                action: Some(pb::ConversationAction {
                    action: Some(pb::conversation_action::Action::ResumeAction(
                        pb::ResumeAction::default(),
                    )),
                    ..Default::default()
                }),
                conversation_state: Some(state),
                conversation_id: Some("conversation".into()),
                run_id: Some("resumed-run".into()),
                requested_model: Some(pb::RequestedModel {
                    model_id: "test-model".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )),
    }
}

fn read_result(id: u32) -> pb::AgentClientMessage {
    pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::ExecClientMessage(
            pb::ExecClientMessage {
                id,
                message: Some(pb::exec_client_message::Message::ReadResult(
                    pb::ReadResult {
                        result: Some(pb::read_result::Result::Success(pb::ReadSuccess {
                            path: "/tmp/a".into(),
                            output: Some(pb::read_success::Output::Content("value".into())),
                            ..Default::default()
                        })),
                    },
                )),
                ..Default::default()
            },
        )),
    }
}

async fn next_message(
    output: &mut tokio::sync::mpsc::UnboundedReceiver<bytes::Bytes>,
) -> pb::AgentServerMessage {
    let frame = tokio::time::timeout(std::time::Duration::from_secs(5), output.recv())
        .await
        .unwrap()
        .unwrap();
    let (flags, payload) = connect::decode_frames(&frame).unwrap().pop().unwrap();
    assert_eq!(
        flags & connect::END_STREAM_FLAG,
        0,
        "unexpected EndStream: {}",
        String::from_utf8_lossy(&payload)
    );
    pb::AgentServerMessage::decode(payload).unwrap()
}

async fn acknowledge(handle: &cursor_server::cursor::TransportHandle, seqno: &mut i64, id: u32) {
    handle
        .command(TransportCommand::Append {
            seqno: *seqno,
            message: Box::new(pb::AgentClientMessage {
                message: Some(pb::agent_client_message::Message::KvClientMessage(
                    pb::KvClientMessage {
                        id,
                        message: Some(pb::kv_client_message::Message::SetBlobResult(
                            pb::SetBlobResult { error: None },
                        )),
                    },
                )),
            }),
        })
        .await
        .unwrap();
    *seqno += 1;
}
