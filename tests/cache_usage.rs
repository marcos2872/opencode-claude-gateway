//! E2E do relato de cache de prefixo: contadores de cache do upstream
//! (`prompt_cache_hit_tokens` do DeepSeek, `prompt_tokens_details.cached_tokens`,
//! `input_tokens_details.cached_tokens`) chegam ao cliente como
//! `usage.cache_read_input_tokens` nos caminhos traduzidos, e o verbatim
//! Anthropic continua intacto. Upstream mockado: HTTP real em 127.0.0.1, sem
//! binário `opencode` e sem rede.
//!
//! Arquivo autocontido de propósito: os testes de compatibilidade Claude em
//! `tests/gateway.rs` e os da edge Codex em `tests/codex.rs` não são tocados
//! por esta feature. Os helpers abaixo espelham os de `codex.rs`.

use axum::{
    extract::State,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use opencode_claude_gateway::api::server::{router, AppState};
use opencode_claude_gateway::config::AppConfig;
use opencode_claude_gateway::domain::{AliasEntry, CatalogEntry, CatalogSettings};
use opencode_claude_gateway::infra::upstream::{
    anthropic_to_openai, anthropic_to_responses, openai_to_anthropic, responses_to_anthropic,
    StreamTranslator,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

const TOKEN: &str = "test-secret";
const CHAT_PKG: &str = "@opencode/ai/providers/openai-compatible";
const RESPONSES_PKG: &str = "@opencode/ai/providers/openai";
const ANTHROPIC_PKG: &str = "@opencode/ai/providers/anthropic";

fn test_config() -> AppConfig {
    AppConfig {
        auth_token: TOKEN.to_string(),
        ..AppConfig::default()
    }
}

fn alias(gateway: &str, opencode_ref: &str) -> AliasEntry {
    AliasEntry {
        gateway_id: gateway.to_string(),
        opencode_ref: opencode_ref.to_string(),
        display_name: gateway.to_string(),
        description: "test".to_string(),
        context_window: None,
        family_tier: None,
        family_default: false,
    }
}

/// Catalog entry pointing at the mock upstream. Provider `opencode` with an
/// inline api_key needs no credential DB, so the forward path is exercised.
fn mock_entry(base_url: &str, package: &str, model: &str) -> CatalogEntry {
    CatalogEntry {
        id: model.to_string(),
        model_id: model.to_string(),
        provider_id: "opencode".to_string(),
        name: model.to_string(),
        package: package.to_string(),
        settings: CatalogSettings {
            base_url: Some(base_url.to_string()),
            api_key: Some("test-upstream-key".to_string()),
            provider: None,
            endpoint: None,
        },
        limit: None,
        enabled: true,
        variants: vec![],
        headers: None,
        body: None,
    }
}

async fn seeded_state(
    config: AppConfig,
    entries: Vec<CatalogEntry>,
    aliases: Vec<AliasEntry>,
) -> AppState {
    let state = AppState::new(config, PathBuf::from("/nonexistent-ocg-test/opencode.db"));
    *state.catalog.write().await = entries;
    *state.aliases.write().await = aliases;
    state
}

fn msg_body(model: &str) -> Value {
    json!({
        "model": model,
        "max_tokens": 8,
        "messages": [{"role": "user", "content": "hi"}]
    })
}

// ---------------------------------------------------------------------------
// Mock upstreams com `usage` configurável por teste.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ChatMock {
    calls: Arc<Mutex<Vec<Value>>>,
    usage: Value,
}

#[derive(Clone)]
struct ResponsesMock {
    calls: Arc<Mutex<Vec<Value>>>,
    usage: Value,
}

#[derive(Clone)]
struct AnthropicMock {
    calls: Arc<Mutex<Vec<Value>>>,
    usage: Value,
}

async fn mock_chat(State(st): State<ChatMock>, Json(body): Json<Value>) -> Response {
    st.calls.lock().await.push(body.clone());
    if body.get("stream").and_then(Value::as_bool).unwrap_or(false) {
        let usage = serde_json::to_string(&st.usage).unwrap_or_default();
        let sse = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"hi\"}}}}]}}\n\n\
             data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}],\"usage\":{usage}}}\n\n\
             data: [DONE]\n\n",
        );
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from(sse))
            .unwrap();
    }
    Json(json!({
        "id": "chatcmpl-cache",
        "choices": [{"finish_reason": "stop",
            "message": {"role": "assistant", "content": "cached reply"}}],
        "usage": st.usage,
    }))
    .into_response()
}

async fn mock_responses(State(st): State<ResponsesMock>, Json(body): Json<Value>) -> Response {
    st.calls.lock().await.push(body.clone());
    if body.get("stream").and_then(Value::as_bool).unwrap_or(false) {
        let usage = serde_json::to_string(&st.usage).unwrap_or_default();
        let sse = format!(
            "data: {{\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}}\n\n\
             data: {{\"type\":\"response.completed\",\"response\":{{\"id\":\"resp-cache\",\
             \"status\":\"completed\",\"usage\":{usage}}}}}\n\n",
        );
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from(sse))
            .unwrap();
    }
    Json(json!({
        "id": "resp-cache",
        "status": "completed",
        "output": [{"type": "message", "content": [
            {"type": "output_text", "text": "cached reply"}
        ]}],
        "usage": st.usage,
    }))
    .into_response()
}

async fn mock_messages(State(st): State<AnthropicMock>, Json(body): Json<Value>) -> Response {
    st.calls.lock().await.push(body.clone());
    if body.get("stream").and_then(Value::as_bool).unwrap_or(false) {
        let usage = serde_json::to_string(&st.usage).unwrap_or_default();
        let sse = format!(
            "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"id\":\"msg_up\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"mock-anth\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{usage}}}}}\n\n\
             event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n\
             event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"hi\"}}}}\n\n\
             event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
             event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\",\"stop_sequence\":null}},\"usage\":{usage}}}\n\n\
             event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n",
        );
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from(sse))
            .unwrap();
    }
    Json(json!({
        "id": "msg_up", "type": "message", "role": "assistant",
        "model": "mock-anth",
        "content": [{"type": "text", "text": "mock anthropic reply"}],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": st.usage,
    }))
    .into_response()
}

async fn spawn_chat(usage: Value) -> (String, ChatMock) {
    let mock = ChatMock {
        calls: Arc::new(Mutex::new(vec![])),
        usage,
    };
    let app = Router::new()
        .route("/chat/completions", post(mock_chat))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), mock)
}

async fn spawn_responses(usage: Value) -> (String, ResponsesMock) {
    let mock = ResponsesMock {
        calls: Arc::new(Mutex::new(vec![])),
        usage,
    };
    let app = Router::new()
        .route("/responses", post(mock_responses))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), mock)
}

async fn spawn_anthropic(usage: Value) -> (String, AnthropicMock) {
    let mock = AnthropicMock {
        calls: Arc::new(Mutex::new(vec![])),
        usage,
    };
    let app = Router::new()
        .route("/messages", post(mock_messages))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), mock)
}

// ---------------------------------------------------------------------------
// Leitor tolerante, exercido pelos conversores públicos (o helper é
// crate-internal, então os shapes são cobertos pelo comportamento).
// ---------------------------------------------------------------------------

#[test]
fn cached_reader_covers_all_upstream_shapes() {
    // DeepSeek nativo.
    let resp = json!({"choices": [{"finish_reason": "stop",
        "message": {"content": "x"}}],
        "usage": {"prompt_tokens": 100, "completion_tokens": 5,
                  "prompt_cache_hit_tokens": 80, "prompt_cache_miss_tokens": 20}});
    let out = openai_to_anthropic(&resp, "gw");
    assert_eq!(out["usage"]["cache_read_input_tokens"], 80);
    assert_eq!(out["usage"]["input_tokens"], 100);
    assert_eq!(out["usage"]["output_tokens"], 5);

    // Shape OpenAI (`prompt_tokens_details`).
    let resp = json!({"choices": [{"finish_reason": "stop",
        "message": {"content": "x"}}],
        "usage": {"prompt_tokens": 100, "completion_tokens": 5,
                  "prompt_tokens_details": {"cached_tokens": 60}}});
    assert_eq!(
        openai_to_anthropic(&resp, "gw")["usage"]["cache_read_input_tokens"],
        60
    );

    // Shape Responses (`input_tokens_details`).
    let resp = json!({"status": "completed", "output": [],
        "usage": {"input_tokens": 100, "output_tokens": 5,
                  "input_tokens_details": {"cached_tokens": 40}}});
    assert_eq!(
        responses_to_anthropic(&resp, "gw")["usage"]["cache_read_input_tokens"],
        40
    );

    // Precedência: nativo DeepSeek vence os detalhes.
    let resp = json!({"choices": [{"finish_reason": "stop",
        "message": {"content": "x"}}],
        "usage": {"prompt_tokens": 100, "completion_tokens": 5,
                  "prompt_cache_hit_tokens": 80,
                  "prompt_tokens_details": {"cached_tokens": 60}}});
    assert_eq!(
        openai_to_anthropic(&resp, "gw")["usage"]["cache_read_input_tokens"],
        80
    );

    // Miss: campo omitido (o shape de `usage` fica byte-idêntico ao de antes).
    let resp = json!({"choices": [{"finish_reason": "stop",
        "message": {"content": "x"}}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 5}});
    let out = openai_to_anthropic(&resp, "gw");
    assert!(
        out["usage"].get("cache_read_input_tokens").is_none(),
        "{out}"
    );
    let resp = json!({"status": "completed", "output": [],
        "usage": {"input_tokens": 3, "output_tokens": 5}});
    let out = responses_to_anthropic(&resp, "gw");
    assert!(
        out["usage"].get("cache_read_input_tokens").is_none(),
        "{out}"
    );

    // Tradutor de stream: miss omite, hit emite.
    let mut tr = StreamTranslator::new("gw");
    let _ = tr.prefix();
    let end = tr.finish("end_turn", 3, 5);
    assert!(!end.iter().any(|e| e.contains("cache_read_input_tokens")));
    let mut tr = StreamTranslator::new("gw");
    let _ = tr.prefix();
    tr.cache_read_tokens = 80;
    let end = tr.finish("end_turn", 100, 5);
    assert!(
        end.iter()
            .any(|e| e.contains("\"cache_read_input_tokens\":80")),
        "{end:?}"
    );
}

// ---------------------------------------------------------------------------
// E2E Chat (o caminho do deepseek-v4.1-flash via opencode-go).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chat_nonstream_reports_deepseek_cache_hit() {
    let (base, _mock) = spawn_chat(json!({
        "prompt_tokens": 100, "completion_tokens": 5,
        "prompt_cache_hit_tokens": 80, "prompt_cache_miss_tokens": 20
    }))
    .await;
    let state = seeded_state(
        test_config(),
        vec![mock_entry(&base, CHAT_PKG, "mock-chat")],
        vec![alias("claude-mock-chat", "opencode/mock-chat")],
    )
    .await;
    let server = axum_test::TestServer::new(router(state)).unwrap();
    let body: Value = server
        .post("/v1/messages")
        .add_header("x-api-key", TOKEN)
        .json(&msg_body("claude-mock-chat"))
        .await
        .json();
    assert_eq!(body["usage"]["input_tokens"], 100);
    assert_eq!(body["usage"]["output_tokens"], 5);
    assert_eq!(body["usage"]["cache_read_input_tokens"], 80);
}

#[tokio::test]
async fn chat_nonstream_reports_openai_details_shape() {
    let (base, _mock) = spawn_chat(json!({
        "prompt_tokens": 100, "completion_tokens": 5,
        "prompt_tokens_details": {"cached_tokens": 60, "audio_tokens": 0}
    }))
    .await;
    let state = seeded_state(
        test_config(),
        vec![mock_entry(&base, CHAT_PKG, "mock-chat")],
        vec![alias("claude-mock-chat", "opencode/mock-chat")],
    )
    .await;
    let server = axum_test::TestServer::new(router(state)).unwrap();
    let body: Value = server
        .post("/v1/messages")
        .add_header("x-api-key", TOKEN)
        .json(&msg_body("claude-mock-chat"))
        .await
        .json();
    assert_eq!(body["usage"]["cache_read_input_tokens"], 60);
}

#[tokio::test]
async fn chat_nonstream_miss_omits_cache_field() {
    let (base, _mock) = spawn_chat(json!({"prompt_tokens": 3, "completion_tokens": 5})).await;
    let state = seeded_state(
        test_config(),
        vec![mock_entry(&base, CHAT_PKG, "mock-chat")],
        vec![alias("claude-mock-chat", "opencode/mock-chat")],
    )
    .await;
    let server = axum_test::TestServer::new(router(state)).unwrap();
    let body: Value = server
        .post("/v1/messages")
        .add_header("x-api-key", TOKEN)
        .json(&msg_body("claude-mock-chat"))
        .await
        .json();
    assert!(
        body["usage"].get("cache_read_input_tokens").is_none(),
        "{body}"
    );
}

#[tokio::test]
async fn chat_stream_reports_cache_in_message_delta() {
    let (base, _mock) = spawn_chat(json!({
        "prompt_tokens": 100, "completion_tokens": 5,
        "prompt_cache_hit_tokens": 80
    }))
    .await;
    let state = seeded_state(
        test_config(),
        vec![mock_entry(&base, CHAT_PKG, "mock-chat")],
        vec![alias("claude-mock-chat", "opencode/mock-chat")],
    )
    .await;
    let server = axum_test::TestServer::new(router(state)).unwrap();
    let mut req = msg_body("claude-mock-chat");
    req["stream"] = Value::Bool(true);
    let resp = server
        .post("/v1/messages")
        .add_header("x-api-key", TOKEN)
        .json(&req)
        .await;
    assert_eq!(resp.status_code(), 200);
    let text = resp.text();
    assert!(text.contains("message_delta"), "{text}");
    assert!(text.contains("\"cache_read_input_tokens\":80"), "{text}");
    assert!(text.contains("message_stop"), "{text}");
}

#[tokio::test]
async fn chat_stream_miss_omits_cache_field() {
    let (base, _mock) = spawn_chat(json!({"prompt_tokens": 3, "completion_tokens": 2})).await;
    let state = seeded_state(
        test_config(),
        vec![mock_entry(&base, CHAT_PKG, "mock-chat")],
        vec![alias("claude-mock-chat", "opencode/mock-chat")],
    )
    .await;
    let server = axum_test::TestServer::new(router(state)).unwrap();
    let mut req = msg_body("claude-mock-chat");
    req["stream"] = Value::Bool(true);
    let resp = server
        .post("/v1/messages")
        .add_header("x-api-key", TOKEN)
        .json(&req)
        .await;
    assert_eq!(resp.status_code(), 200);
    let text = resp.text();
    assert!(text.contains("message_stop"), "{text}");
    assert!(!text.contains("cache_read_input_tokens"), "{text}");
}

// ---------------------------------------------------------------------------
// E2E Responses.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn responses_nonstream_reports_cache_hit() {
    let (base, _mock) = spawn_responses(json!({
        "input_tokens": 100, "output_tokens": 5, "total_tokens": 105,
        "input_tokens_details": {"cached_tokens": 40}
    }))
    .await;
    let state = seeded_state(
        test_config(),
        vec![mock_entry(&base, RESPONSES_PKG, "mock-resp")],
        vec![alias("claude-mock-resp", "opencode/mock-resp")],
    )
    .await;
    let server = axum_test::TestServer::new(router(state)).unwrap();
    let body: Value = server
        .post("/v1/messages")
        .add_header("x-api-key", TOKEN)
        .json(&msg_body("claude-mock-resp"))
        .await
        .json();
    assert_eq!(body["usage"]["input_tokens"], 100);
    assert_eq!(body["usage"]["cache_read_input_tokens"], 40);
}

#[tokio::test]
async fn responses_stream_reports_cache_in_message_delta() {
    let (base, _mock) = spawn_responses(json!({
        "input_tokens": 100, "output_tokens": 5, "total_tokens": 105,
        "input_tokens_details": {"cached_tokens": 40}
    }))
    .await;
    let state = seeded_state(
        test_config(),
        vec![mock_entry(&base, RESPONSES_PKG, "mock-resp")],
        vec![alias("claude-mock-resp", "opencode/mock-resp")],
    )
    .await;
    let server = axum_test::TestServer::new(router(state)).unwrap();
    let mut req = msg_body("claude-mock-resp");
    req["stream"] = Value::Bool(true);
    let resp = server
        .post("/v1/messages")
        .add_header("x-api-key", TOKEN)
        .json(&req)
        .await;
    assert_eq!(resp.status_code(), 200);
    let text = resp.text();
    assert!(text.contains("message_delta"), "{text}");
    assert!(text.contains("\"cache_read_input_tokens\":40"), "{text}");
    assert!(text.contains("message_stop"), "{text}");
}

// ---------------------------------------------------------------------------
// Passthrough Anthropic: `usage` (incluindo cache) passa verbatim.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anthropic_passthrough_keeps_cache_verbatim() {
    let usage = json!({
        "input_tokens": 50, "output_tokens": 7,
        "cache_read_input_tokens": 30, "cache_creation_input_tokens": 20
    });
    let (base, _mock) = spawn_anthropic(usage).await;
    let state = seeded_state(
        test_config(),
        vec![mock_entry(&base, ANTHROPIC_PKG, "mock-anth")],
        vec![alias("claude-mock-anth", "opencode/mock-anth")],
    )
    .await;
    let server = axum_test::TestServer::new(router(state)).unwrap();
    let body: Value = server
        .post("/v1/messages")
        .add_header("x-api-key", TOKEN)
        .json(&msg_body("claude-mock-anth"))
        .await
        .json();
    assert_eq!(body["usage"]["cache_read_input_tokens"], 30);
    assert_eq!(body["usage"]["cache_creation_input_tokens"], 20);
}

// ---------------------------------------------------------------------------
// Estabilidade do prefixo: traduzir o mesmo body duas vezes produz bytes
// idênticos — o gateway não injeta jitter que quebraria o prefix-cache
// server-side (ex. DeepSeek) antes mesmo do upstream.
// ---------------------------------------------------------------------------

#[test]
fn translation_is_prefix_stable() {
    let body = json!({
        "model": "x",
        "system": [
            {"type": "text", "text": "you are helpful"},
            {"type": "text", "text": "be concise",
             "cache_control": {"type": "ephemeral"}}
        ],
        "messages": [
            {"role": "user", "content": "read it"},
            {"role": "assistant", "content": [
                {"type": "text", "text": "looking"},
                {"type": "tool_use", "id": "t1", "name": "Read", "input": {"path": "a"}}
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "file contents"},
                {"type": "text", "text": "now summarize"}
            ]}
        ],
        "tools": [{"name": "Read", "description": "d", "input_schema": {"type": "object"}}],
        "tool_choice": {"type": "auto"},
        "temperature": 0.7,
        "max_tokens": 64
    });
    let a = serde_json::to_string(&anthropic_to_openai(&body, "up")).unwrap();
    let b = serde_json::to_string(&anthropic_to_openai(&body, "up")).unwrap();
    assert_eq!(a, b);
    let a = serde_json::to_string(&anthropic_to_responses(&body, "up")).unwrap();
    let b = serde_json::to_string(&anthropic_to_responses(&body, "up")).unwrap();
    assert_eq!(a, b);
}
