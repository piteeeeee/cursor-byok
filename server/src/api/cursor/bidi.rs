//! Accepts ordered Cursor Bidi append requests and routes them by request_id.
use prost::Message;

use crate::{
    cursor::{
        conversation::TransportCommand,
        protocol::{
            events,
            proto::{agent::v1 as agent, aiserver::v1 as ai},
        },
        transport::{TransportParent, TransportRegistry},
    },
    Error, Result,
};

pub struct DecodedAppend {
    pub request_id: String,
    pub seqno: i64,
    pub message: agent::AgentClientMessage,
}

impl DecodedAppend {
    pub fn model_id(&self) -> Option<&str> {
        let agent::agent_client_message::Message::RunRequest(request) =
            self.message.message.as_ref()?
        else {
            return None;
        };
        request
            .requested_model
            .as_ref()
            .map(|model| model.model_id.as_str())
            .filter(|model| !model.is_empty())
            .or_else(|| {
                request
                    .model_details
                    .as_ref()
                    .map(|model| model.model_id.as_str())
                    .filter(|model| !model.is_empty())
            })
    }

    pub fn conversation_id(&self) -> Option<&str> {
        let agent::agent_client_message::Message::RunRequest(request) =
            self.message.message.as_ref()?
        else {
            return None;
        };
        request.conversation_id.as_deref()
    }

    pub fn is_background_task_completion(&self) -> bool {
        let Some(agent::agent_client_message::Message::RunRequest(request)) =
            self.message.message.as_ref()
        else {
            return false;
        };
        matches!(
            request
                .action
                .as_ref()
                .and_then(|action| action.action.as_ref()),
            Some(agent::conversation_action::Action::BackgroundTaskCompletionAction(_))
        )
    }

    pub fn trace_metadata(&self) -> serde_json::Value {
        let Some(message) = self.message.message.as_ref() else {
            return serde_json::json!({
                "append_seqno": self.seqno,
                "message_type": "empty",
            });
        };
        let agent::agent_client_message::Message::RunRequest(request) = message else {
            return serde_json::json!({
                "append_seqno": self.seqno,
                "message_type": client_message_type(message),
            });
        };
        let (action_type, history_messages, history_images) = request
            .action
            .as_ref()
            .and_then(|action| action.action.as_ref())
            .map(|action| match action {
                agent::conversation_action::Action::UserMessageAction(action) => {
                    let history = action.conversation_history.as_ref();
                    (
                        "user_message",
                        history.map_or(0, |history| history.messages.len()),
                        history.map_or(0, history_image_count),
                    )
                }
                agent::conversation_action::Action::BackgroundTaskCompletionAction(_) => {
                    ("background_task_completion", 0, 0)
                }
                agent::conversation_action::Action::ExecutePlanAction(_) => ("execute_plan", 0, 0),
                agent::conversation_action::Action::SummarizeAction(_) => ("summarize", 0, 0),
                _ => ("other", 0, 0),
            })
            .unwrap_or(("none", 0, 0));
        let state = request.conversation_state.as_ref();
        serde_json::json!({
            "append_seqno": self.seqno,
            "message_type": "run_request",
            "conversation_id": request.conversation_id,
            "model_id": self.model_id(),
            "action_type": action_type,
            "conversation_history_messages": history_messages,
            "conversation_history_images": history_images,
            "root_message_count": state.map_or(0, |state| state.root_prompt_messages_json.len()),
            "turn_count": state.map_or(0, |state| state.turns.len()),
            "prefetched_blob_count": request.pre_fetched_blobs.len(),
        })
    }
}

fn client_message_type(message: &agent::agent_client_message::Message) -> &'static str {
    use agent::agent_client_message::Message;
    match message {
        Message::RunRequest(_) => "run_request",
        Message::ExecClientMessage(_) => "exec_client_message",
        Message::ExecClientControlMessage(_) => "exec_client_control_message",
        Message::KvClientMessage(_) => "kv_client_message",
        Message::ConversationAction(_) => "conversation_action",
        Message::InteractionResponse(_) => "interaction_response",
        Message::ClientHeartbeat(_) => "client_heartbeat",
        Message::PrewarmRequest(_) => "prewarm_request",
    }
}

fn history_image_count(history: &agent::ConversationHistory) -> usize {
    use agent::{
        conversation_history_message::Message,
        conversation_history_tool_result_content::Content as ToolContent,
        conversation_history_user_content::Content as UserContent,
    };
    history
        .messages
        .iter()
        .map(|message| match message.message.as_ref() {
            Some(Message::User(user)) => user
                .content
                .iter()
                .filter(|content| matches!(content.content, Some(UserContent::Image(_))))
                .count(),
            Some(Message::Tool(tool)) => tool
                .content
                .iter()
                .filter(|content| matches!(content.content, Some(ToolContent::Image(_))))
                .count(),
            _ => 0,
        })
        .sum()
}

fn decode_client_message(payload: &[u8]) -> Result<agent::AgentClientMessage> {
    agent::AgentClientMessage::decode(payload).map_err(|error| {
        if !cfg!(test) {
            let path = "/tmp/byok-failed-append.hex";
            if let Err(write_error) = std::fs::write(path, hex::encode(payload)) {
                tracing::warn!(
                    path,
                    error = %write_error,
                    "failed to write failed BidiAppend payload hex"
                );
            } else {
                tracing::warn!(
                    path,
                    bytes = payload.len(),
                    "dumped failed BidiAppend payload hex"
                );
            }
        }
        error.into()
    })
}

pub fn decode(request: &ai::BidiAppendRequest) -> Result<DecodedAppend> {
    let request_id = request
        .request_id
        .as_ref()
        .map(|id| id.request_id.as_str())
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Error::Protocol("BidiAppend request_id is required".into()))?;
    let message = match (request.data.is_empty(), request.data_binary.is_empty()) {
        (true, false) => decode_client_message(request.data_binary.as_slice())?,
        (false, true) => {
            let payload = hex::decode(&request.data)
                .map_err(|error| Error::Protocol(format!("invalid BidiAppend hex: {error}")))?;
            decode_client_message(payload.as_slice())?
        }
        (true, true) => {
            return Err(Error::Protocol(
                "BidiAppend contains no AgentClientMessage".into(),
            ));
        }
        (false, false) => {
            return Err(Error::Protocol(
                "BidiAppend cannot set both data and data_binary".into(),
            ));
        }
    };
    Ok(DecodedAppend {
        request_id: request_id.into(),
        seqno: request.append_seqno,
        message,
    })
}

pub async fn append(
    registry: &TransportRegistry,
    request: DecodedAppend,
    parent: Option<TransportParent>,
) -> Result<ai::BidiAppendResponse> {
    let replace_closing = request.model_id().is_some();
    let handle = registry
        .get_or_create_for_append(&request.request_id, replace_closing)
        .await?;
    let _admission = handle.admit()?;
    if let Some(conversation_id) = request.conversation_id() {
        handle.set_conversation_id(conversation_id)?;
    }
    if let Some(parent) = parent {
        handle.set_parent(parent)?;
    }
    if matches!(
        request.message.message.as_ref(),
        Some(agent::agent_client_message::Message::ClientHeartbeat(_))
    ) {
        handle.emit(&events::heartbeat())?;
    }
    handle
        .command(TransportCommand::Append {
            seqno: request.seqno,
            message: Box::new(request.message),
        })
        .await?;
    Ok(ai::BidiAppendResponse {})
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cursor 3.29+/3.31 `cursor-always-local` 发送的真实线级形态：
    /// BidiAppendRequest { request_id: "req-329", append_seqno: 0, data_binary: AgentClientMessage.toBinary() }
    #[test]
    fn decode_accepts_cursor_329_data_binary_run_request_wire_bytes() {
        let client_msg = agent::AgentClientMessage {
            message: Some(agent::agent_client_message::Message::RunRequest(
                agent::AgentRunRequest {
                    conversation_id: Some("conv-329".into()),
                    requested_model: Some(agent::RequestedModel {
                        model_id: "83f1333d63840a0c".into(),
                        ..Default::default()
                    }),
                    action: Some(agent::ConversationAction {
                        action: Some(agent::conversation_action::Action::UserMessageAction(
                            agent::UserMessageAction {
                                user_message: Some(agent::UserMessage {
                                    text: "ping".into(),
                                    message_id: "msg-1".into(),
                                    mode: agent::AgentMode::Agent as i32,
                                    ..Default::default()
                                }),
                                ..Default::default()
                            },
                        )),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )),
        };
        let wire = ai::BidiAppendRequest {
            data: String::new(),
            request_id: Some(ai::BidiRequestId {
                request_id: "req-329".into(),
            }),
            append_seqno: 0,
            data_binary: client_msg.encode_to_vec(),
        }
        .encode_to_vec();
        // 校验 tag 4 (0x22 = field 4, wire type 2) 确实出现在外层 protobuf 字节流中
        assert!(wire.windows(2).any(|w| w[0] == 0x22));

        let raw_request = ai::BidiAppendRequest::decode(wire.as_slice()).unwrap();
        let decoded = decode(&raw_request).unwrap();
        assert_eq!(decoded.request_id, "req-329");
        assert_eq!(decoded.seqno, 0);
        assert_eq!(decoded.conversation_id(), Some("conv-329"));
        assert_eq!(decoded.model_id(), Some("83f1333d63840a0c"));
    }

    #[test]
    fn decode_accepts_hex_data_when_data_binary_is_empty() {
        // 真实捕获样本：KvClientMessage { id: 1, SetBlobResult {} } = 0x1a0408011a00
        let request = ai::BidiAppendRequest {
            data: "1a0408011a00".into(),
            request_id: Some(ai::BidiRequestId {
                request_id: "req-hex".into(),
            }),
            append_seqno: 2,
            data_binary: Vec::new(),
        };
        let decoded = decode(&request).unwrap();
        assert_eq!(decoded.request_id, "req-hex");
        assert_eq!(decoded.seqno, 2);
        assert!(matches!(
            decoded.message.message,
            Some(agent::agent_client_message::Message::KvClientMessage(_))
        ));
    }

    #[test]
    fn decode_rejects_simultaneous_data_and_data_binary() {
        let payload = [0x1a, 0x04, 0x08, 0x01, 0x1a, 0x00];
        let request = ai::BidiAppendRequest {
            data: hex::encode(payload),
            request_id: Some(ai::BidiRequestId {
                request_id: "req-both".into(),
            }),
            append_seqno: 1,
            data_binary: payload.to_vec(),
        };
        let error = decode(&request).err().expect("must reject dual payload");
        assert!(error
            .to_string()
            .contains("BidiAppend cannot set both data and data_binary"));
    }

    #[test]
    fn decode_rejects_empty_data_and_data_binary() {
        let request = ai::BidiAppendRequest {
            data: String::new(),
            request_id: Some(ai::BidiRequestId {
                request_id: "req-empty".into(),
            }),
            append_seqno: 0,
            data_binary: Vec::new(),
        };
        let error = decode(&request).err().expect("must reject empty payload");
        assert!(error
            .to_string()
            .contains("BidiAppend contains no AgentClientMessage"));
    }

    #[test]
    fn decode_rejects_malformed_data_binary_protobuf() {
        let request = ai::BidiAppendRequest {
            data: String::new(),
            request_id: Some(ai::BidiRequestId {
                request_id: "req-bad-pb".into(),
            }),
            append_seqno: 0,
            data_binary: vec![0x0a, 0x0f, 0x01], // 截断的 length-delimited 字段
        };
        let error = decode(&request)
            .err()
            .expect("must reject truncated protobuf");
        assert!(matches!(error, Error::Decode(_)));
    }

    #[test]
    fn decode_rejects_missing_request_id() {
        let request = ai::BidiAppendRequest {
            data: String::new(),
            request_id: None,
            append_seqno: 0,
            data_binary: vec![0x1a, 0x04, 0x08, 0x01, 0x1a, 0x00],
        };
        let error = decode(&request).err().expect("must require request_id");
        assert!(error
            .to_string()
            .contains("BidiAppend request_id is required"));
    }

    #[test]
    fn decode_accepts_cursor_331_wire_run_request_with_non_empty_file_states() {
        use std::collections::HashMap;

        let mut file_states = HashMap::new();
        file_states.insert("/workspace/src/main.rs".to_string(), b"blob-ref-1".to_vec());
        let mut file_states_v2 = HashMap::new();
        file_states_v2.insert(
            "/workspace/src/main.rs".to_string(),
            agent::FileStateStructure {
                content: Some(b"content-hash".to_vec()),
                initial_content: Some(b"initial-hash".to_vec()),
            },
        );

        let state = agent::ConversationStateStructure {
            root_prompt_messages_json: vec![b"{\"role\":\"system\"}".to_vec()],
            file_states,
            file_states_v2,
            self_summary_count: 1,
            read_paths: vec!["/workspace/README.md".into()],
            ..Default::default()
        };
        // Append Cursor 3.31.3 fields (tags 34, 37, 55) that are not in agent_v1.proto
        // to ensure unknown trailing fields alongside non-empty file_states decode cleanly.
        let mut state_bytes = state.encode_to_vec();
        prost::encoding::string::encode(34, &"ask-1".to_string(), &mut state_bytes);
        prost::encoding::uint32::encode(37, &2, &mut state_bytes);
        prost::encoding::bool::encode(55, &false, &mut state_bytes);

        let mut run_request_bytes = Vec::new();
        prost::encoding::bytes::encode(1, &state_bytes, &mut run_request_bytes);
        let action_bytes = agent::ConversationAction {
            action: Some(agent::conversation_action::Action::UserMessageAction(
                agent::UserMessageAction {
                    user_message: Some(agent::UserMessage {
                        text: "hi".into(),
                        message_id: "msg-331".into(),
                        mode: agent::AgentMode::Agent as i32,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )),
            ..Default::default()
        }
        .encode_to_vec();
        prost::encoding::bytes::encode(2, &action_bytes, &mut run_request_bytes);
        let requested_model_bytes = agent::RequestedModel {
            model_id: "83f1333d63840a0c".into(),
            ..Default::default()
        }
        .encode_to_vec();
        prost::encoding::bytes::encode(9, &requested_model_bytes, &mut run_request_bytes);
        prost::encoding::string::encode(5, &"conv-331".to_string(), &mut run_request_bytes);
        // Cursor 3.31.3 AgentRunRequest fields (tags 28, 37)
        prost::encoding::bool::encode(28, &true, &mut run_request_bytes);
        prost::encoding::bool::encode(37, &true, &mut run_request_bytes);

        let mut client_msg_bytes = Vec::new();
        prost::encoding::bytes::encode(1, &run_request_bytes, &mut client_msg_bytes);

        let wire = ai::BidiAppendRequest {
            data: String::new(),
            request_id: Some(ai::BidiRequestId {
                request_id: "req-331".into(),
            }),
            append_seqno: 0,
            data_binary: client_msg_bytes,
        }
        .encode_to_vec();

        let raw_request = ai::BidiAppendRequest::decode(wire.as_slice()).unwrap();
        let decoded = decode(&raw_request).unwrap();
        assert_eq!(decoded.request_id, "req-331");
        assert_eq!(decoded.conversation_id(), Some("conv-331"));
        assert_eq!(decoded.model_id(), Some("83f1333d63840a0c"));
        let Some(agent::agent_client_message::Message::RunRequest(run_req)) =
            decoded.message.message
        else {
            panic!("expected RunRequest");
        };
        let decoded_state = run_req.conversation_state.expect("conversation_state");
        assert_eq!(
            decoded_state
                .file_states
                .get("/workspace/src/main.rs")
                .map(Vec::as_slice),
            Some(b"blob-ref-1".as_slice())
        );
        assert_eq!(
            decoded_state
                .file_states_v2
                .get("/workspace/src/main.rs")
                .and_then(|fs| fs.content.as_deref()),
            Some(b"content-hash".as_slice())
        );
    }
}
