//! Error shaping and privacy-safe logging helpers.
//!
//! Every upstream failure is normalized to the Anthropic error shape; the log
//! keeps only counts and block kinds, never prompt content.

use crate::domain::CatalogEntry;
use axum::{
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::Value;

pub(crate) fn anthropic_error(status: StatusCode, err_type: &str, msg: &str) -> Response {
    let body = serde_json::json!({"type": "error", "error": {"type": err_type, "message": msg}});
    (status, Json(body)).into_response()
}

/// OpenAI-style error body for the `/v1/responses` edge (Codex): no
/// top-level `type`, everything lives under `error`. `code` is optional —
/// Codex reads it when present but only requires `message`/`type`.
pub(crate) fn openai_error(status: StatusCode, err_type: &str, msg: &str) -> Response {
    openai_error_code(status, err_type, msg, None)
}

/// Same as [`openai_error`] with an explicit machine-readable `code`
/// (e.g. `not_implemented` on the Fase 1 protocol gate).
pub(crate) fn openai_error_code(
    status: StatusCode,
    err_type: &str,
    msg: &str,
    code: Option<&str>,
) -> Response {
    let mut error = serde_json::json!({"message": msg, "type": err_type});
    if let Some(c) = code {
        error["code"] = Value::String(c.to_string());
    }
    (status, Json(serde_json::json!({ "error": error }))).into_response()
}

/// Extract the human message from an upstream error body, whatever shape it
/// came in (`{"error":{"message"}}`, `{"message"}` or a raw preview).
fn upstream_error_message(text: &str) -> String {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|e| e.get("message"))
                .or_else(|| value.get("message"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| body_preview(text))
}

/// Status → OpenAI `type` mapping for the `/v1/responses` edge (mirror of
/// the Anthropic mapping in [`upstream_error_response`]).
fn openai_error_type(status: StatusCode) -> &'static str {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => "authentication_error",
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => "invalid_request_error",
        StatusCode::NOT_FOUND => "not_found_error",
        StatusCode::TOO_MANY_REQUESTS => "rate_limit_error",
        _ => "server_error",
    }
}

/// Normalize a non-2xx upstream body to the OpenAI error shape, keeping the
/// upstream `error.code` when it carried one (mirrors
/// [`upstream_error_response`] for the Codex edge).
pub(crate) fn openai_error_response(status: StatusCode, text: &str) -> Response {
    let message = upstream_error_message(text);
    let upstream_code = serde_json::from_str::<Value>(text).ok().and_then(|value| {
        value
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(|c| c.as_str())
            .map(str::to_owned)
    });
    openai_error_code(
        status,
        openai_error_type(status),
        &message,
        upstream_code.as_deref(),
    )
}

pub(crate) fn upstream_error_response(status: StatusCode, text: &str) -> Response {
    let message = upstream_error_message(text);
    let error_type = match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => "authentication_error",
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => "invalid_request_error",
        StatusCode::NOT_FOUND => "not_found_error",
        StatusCode::TOO_MANY_REQUESTS => "rate_limit_error",
        _ => "api_error",
    };
    anthropic_error(status, error_type, &message)
}

/// Truncate an upstream error body for logs (never log credentials here;
///
/// callers only pass status + body, never the bearer).
/// Message from a Responses `response.failed` event: the upstream reports the
/// failure under `response.error` (falling back to a few legacy shapes).
pub(crate) fn response_failure_message(v: &Value) -> String {
    let r = v.get("response").unwrap_or(v);
    let picked = r
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| r.get("error").and_then(Value::as_str).map(str::to_owned))
        .or_else(|| {
            r.get("incomplete_details")
                .and_then(|d| d.get("reason"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    match picked {
        Some(s) if !s.is_empty() => format!("upstream response failed: {s}"),
        _ => "upstream response failed".to_string(),
    }
}

pub(crate) fn body_preview(s: &str) -> String {
    const MAX: usize = 500;
    let t = s.trim();
    if t.len() <= MAX {
        return t.to_string();
    }
    let end = t
        .char_indices()
        .take_while(|(index, _)| *index < MAX)
        .map(|(index, _)| index)
        .last()
        .unwrap_or(0);
    let mut out = t[..end].to_string();
    out.push('…');
    out
}

pub(crate) fn log_upstream_error(
    entry: &CatalogEntry,
    gateway_model: &str,
    base: &str,
    status: StatusCode,
    text: &str,
    req_summary: &serde_json::Value,
    headers: &HeaderMap,
) {
    // Client identification (no prompt content): which app/CLI sent the
    // request and from which session — enough to attribute background flows
    // (title-gen, classifier, probes) that pick a model on their own.
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-")
            .to_string()
    };
    tracing::warn!(
        gateway_model = %gateway_model,
        opencode_ref = %entry.qualified(),
        entry_id = %entry.id,
        provider = %entry.provider_id,
        base_url = %base,
        status = status.as_u16(),
        body = %body_preview(text),
        user_agent = %header("user-agent"),
        session = %header("x-claude-code-session-id"),
        req = %req_summary,
        "upstream rejected request"
    );
}

/// Privacy-safe shape of the Anthropic request that failed: counts, block
/// kinds and sizes only, never prompt/tool content. Lets us tell "poisoned
/// history in this session" (e.g. a tool_result shape the translator or the
/// upstream rejects) apart from "model down" without logging user data.
pub(crate) fn request_summary(body: &Value) -> Value {
    let mut msgs = vec![];
    if let Some(arr) = body.get("messages").and_then(|m| m.as_array()) {
        for m in arr.iter().take(50) {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("?");
            match m.get("content") {
                Some(Value::String(s)) => msgs.push(serde_json::json!({
                    "role": role, "kind": "text", "chars": s.chars().count()
                })),
                Some(Value::Array(blocks)) => {
                    let kinds: Vec<String> = blocks
                        .iter()
                        .map(|b| {
                            let t = b.get("type").and_then(|t| t.as_str()).unwrap_or("?");
                            // Text length without content: tool I/O still needs a size hint.
                            let chars = b
                                .get("text")
                                .and_then(|t| t.as_str())
                                .map(|s| s.chars().count())
                                .unwrap_or(0);
                            if chars > 0 {
                                format!("{t}:{chars}ch")
                            } else {
                                t.to_string()
                            }
                        })
                        .collect();
                    msgs.push(serde_json::json!({"role": role, "kind": kinds}));
                }
                _ => msgs.push(serde_json::json!({"role": role, "kind": "other"})),
            }
        }
    }
    let tools: Vec<String> = body
        .get("tools")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .take(30)
                .map(|t| {
                    t.get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("?")
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default();
    serde_json::json!({
        "messages": msgs.len(),
        "detail": msgs,
        "tools": tools,
        "tool_choice": body.get("tool_choice").and_then(|t| t.get("type").and_then(|x| x.as_str())),
        "max_tokens": body.get("max_tokens"),
        "stream": body.get("stream"),
        "system": body.get("system").map(|s| if s.is_string() { "text" } else { "blocks" }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn request_summary_counts_without_content() {
        let body = json!({
            "model": "claude-x",
            "max_tokens": 128,
            "stream": true,
            "system": "secret-system",
            "messages": [
                {"role": "user", "content": "secret-prompt"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "secret-reply"},
                    {"type": "tool_use", "id": "t1", "name": "Read", "input": {"path": "secret-path"}},
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "secret-file-contents"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAA"}},
                ]},
            ],
            "tools": [{"name": "Read", "description": "d", "input_schema": {"type": "object"}}],
            "tool_choice": {"type": "auto"},
        });
        let s = request_summary(&body);
        let rendered = s.to_string();
        for secret in [
            "secret-prompt",
            "secret-reply",
            "secret-path",
            "secret-file-contents",
            "secret-system",
            "AAA",
        ] {
            assert!(!rendered.contains(secret), "{rendered}");
        }
        assert_eq!(s["messages"], 3);
        assert_eq!(s["tools"], json!(["Read"]));
        assert_eq!(s["max_tokens"], 128);
    }

    #[test]
    fn body_preview_truncates_at_utf8_boundary() {
        let long = format!("{}x", "é".repeat(300));
        let preview = body_preview(&long);
        assert!(preview.ends_with('…'));
        assert!(std::str::from_utf8(preview.as_bytes()).is_ok());
    }
    #[test]
    fn body_preview_truncates() {
        assert_eq!(body_preview("  ok  "), "ok");
        let long = "x".repeat(600);
        let p = body_preview(&long);
        assert!(p.len() < 600 && p.ends_with('…'), "{p}");
    }

    // -- /v1/responses (Codex) edge: OpenAI error shape --------------------

    #[tokio::test]
    async fn openai_error_shape_has_no_top_level_type() {
        let resp = openai_error(StatusCode::NOT_FOUND, "not_found_error", "nope");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v.get("type").is_none(), "{v}");
        assert_eq!(v["error"]["type"], "not_found_error");
        assert_eq!(v["error"]["message"], "nope");
        assert!(v["error"].get("code").is_none());
    }

    #[tokio::test]
    async fn openai_error_response_keeps_upstream_code() {
        let resp = openai_error_response(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"message":"slow down","type":"rate_limit","code":"quota_exceeded"}}"#,
        );
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["type"], "rate_limit_error");
        assert_eq!(v["error"]["message"], "slow down");
        assert_eq!(v["error"]["code"], "quota_exceeded");
    }

    #[tokio::test]
    async fn openai_error_response_falls_back_to_preview_and_server_error() {
        let resp = openai_error_response(StatusCode::BAD_GATEWAY, "not json at all");
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["type"], "server_error");
        assert_eq!(v["error"]["message"], "not json at all");
        assert!(v["error"].get("code").is_none());
    }
}
