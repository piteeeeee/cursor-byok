//! Verifies captured Cursor Connect framing and protobuf compatibility.
#[path = "support/fake_cursor.rs"]
mod fake_cursor;
#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/fixtures.rs"]
mod fixtures;

use std::{io::Write, sync::Arc};

use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
};
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine};
use cursor_server::{
    api::cursor,
    cursor::prompting::{PromptAssets, PromptCompiler},
    cursor::protocol::{
        connect,
        proto::{agent::v1 as pb, aiserver::v1 as ai},
    },
    cursor::transport::TransportRegistry,
    network::NetworkClients,
};
use flate2::{write::GzEncoder, Compression};
use prost::Message;
use tower::ServiceExt;

#[test]
fn connect_envelope_is_flag_plus_big_endian_length_plus_protobuf() {
    let message = pb::BidiRequestId {
        request_id: "abc".into(),
    };
    let frame = connect::encode_message(&message).unwrap();
    assert_eq!(&frame[..5], &[0, 0, 0, 0, 5]);
    let decoded: pb::BidiRequestId = fake_cursor::decode_single(&frame).unwrap();
    assert_eq!(decoded.request_id, "abc");
}

#[test]
fn end_stream_matches_captured_connect_shape() {
    assert_eq!(
        connect::encode_end_stream().as_ref(),
        &[2, 0, 0, 0, 2, b'{', b'}']
    );
}

#[test]
fn error_end_stream_is_flagged_json_not_protobuf() {
    let frame = connect::encode_error_end_stream(&connect::ConnectStreamError {
        code: connect::ConnectCode::Unavailable,
        message: "overloaded".into(),
        details: vec![connect::ConnectErrorDetail {
            type_name: "aiserver.v1.ErrorDetails".into(),
            value: "AQ".into(),
        }],
    })
    .unwrap();
    let (flags, payload) = connect::decode_frames(&frame).unwrap().pop().unwrap();
    assert_eq!(flags, connect::END_STREAM_FLAG);
    let json: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(json["error"]["code"], "unavailable");
    assert_eq!(json["error"]["message"], "overloaded");
    assert_eq!(
        json["error"]["details"][0]["type"],
        "aiserver.v1.ErrorDetails"
    );
}

#[test]
fn cursor_error_details_subset_decodes_captured_wire_value() {
    let captured = "CAISVQoUQXV0aGVudGljYXRpb24gZXJyb3ISMklmIHlvdSBhcmUgbG9nZ2VkIGluLCB0cnkgbG9nZ2luZyBvdXQgYW5kIGJhY2sgaW4uIABSBwoFbG9naW4YAQ";
    let bytes = STANDARD_NO_PAD.decode(captured).unwrap();
    let details = ai::ErrorDetails::decode(bytes.as_slice()).unwrap();
    assert_eq!(details.error, 2, "ERROR_NOT_LOGGED_IN");
    assert_eq!(details.is_expected, Some(true));
    let custom = details.details.unwrap();
    assert_eq!(custom.title, "Authentication error");
    assert_eq!(custom.is_retryable, Some(false));
}

#[test]
fn captured_kv_ack_hex_decodes_as_agent_client_message() {
    let bytes = hex::decode("1a0408011a00").unwrap();
    let message = pb::AgentClientMessage::decode(bytes.as_slice()).unwrap();
    let Some(pb::agent_client_message::Message::KvClientMessage(kv)) = message.message else {
        panic!("expected KV client message")
    };
    assert_eq!(kv.id, 1);
    assert!(matches!(
        kv.message,
        Some(pb::kv_client_message::Message::SetBlobResult(_))
    ));
}

#[tokio::test]
async fn bidi_append_gzip_body_is_decompressed_before_protobuf_decode() {
    let (_directory, store) = fixtures::temp_store().await;
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let clients = NetworkClients::new(None);
    let registry = TransportRegistry::new(
        store,
        fixtures::empty_runtime(),
        Arc::new(fake_provider::FakeProvider::default()),
        PromptCompiler::new(assets),
    );
    let wire = ai::BidiAppendRequest {
        request_id: Some(ai::BidiRequestId {
            request_id: "gzip-request".into(),
        }),
        ..Default::default()
    }
    .encode_to_vec();
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&wire).unwrap();
    let compressed = encoder.finish().unwrap();

    let response = cursor::router(registry, clients)
        .unwrap()
        .oneshot(
            Request::post("/aiserver.v1.BidiService/BidiAppend")
                .header(header::CONTENT_TYPE, "application/proto")
                .header(header::CONTENT_ENCODING, "gzip")
                .body(Body::from(compressed))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let text = std::str::from_utf8(&body).unwrap();
    assert!(text.contains("BidiAppend contains no AgentClientMessage"));
    assert!(!text.contains("protobuf decode error"));
}

#[tokio::test]
async fn bidi_append_rejects_simultaneous_data_and_data_binary_with_bad_request() {
    let (_directory, store) = fixtures::temp_store().await;
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let clients = NetworkClients::new(None);
    let registry = TransportRegistry::new(
        store,
        fixtures::empty_runtime(),
        Arc::new(fake_provider::FakeProvider::default()),
        PromptCompiler::new(assets),
    );
    let payload = [0x1a, 0x04, 0x08, 0x01, 0x1a, 0x00];
    let wire = ai::BidiAppendRequest {
        data: hex::encode(payload),
        request_id: Some(ai::BidiRequestId {
            request_id: "dual-payload".into(),
        }),
        append_seqno: 0,
        data_binary: payload.to_vec(),
    }
    .encode_to_vec();

    let response = cursor::router(registry, clients)
        .unwrap()
        .oneshot(
            Request::post("/aiserver.v1.BidiService/BidiAppend")
                .header(header::CONTENT_TYPE, "application/proto")
                .body(Body::from(wire))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let text = std::str::from_utf8(&body).unwrap();
    assert!(text.contains("BidiAppend cannot set both data and data_binary"));
}

#[tokio::test]
async fn bidi_append_data_binary_routes_to_local_provider_and_streams_run_sse() {
    use cursor_server::{
        model::{ModelConfigInput, ModelType, OPENAI_CHAT_ENDPOINT},
        provider::{FinishReason, ModelEvent},
    };
    use futures_util::StreamExt;

    let (_directory, store) = fixtures::temp_store().await;
    let saved_model = fixtures::model(ModelConfigInput {
        sort_order: 1,
        display_name: "deepseek-v4.1-flash".into(),
        group_name: None,
        model_type: ModelType::OpenAi,
        base_url: "https://example.com/v1".into(),
        use_full_url: false,
        api_key: "test-key".into(),
        tooltip_data: "test".into(),
        model_id: "deepseek-v4.1-flash".into(),
        reasoning_effort: None,
        openai_endpoint: OPENAI_CHAT_ENDPOINT.into(),
        openai_extra_params_enabled: false,
        openai_extra_params: serde_json::json!({}),
        custom_headers_enabled: false,
        custom_headers: serde_json::json!({}),
        anthropic_extra_params_enabled: false,
        anthropic_extra_params: serde_json::json!({}),
        context_window_tokens: None,
        max_completion_tokens: None,
        anthropic_max_tokens: None,
        anthropic_thinking_effort: None,
        thinking_budget_tokens: None,
    });

    let provider = fake_provider::FakeProvider::default();
    provider.push(vec![
        ModelEvent::Start {
            model_call_id: "call-1".into(),
        },
        ModelEvent::TextStart,
        ModelEvent::TextDelta("pong from byok".into()),
        ModelEvent::TextEnd,
        ModelEvent::Done(FinishReason::Stop),
    ]);
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let clients = NetworkClients::new(None);
    let registry = TransportRegistry::new(
        store,
        fixtures::runtime(vec![saved_model.clone()]),
        Arc::new(provider),
        PromptCompiler::new(assets),
    );
    let app = cursor::router(registry, clients).unwrap();

    let request_id = "req-data-binary-e2e";
    let run_sse_app = app.clone();
    let sse_task = tokio::spawn(async move {
        let req_frame = connect::encode_message(&pb::BidiRequestId {
            request_id: request_id.into(),
        })
        .unwrap();
        run_sse_app
            .oneshot(
                Request::post("/agent.v1.AgentService/RunSSE")
                    .header(header::CONTENT_TYPE, "application/connect+proto")
                    .body(Body::from(req_frame))
                    .unwrap(),
            )
            .await
            .unwrap()
    });

    let run_client_msg = pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::RunRequest(
            pb::AgentRunRequest {
                conversation_id: Some("conv-data-binary-e2e".into()),
                run_id: Some(request_id.into()),
                requested_model: Some(pb::RequestedModel {
                    model_id: saved_model.model_hash.clone(),
                    ..Default::default()
                }),
                action: Some(pb::ConversationAction {
                    action: Some(pb::conversation_action::Action::UserMessageAction(
                        pb::UserMessageAction {
                            user_message: Some(pb::UserMessage {
                                text: "hello".into(),
                                message_id: "user-1".into(),
                                mode: pb::AgentMode::Agent as i32,
                                ..Default::default()
                            }),
                            request_context: Some(pb::RequestContext::default()),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )),
    };
    let append_wire = connect::encode_message(&ai::BidiAppendRequest {
        data: String::new(),
        request_id: Some(ai::BidiRequestId {
            request_id: request_id.into(),
        }),
        append_seqno: 0,
        data_binary: run_client_msg.encode_to_vec(),
    })
    .unwrap();

    let append_resp = app
        .clone()
        .oneshot(
            Request::post("/aiserver.v1.BidiService/BidiAppend")
                .header(header::CONTENT_TYPE, "application/connect+proto")
                .body(Body::from(append_wire))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(append_resp.status(), StatusCode::OK);

    let sse_resp = sse_task.await.unwrap();
    assert_eq!(sse_resp.status(), StatusCode::OK);
    let mut stream = sse_resp.into_body().into_data_stream();
    let mut next_seqno = 1_i64;
    let mut collected_text = String::new();
    let mut saw_end_stream = false;

    while let Some(chunk) = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
        .await
        .expect("RunSSE stream timed out")
    {
        let chunk = chunk.unwrap();
        for (flags, payload) in connect::decode_frames(&chunk).unwrap() {
            if flags & connect::END_STREAM_FLAG != 0 {
                assert_eq!(payload.as_ref(), b"{}");
                saw_end_stream = true;
                break;
            }
            let server_msg = pb::AgentServerMessage::decode(payload).unwrap();
            match server_msg.message {
                Some(pb::agent_server_message::Message::KvServerMessage(kv)) => {
                    let ack = pb::AgentClientMessage {
                        message: Some(pb::agent_client_message::Message::KvClientMessage(
                            pb::KvClientMessage {
                                id: kv.id,
                                message: Some(pb::kv_client_message::Message::SetBlobResult(
                                    pb::SetBlobResult { error: None },
                                )),
                            },
                        )),
                    };
                    let ack_wire = connect::encode_message(&ai::BidiAppendRequest {
                        data: String::new(),
                        request_id: Some(ai::BidiRequestId {
                            request_id: request_id.into(),
                        }),
                        append_seqno: next_seqno,
                        data_binary: ack.encode_to_vec(),
                    })
                    .unwrap();
                    next_seqno += 1;
                    let ack_resp = app
                        .clone()
                        .oneshot(
                            Request::post("/aiserver.v1.BidiService/BidiAppend")
                                .header(header::CONTENT_TYPE, "application/connect+proto")
                                .body(Body::from(ack_wire))
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    assert_eq!(ack_resp.status(), StatusCode::OK);
                }
                Some(pb::agent_server_message::Message::InteractionUpdate(update)) => {
                    if let Some(pb::interaction_update::Message::TextDelta(delta)) = update.message
                    {
                        collected_text.push_str(&delta.text);
                    }
                }
                _ => {}
            }
        }
        if saw_end_stream {
            break;
        }
    }

    assert_eq!(collected_text, "pong from byok");
    assert!(saw_end_stream);
}

#[tokio::test]
async fn bidi_append_declines_captured_cursor_331_run_inference_payload_with_permission_denied() {
    let (_directory, store) = fixtures::temp_store().await;
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let clients = NetworkClients::new(None);
    let registry = TransportRegistry::new(
        store,
        fixtures::empty_runtime(),
        Arc::new(fake_provider::FakeProvider::default()),
        PromptCompiler::new(assets),
    );
    let payload =
        hex::decode(include_str!("fixtures/cursor_331_run_inference_client_message.hex").trim())
            .unwrap();
    let wire = ai::BidiAppendRequest {
        data: String::new(),
        request_id: Some(ai::BidiRequestId {
            request_id: "run-inference-append".into(),
        }),
        append_seqno: 0,
        data_binary: payload,
    }
    .encode_to_vec();

    let response = cursor::router(registry, clients)
        .unwrap()
        .oneshot(
            Request::post("/aiserver.v1.BidiService/BidiAppend")
                .header(header::CONTENT_TYPE, "application/proto")
                .body(Body::from(wire))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["code"], "permission_denied");
    assert_eq!(
        json["message"],
        "InferenceService.RunInference is not enabled for this account"
    );
}

#[tokio::test]
async fn run_inference_sse_and_ws_upgrade_return_fallback_responses() {
    let (_directory, store) = fixtures::temp_store().await;
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let clients = NetworkClients::new(None);
    let registry = TransportRegistry::new(
        store,
        fixtures::empty_runtime(),
        Arc::new(fake_provider::FakeProvider::default()),
        PromptCompiler::new(assets),
    );
    let app = cursor::router(registry, clients).unwrap();

    let req_frame = connect::encode_message(&pb::BidiRequestId {
        request_id: "run-inference-sse".into(),
    })
    .unwrap();
    let sse_resp = app
        .clone()
        .oneshot(
            Request::post("/aiserver.v1.InferenceService/RunInferenceSSE")
                .header(header::CONTENT_TYPE, "application/connect+proto")
                .body(Body::from(req_frame))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(sse_resp.status(), StatusCode::OK);
    let body = to_bytes(sse_resp.into_body(), 4096).await.unwrap();
    let frames = connect::decode_frames(&body).unwrap();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].0, connect::END_STREAM_FLAG);
    let json: serde_json::Value = serde_json::from_slice(&frames[0].1).unwrap();
    assert_eq!(json["error"]["code"], "permission_denied");
    assert_eq!(
        json["error"]["message"],
        "InferenceService.RunInference is not enabled for this account"
    );

    let ws_resp = app
        .oneshot(Request::get("/agent/v1/run").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(ws_resp.status(), StatusCode::UPGRADE_REQUIRED);
}
