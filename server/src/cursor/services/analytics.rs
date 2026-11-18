//! Implements Cursor analytics endpoints and event handling.
use axum::{
    body::{Body, Bytes},
    extract::Extension,
    http::{header, HeaderValue, Request, Response, StatusCode},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use bytes::{BufMut, BytesMut};
use prost::Message;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::{api::cursor::proxy, Error, Result};

pub const BOOTSTRAP_STATSIG_PATH: &str = "/aiserver.v1.AnalyticsService/BootstrapStatsig";
const AGENT_RETRIES_GATE: &str = "nal_agent_retries";
const AGENT_HOST_LOCAL_LOOP_GATE: &str = "agent_host_local_loop";
const NAL_WEBSOCKET_CLIENT_GATE: &str = "nal_websocket_client";
const LOCAL_ENABLED_RULE: &str = "local_enabled";
const LOCAL_DISABLED_RULE: &str = "local_disabled";

#[derive(Clone, PartialEq, Message)]
struct BootstrapStatsigResponse {
    #[prost(string, tag = "1")]
    config: String,
    #[prost(uint64, tag = "2")]
    generated_at_ms: u64,
}

pub async fn bootstrap_statsig(
    Extension(upstream): Extension<proxy::CursorProxy>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    match proxy::forward_buffered(&upstream, request).await {
        Ok(response) if response.status.is_success() => match patch_upstream(response) {
            Ok(response) => Ok(response),
            Err(error) => {
                tracing::warn!(%error, "Cursor Statsig bootstrap was invalid; using local bootstrap");
                local_response()
            }
        },
        Ok(response) => {
            tracing::warn!(status = %response.status, "Cursor Statsig bootstrap was rejected; using local bootstrap");
            local_response()
        }
        Err(error) => {
            tracing::warn!(%error, "Cursor Statsig bootstrap was unavailable; using local bootstrap");
            local_response()
        }
    }
}

fn patch_upstream(response: proxy::BufferedResponse) -> Result<Response<Body>> {
    let (framed, payload) = unary_payload(&response.body)?;
    let mut message = BootstrapStatsigResponse::decode(payload)?;
    let mut config = serde_json::from_str::<Value>(&message.config)?;
    let hash_used = config
        .get("hash_used")
        .and_then(Value::as_str)
        .unwrap_or("none")
        .to_owned();
    apply_local_gates(&mut config)?;
    message.config = serde_json::to_string(&config)?;
    tracing::info!(
        hash_used,
        agent_host_local_loop = false,
        nal_websocket_client = false,
        nal_agent_retries = true,
        "patched Cursor BootstrapStatsig feature gates"
    );
    Ok(response.with_body(encode_unary(&message, framed)))
}

fn local_response() -> Result<Response<Body>> {
    let generated_at_ms = chrono::Utc::now().timestamp_millis() as u64;
    let mut config = json!({
        "feature_gates": {},
        "dynamic_configs": {},
        "layer_configs": {},
        "user": {
            "userID": "local_ultra",
            "customIDs": { "localUserID": "local_ultra" }
        },
        "has_updates": true,
        "hash_used": "none",
        "sdkParams": {
            "stableID": "local_ultra",
            "disableDiagnosticsLogging": true
        },
        "time": generated_at_ms
    });
    apply_local_gates(&mut config)?;
    tracing::info!(
        hash_used = "none",
        agent_host_local_loop = false,
        nal_websocket_client = false,
        nal_agent_retries = true,
        "serving local Cursor BootstrapStatsig feature gates"
    );
    let message = BootstrapStatsigResponse {
        config: serde_json::to_string(&config)?,
        generated_at_ms,
    };
    let body = message.encode_to_vec();
    let mut response = Response::new(Body::from(body.clone()));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/proto"),
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        body.len()
            .to_string()
            .parse()
            .expect("body length is a valid header value"),
    );
    Ok(response)
}

fn apply_local_gates(config: &mut Value) -> Result<()> {
    let retries_key = statsig_key(config, AGENT_RETRIES_GATE);
    let local_loop_key = statsig_key(config, AGENT_HOST_LOCAL_LOOP_GATE);
    let websocket_key = statsig_key(config, NAL_WEBSOCKET_CLIENT_GATE);
    let root = config
        .as_object_mut()
        .ok_or_else(|| Error::Protocol("Statsig bootstrap config must be an object".into()))?;
    let gates = root
        .entry("feature_gates")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| Error::Protocol("Statsig feature_gates must be an object".into()))?;
    gates.insert(retries_key.clone(), gate_entry(&retries_key, true));
    // Cursor 3.31+ switches `cursor-agent-host` onto `InferenceService/RunInference`
    // (`RunInferenceClientMessage` over `BidiAppend`) when `agent_host_local_loop` is true,
    // and onto `wss://api2.cursor.sh/agent/v1/run` when `nal_websocket_client` is true.
    // Force both off so all Agent runs stay on HTTP/1.1 `AgentService/RunSSE` + `BidiAppend`.
    gates.insert(local_loop_key.clone(), gate_entry(&local_loop_key, false));
    gates.insert(websocket_key.clone(), gate_entry(&websocket_key, false));
    Ok(())
}

fn statsig_key(config: &Value, name: &str) -> String {
    match config.get("hash_used").and_then(Value::as_str) {
        Some("djb2") => djb2(name),
        Some("sha256") => STANDARD.encode(Sha256::digest(name.as_bytes())),
        _ => name.to_owned(),
    }
}

fn djb2(value: &str) -> String {
    value
        .encode_utf16()
        .fold(0_u32, |hash, character| {
            hash.wrapping_mul(31).wrapping_add(u32::from(character))
        })
        .to_string()
}

fn gate_entry(name: &str, value: bool) -> Value {
    let rule = if value {
        LOCAL_ENABLED_RULE
    } else {
        LOCAL_DISABLED_RULE
    };
    json!({
        "name": name,
        "value": value,
        "rule_id": rule,
        "ruleID": rule,
        "group_name": rule,
        "groupName": rule,
        "secondary_exposures": [],
        "secondaryExposures": [],
        "undelegated_secondary_exposures": [],
        "undelegatedSecondaryExposures": [],
        "is_device_based": false,
        "isDeviceBased": false,
        "id_type": "userID",
        "idType": "userID"
    })
}

fn unary_payload(body: &Bytes) -> Result<(bool, &[u8])> {
    if body.len() < 5 {
        return Ok((false, body));
    }
    let flags = body[0];
    let length = u32::from_be_bytes([body[1], body[2], body[3], body[4]]) as usize;
    if length != body.len() - 5 {
        return Ok((false, body));
    }
    if flags != 0 {
        return Err(Error::Protocol(format!(
            "cannot patch compressed or terminal Statsig frame: flags={flags}"
        )));
    }
    Ok((true, &body[5..]))
}

fn encode_unary(message: &impl Message, framed: bool) -> Bytes {
    let payload = message.encode_to_vec();
    if !framed {
        return Bytes::from(payload);
    }
    let mut output = BytesMut::with_capacity(5 + payload.len());
    output.put_u8(0);
    output.put_u32(payload.len() as u32);
    output.extend_from_slice(&payload);
    output.freeze()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn djb2_overrides_upstream_local_loop_and_websocket_gates_for_cursor_331() {
        let mut config = json!({
            "hash_used": "djb2",
            "feature_gates": {
                "1967931573": {
                    "name": "1967931573",
                    "value": true,
                    "rule_id": "74FdOxogOo75aIlcPjrAA8:100.00:6"
                },
                "579159465": {
                    "name": "579159465",
                    "value": true,
                    "rule_id": "7IcxuoEaMtArwA8jq1Zr9H:100.00:14"
                }
            }
        });
        apply_local_gates(&mut config).unwrap();

        let gates = &config["feature_gates"];
        assert_eq!(djb2(AGENT_RETRIES_GATE), "101914982");
        assert_eq!(djb2(AGENT_HOST_LOCAL_LOOP_GATE), "1967931573");
        assert_eq!(djb2(NAL_WEBSOCKET_CLIENT_GATE), "579159465");
        assert_eq!(gates["101914982"]["value"], true);
        assert_eq!(gates["1967931573"]["value"], false);
        assert_eq!(gates["579159465"]["value"], false);
    }

    #[test]
    fn plain_and_sha256_configs_force_nal_http_transport_gates() {
        for hash_used in ["none", "sha256"] {
            let mut config = json!({
                "hash_used": hash_used,
                "feature_gates": {}
            });
            apply_local_gates(&mut config).unwrap();
            let retries = statsig_key(&config, AGENT_RETRIES_GATE);
            let local_loop = statsig_key(&config, AGENT_HOST_LOCAL_LOOP_GATE);
            let websocket = statsig_key(&config, NAL_WEBSOCKET_CLIENT_GATE);
            assert_eq!(config["feature_gates"][&retries]["value"], true);
            assert_eq!(config["feature_gates"][&local_loop]["value"], false);
            assert_eq!(config["feature_gates"][&websocket]["value"], false);
        }
    }
}
