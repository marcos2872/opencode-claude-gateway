//! E2E da edge Codex (`POST /v1/responses`, Fase 1 passthrough) com upstream
//! mockado: HTTP real em 127.0.0.1, sem binário `opencode` e sem rede.
//!
//! Arquivo autocontido de propósito: os testes de compatibilidade Claude em
//! `tests/gateway.rs` não são tocados por esta feature, e qualquer regressão
//! deles continua sendo medida exatamente onde sempre foi. Os helpers abaixo
//! espelham os de `gateway.rs` (mesmo padrão já usado por `tests/perf.rs`).

use axum::{
    extract::State,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use opencode_claude_gateway::api::server::{router, AppState};
use opencode_claude_gateway::config::AppConfig;
use opencode_claude_gateway::domain::{AliasEntry, CatalogEntry, CatalogSettings};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
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

/// Body estilo Codex: `instructions` + `input` com itens, tools, `include`
/// (sempre), `store:false`, `reasoning` e `prompt_cache_key`.
fn codex_body(model: &str) -> Value {
    json!({
        "model": model,
        "instructions": "you are codex",
        "input": [
            {"type": "message", "role": "user", "content": [
                {"type": "input_text", "text": "hi"}
            ]},
            {"type": "function_call", "id": "fc_1", "call_id": "call_1",
             "name": "shell", "arguments": "{\"cmd\":\"ls\"}"},
            {"type": "function_call_output", "call_id": "call_1", "output": "ok"}
        ],
        "tools": [{"type": "function", "name": "shell", "description": "d",
                   "parameters": {"type": "object"}}],
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "reasoning": {"effort": "medium"},
        "include": ["reasoning.encrypted_content"],
        "prompt_cache_key": "cache-key-1",
        "store": false,
        "stream": true
    })
}

// ---------------------------------------------------------------------------
// Mock upstream (Responses): happy-path SSE/JSON, truncamento de stream e
// rejeição 400 — cada teste escolhe o modo.
// ---------------------------------------------------------------------------

// Modos do mock: o padrão (`_`) responde normalmente; os outros forçam os
// caminhos de erro.
const MODE_TRUNCATED: u8 = 1;
const MODE_REJECT: u8 = 2;

#[derive(Clone, Default)]
struct MockUpstream {
    calls: Arc<Mutex<Vec<Value>>>,
    sessions: Arc<Mutex<Vec<String>>>,
    mode: Arc<AtomicU8>,
}

/// SSE do upstream: `response.created` → `output_item.done` com os
/// arguments completos de um `function_call` → `response.completed` com
/// usage completo (exatamente o contrato que o parser do Codex exige).
const SSE_HAPPY: &str = concat!(
    "event: response.created\n",
    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_up\",",
    "\"object\":\"response\",\"status\":\"in_progress\"}}\n\n",
    "data: {\"type\":\"response.output_item.done\",\"output_index\":0,",
    "\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",",
    "\"name\":\"shell\",\"arguments\":\"{\\\"cmd\\\":\\\"ls\\\"}\"}}\n\n",
    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_up\",",
    "\"object\":\"response\",\"status\":\"completed\",\"usage\":",
    "{\"input_tokens\":5,\"output_tokens\":7,\"total_tokens\":12}}}\n\n",
);

async fn mock_responses(
    State(st): State<MockUpstream>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    st.calls.lock().await.push(body.clone());
    if let Some(s) = headers
        .get("x-opencode-session")
        .and_then(|v| v.to_str().ok())
    {
        st.sessions.lock().await.push(s.to_string());
    }
    match st.mode.load(Ordering::SeqCst) {
        MODE_REJECT => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": {
                "message": "unknown parameter: prompt_cache_key",
                "type": "invalid_request_error",
                "code": "unknown_parameter"
            }})),
        )
            .into_response(),
        MODE_TRUNCATED => {
            // `response.created` + uma linha cortada no meio, depois EOF sem
            // evento terminal: a garra do gateway precisa emitir
            // `response.failed` antes de fechar.
            let s = async_stream::stream! {
                yield Ok::<_, std::io::Error>(axum::body::Bytes::from_static(
                    b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_up\"}}\n\n",
                ));
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                yield Ok(axum::body::Bytes::from_static(
                    b"data: {\"type\":\"response.outp",
                ));
            };
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(axum::body::Body::from_stream(s))
                .unwrap()
        }
        _ => {
            if body
                .get("stream")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from(SSE_HAPPY))
                    .unwrap()
            } else {
                Json(json!({
                    "id": "resp_up",
                    "object": "response",
                    "model": "mock-codex",
                    "status": "completed",
                    "output": [{"type": "message", "content": [
                        {"type": "output_text", "text": "mock codex reply"}
                    ]}],
                    "usage": {"input_tokens": 3, "output_tokens": 5, "total_tokens": 8}
                }))
                .into_response()
            }
        }
    }
}

async fn mock_chat_completions(
    State(st): State<MockUpstream>,
    Json(body): Json<Value>,
) -> Response {
    st.calls.lock().await.push(body.clone());
    Json(json!({
        "id": "chatcmpl-codex",
        "choices": [{"index": 0, "finish_reason": "stop",
            "message": {"role": "assistant", "content": "mock chat reply"}}],
        "usage": {"prompt_tokens": 7, "completion_tokens": 9}
    }))
    .into_response()
}

/// Anthropic upstream for the Fase 2 translated path: answers a canned
/// streaming message (message_start -> text delta -> message_stop) when the
/// request asks for `stream`, else a plain message JSON.
async fn mock_messages(State(st): State<MockUpstream>, Json(body): Json<Value>) -> Response {
    st.calls.lock().await.push(body.clone());
    if body.get("stream").and_then(Value::as_bool).unwrap_or(false) {
        let sse = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_up\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"mock-anth\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":7,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"olá codex\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"input_tokens\":7,\"output_tokens\":9}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        );
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from(sse))
            .unwrap();
    }
    Json(json!({
        "id": "msg_up", "type": "message", "role": "assistant",
        "model": body.get("model").cloned().unwrap_or(Value::Null),
        "content": [{"type": "text", "text": "mock anthropic reply"}],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": {"input_tokens": 3, "output_tokens": 5}
    }))
    .into_response()
}

/// Bodies recorded by the upstream. Each test drives exactly one route on
/// its own mock, so the path filter is documentation, not selection.
async fn calls_to(mock: &MockUpstream, _path: &str) -> Vec<Value> {
    mock.calls.lock().await.clone()
}

async fn spawn_mock(mock: MockUpstream) -> String {
    let app = Router::new()
        .route("/responses", post(mock_responses))
        .route("/chat/completions", post(mock_chat_completions))
        .route("/messages", post(mock_messages))
        .with_state(mock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

// ---------------------------------------------------------------------------
// Testes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn codex_responses_stream_passthrough_keeps_contract() {
    let mock = MockUpstream::default();
    let base = spawn_mock(mock.clone()).await;
    let state = seeded_state(
        test_config(),
        vec![mock_entry(&base, RESPONSES_PKG, "mock-codex")],
        vec![alias("claude-codex", "opencode/mock-codex")],
    )
    .await;
    let server = axum_test::TestServer::new(router(state)).unwrap();
    let resp = server
        .post("/v1/responses")
        // Codex sempre manda Bearer + `session-id` (nunca os headers Claude).
        .add_header("authorization", format!("Bearer {TOKEN}"))
        .add_header("session-id", "codex-session-1")
        .json(&codex_body("claude-codex"))
        .await;
    assert_eq!(resp.status_code(), 200);
    let text = resp.text();
    // Eventos do upstream chegam intactos, incluindo o terminal exigido.
    assert!(text.contains("response.created"), "{text}");
    assert!(
        text.contains("response.output_item.done")
            && text.contains("\"name\":\"shell\"")
            && text.contains("\\\"cmd\\\":\\\"ls\\\""),
        "{text}"
    );
    assert!(text.contains("response.completed"), "{text}");
    assert!(text.contains("\"total_tokens\":12"), "{text}");
    // Nenhum frame Anthropic vazou para esta edge.
    assert!(!text.contains("message_start"), "{text}");

    // Upstream recebeu /responses com `model` reescrito e o resto do corpo
    // do Codex preservado (passthrough).
    let calls = mock.calls.lock().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["model"], "mock-codex");
    assert_eq!(calls[0]["instructions"], "you are codex");
    assert_eq!(calls[0]["store"], false);
    assert_eq!(calls[0]["prompt_cache_key"], "cache-key-1");
    assert_eq!(calls[0]["include"][0], "reasoning.encrypted_content");
    assert_eq!(calls[0]["reasoning"]["effort"], "medium");
    assert_eq!(calls[0]["input"][1]["type"], "function_call");
    assert_eq!(calls[0]["tools"][0]["type"], "function");
    drop(calls);
    // `session-id` do Codex vira o header de roteamento do Go.
    let sessions = mock.sessions.lock().await;
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0], "codex-session-1");
}

#[tokio::test]
async fn codex_stream_upstream_died_emits_response_failed() {
    let mock = MockUpstream {
        mode: Arc::new(AtomicU8::new(MODE_TRUNCATED)),
        ..MockUpstream::default()
    };
    let base = spawn_mock(mock.clone()).await;
    let state = seeded_state(
        test_config(),
        vec![mock_entry(&base, RESPONSES_PKG, "mock-codex")],
        vec![alias("claude-codex", "opencode/mock-codex")],
    )
    .await;
    let server = axum_test::TestServer::new(router(state)).unwrap();
    let resp = server
        .post("/v1/responses")
        .add_header("authorization", format!("Bearer {TOKEN}"))
        .json(&codex_body("claude-codex"))
        .await;
    assert_eq!(resp.status_code(), 200);
    let text = resp.text();
    // A garra emite `response.failed` (com `response.error`) antes do EOF —
    // sem ele o Codex esperaria os 300s do idle timeout dele.
    assert!(text.contains("event: response.failed"), "{text}");
    assert!(text.contains("\"type\":\"response.failed\""), "{text}");
    assert!(text.contains("\"code\":\"stream_closed\""), "{text}");
    // O evento chega depois do `created` parcial e é o último do stream
    // (a mensagem da garra cita `response.completed`, então casa o type).
    let created_at = text.find("response.created").unwrap();
    let failed_at = text.find("\"type\":\"response.failed\"").unwrap();
    assert!(created_at < failed_at, "{text}");
    assert!(!text.contains("\"type\":\"response.completed\""), "{text}");
}

#[tokio::test]
async fn codex_unknown_model_is_404_openai_shape() {
    let mock = MockUpstream::default();
    let base = spawn_mock(mock.clone()).await;
    let state = seeded_state(
        test_config(),
        vec![mock_entry(&base, RESPONSES_PKG, "mock-codex")],
        vec![alias("claude-codex", "opencode/mock-codex")],
    )
    .await;
    let server = axum_test::TestServer::new(router(state)).unwrap();
    let mut body = codex_body("claude-not-there");
    body["stream"] = json!(false);
    let resp = server
        .post("/v1/responses")
        .add_header("authorization", format!("Bearer {TOKEN}"))
        .json(&body)
        .await;
    assert_eq!(resp.status_code(), 404);
    let v: Value = resp.json();
    // Shape OpenAI: sem `type` top-level, mensagem sob `error`.
    assert!(v.get("type").is_none(), "{v}");
    assert_eq!(v["error"]["type"], "not_found_error");
    assert!(v["error"]["message"].is_string(), "{v}");
    // E nada foi ao upstream.
    assert!(mock.calls.lock().await.is_empty());
}

#[tokio::test]
async fn codex_unauthorized_is_401_openai_shape() {
    let state = seeded_state(test_config(), vec![], vec![]).await;
    let server = axum_test::TestServer::new(router(state)).unwrap();
    // Sem credential nenhuma (Codex só manda Authorization).
    let resp = server.post("/v1/responses").json(&codex_body("x")).await;
    assert_eq!(resp.status_code(), 401);
    let v: Value = resp.json();
    assert!(v.get("type").is_none(), "{v}");
    assert_eq!(v["error"]["type"], "authentication_error");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("OCG_AUTH_TOKEN"),
        "{v}"
    );
}

#[tokio::test]
async fn codex_chat_upstream_translated_round_trip() {
    let mock = MockUpstream::default();
    let base = spawn_mock(mock.clone()).await;
    let state = seeded_state(
        test_config(),
        vec![mock_entry(&base, CHAT_PKG, "mock-chat")],
        vec![alias("claude-codex-chat", "opencode/mock-chat")],
    )
    .await;
    let server = axum_test::TestServer::new(router(state)).unwrap();
    let mut body = codex_body("claude-codex-chat");
    body["stream"] = json!(false);
    body["tools"] = json!([
        {"type": "function", "name": "shell", "description": "d",
         "parameters": {"type": "object"}},
        {"type": "custom", "name": "apply_patch", "description": "p"},
        {"type": "web_search"}
    ]);
    let resp = server
        .post("/v1/responses")
        .add_header("authorization", format!("Bearer {TOKEN}"))
        .json(&body)
        .await;
    assert_eq!(resp.status_code(), 200);
    // Client sees the Responses dialect, even though upstream is Chat.
    let v: Value = resp.json();
    assert_eq!(v["object"], "response");
    assert_eq!(v["status"], "completed");
    assert!(v["id"].as_str().unwrap().starts_with("resp_"), "{v}");
    assert_eq!(v["output"][0]["type"], "message");
    assert_eq!(v["output"][0]["content"][0]["type"], "output_text");
    assert_eq!(v["output"][0]["content"][0]["text"], "mock chat reply");
    assert_eq!(v["usage"]["input_tokens"], 7);
    assert_eq!(v["usage"]["total_tokens"], 16);

    // Upstream received the canonical translation behind the scenes.
    let calls = calls_to(&mock, "chat/completions").await;
    assert_eq!(calls.len(), 1);
    let up = &calls[0];
    assert_eq!(up["model"], "mock-chat");
    let sys = up["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "system")
        .and_then(|m| m["content"].as_str())
        .unwrap_or("")
        .to_string();
    assert!(sys.contains("you are codex"), "{up}");
    // function + custom survive as functions; web_search has no equivalent.
    let names: Vec<&str> = up["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["shell", "apply_patch"], "{names:?}");
    // The tool history round-tripped (function_call -> tool_calls/tool role).
    let roles: Vec<&str> = up["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert!(roles.contains(&"tool"), "{roles:?}");
}

#[tokio::test]
async fn codex_anthropic_upstream_translated_stream() {
    let mock = MockUpstream::default();
    let base = spawn_mock(mock.clone()).await;
    let state = seeded_state(
        test_config(),
        vec![mock_entry(&base, ANTHROPIC_PKG, "mock-anth")],
        vec![alias("claude-codex-anth", "opencode/mock-anth")],
    )
    .await;
    let server = axum_test::TestServer::new(router(state)).unwrap();
    let resp = server
        .post("/v1/responses")
        .add_header("authorization", format!("Bearer {TOKEN}"))
        .json(&codex_body("claude-codex-anth"))
        .await;
    assert_eq!(resp.status_code(), 200);
    // Anthropic SSE comes back as Responses SSE — no Anthropic leak.
    let text = resp.text();
    assert!(text.contains("response.created"), "{text}");
    assert!(text.contains("response.output_text.delta"), "{text}");
    assert!(text.contains("olá codex"), "{text}");
    assert!(text.contains("response.output_item.done"), "{text}");
    assert!(text.contains("response.completed"), "{text}");
    assert!(text.contains("\"input_tokens\":7"), "{text}");
    assert!(text.contains("\"total_tokens\":16"), "{text}");
    assert!(!text.contains("message_start"), "{text}");
    // The upstream saw a canonical Anthropic Messages body.
    let calls = calls_to(&mock, "messages").await;
    assert_eq!(calls.len(), 1);
    let up = &calls[0];
    assert_eq!(up["model"], "mock-anth");
    assert_eq!(up["stream"], true);
    let sys = up["system"].as_str().unwrap_or("");
    assert!(sys.contains("you are codex"), "{up}");
    let has_tool_use = up["messages"].as_array().unwrap().iter().any(|m| {
        m["content"]
            .as_array()
            .map(|b| b.iter().any(|x| x["type"] == "tool_use"))
            .unwrap_or(false)
    });
    assert!(has_tool_use, "{up}");
}

#[tokio::test]
async fn codex_upstream_400_maps_to_openai_error() {
    let mock = MockUpstream {
        mode: Arc::new(AtomicU8::new(MODE_REJECT)),
        ..MockUpstream::default()
    };
    let base = spawn_mock(mock.clone()).await;
    let state = seeded_state(
        test_config(),
        vec![mock_entry(&base, RESPONSES_PKG, "mock-codex")],
        vec![alias("claude-codex", "opencode/mock-codex")],
    )
    .await;
    let server = axum_test::TestServer::new(router(state)).unwrap();
    let resp = server
        .post("/v1/responses")
        .add_header("authorization", format!("Bearer {TOKEN}"))
        .json(&codex_body("claude-codex"))
        .await;
    assert_eq!(resp.status_code(), 400);
    let v: Value = resp.json();
    assert!(v.get("type").is_none(), "{v}");
    assert_eq!(v["error"]["type"], "invalid_request_error");
    assert_eq!(v["error"]["message"], "unknown parameter: prompt_cache_key");
    // `code` do upstream é preservado quando existe.
    assert_eq!(v["error"]["code"], "unknown_parameter");
}

#[tokio::test]
async fn codex_non_stream_rewrites_model_to_gateway_id() {
    let mock = MockUpstream::default();
    let base = spawn_mock(mock.clone()).await;
    let state = seeded_state(
        test_config(),
        vec![mock_entry(&base, RESPONSES_PKG, "mock-codex")],
        vec![alias("claude-codex", "opencode/mock-codex")],
    )
    .await;
    let server = axum_test::TestServer::new(router(state)).unwrap();
    let mut body = codex_body("claude-codex");
    body["stream"] = json!(false);
    let resp = server
        .post("/v1/responses")
        .add_header("authorization", format!("Bearer {TOKEN}"))
        .json(&body)
        .await;
    assert_eq!(resp.status_code(), 200);
    let v: Value = resp.json();
    // Visão consistente para o cliente: o id do gateway, não o do upstream.
    assert_eq!(v["model"], "claude-codex");
    assert_eq!(v["usage"]["total_tokens"], 8);
    let calls = mock.calls.lock().await;
    assert_eq!(calls[0]["model"], "mock-codex");
    assert_eq!(calls[0]["stream"], false);
}

#[tokio::test]
async fn codex_endpoint_can_be_disabled() {
    let config = AppConfig {
        auth_token: TOKEN.to_string(),
        responses_endpoint: false,
        ..AppConfig::default()
    };
    let state = seeded_state(config, vec![], vec![]).await;
    let server = axum_test::TestServer::new(router(state)).unwrap();
    let resp = server
        .post("/v1/responses")
        .add_header("authorization", format!("Bearer {TOKEN}"))
        .json(&codex_body("x"))
        .await;
    assert_eq!(resp.status_code(), 404);
    // A edge Anthropic continua de pé com o flag desligado.
    let resp = server
        .post("/v1/messages")
        .add_header("authorization", format!("Bearer {TOKEN}"))
        .json(&json!({"model": "x", "max_tokens": 8, "messages": []}))
        .await;
    // 404 do resolve (estado vazio), em shape Anthropic — rota existe.
    assert_eq!(resp.status_code(), 404);
    let v: Value = resp.json();
    assert_eq!(v["type"], "error");
    assert_eq!(v["error"]["type"], "not_found_error");
}

// ---------------------------------------------------------------------------
// Catálogo nativo para o picker do Codex (`model_catalog_url` →
// GET /v1/models/codex). Sem upstream: só a moldura `{"models":[...]}` que
// o parser `ModelsResponse` do Codex decoda.
// ---------------------------------------------------------------------------

/// Campos obrigatórios do `ModelInfo` (fixture `remote_model` em
/// `model-provider/src/provider.rs` do Codex): sem eles o decode falha e o
/// picker não mostra nada.
const CODEX_REQUIRED_MODEL_FIELDS: [&str; 11] = [
    "slug",
    "display_name",
    // Sem isto o `ModelsResponse` do Codex rejeita o decode inteiro
    // ("missing both `base_instructions` and `model_messages...`").
    "base_instructions",
    "supported_reasoning_levels",
    "shell_type",
    "visibility",
    "supported_in_api",
    "priority",
    "support_verbosity",
    "truncation_policy",
    "experimental_supported_tools",
];

#[tokio::test]
async fn codex_model_catalog_lists_every_gateway_model() {
    let base = "http://127.0.0.1:9";
    let mut windowed = alias("claude-codex-grok", "opencode/mock-codex");
    windowed.context_window = Some(256_000);
    let state = seeded_state(
        test_config(),
        vec![
            mock_entry(base, RESPONSES_PKG, "mock-codex"),
            mock_entry(base, CHAT_PKG, "mock-chat"),
        ],
        vec![windowed, alias("claude-codex-chat", "opencode/mock-chat")],
    )
    .await;
    let server = axum_test::TestServer::new(router(state)).unwrap();
    let resp = server
        .get("/v1/models/codex")
        .add_header("authorization", format!("Bearer {TOKEN}"))
        .await;
    assert_eq!(resp.status_code(), 200);
    let v: Value = resp.json();
    // Fase 2: Responses (passthrough) AND Chat (translated) both qualify.
    let models = v["models"].as_array().expect("`models` array");
    assert_eq!(models.len(), 2, "{v}");
    let slugs: Vec<&str> = models.iter().map(|m| m["slug"].as_str().unwrap()).collect();
    assert_eq!(
        slugs,
        ["claude-codex-grok", "claude-codex-chat"],
        "{slugs:?}"
    );
    let m = &models[0];
    for k in CODEX_REQUIRED_MODEL_FIELDS {
        assert!(m.get(k).is_some(), "missing `{k}` in {v}");
    }
    assert!(!m["base_instructions"].as_str().unwrap().is_empty(), "{v}");
    assert_eq!(m["visibility"], "list");
    assert_eq!(m["supported_in_api"], true);
    assert_eq!(m["shell_type"], "unified_exec");
    assert_eq!(m["truncation_policy"]["mode"], "bytes");
    assert_eq!(m["truncation_policy"]["limit"], 10000);
    // Janela como campo estruturado — slug sem sufixo `[1m]`.
    assert_eq!(m["context_window"], 256_000);
    assert_eq!(m["max_context_window"], 256_000);
    assert!(!m["slug"].as_str().unwrap().contains('['));
    // Linha sem janela conhecida omite os campos (não manda null).
    assert!(models[1].get("context_window").is_none(), "{v}");
    // Níveis de raciocínio no formato {effort, description}.
    let levels = m["supported_reasoning_levels"].as_array().unwrap();
    assert!(levels.len() >= 2, "{v}");
    assert_eq!(levels[0]["effort"], "minimal");
    assert!(levels[0]["description"].is_string());
}

#[tokio::test]
async fn codex_model_catalog_requires_token_in_openai_shape() {
    let state = seeded_state(test_config(), vec![], vec![]).await;
    let server = axum_test::TestServer::new(router(state)).unwrap();
    let resp = server.get("/v1/models/codex").await;
    assert_eq!(resp.status_code(), 401);
    let v: Value = resp.json();
    assert!(v.get("type").is_none(), "{v}");
    assert_eq!(v["error"]["type"], "authentication_error");
}

#[tokio::test]
async fn codex_model_catalog_hidden_when_endpoint_disabled() {
    let config = AppConfig {
        auth_token: TOKEN.to_string(),
        responses_endpoint: false,
        ..AppConfig::default()
    };
    let state = seeded_state(config, vec![], vec![]).await;
    let server = axum_test::TestServer::new(router(state)).unwrap();
    let resp = server
        .get("/v1/models/codex")
        .add_header("authorization", format!("Bearer {TOKEN}"))
        .await;
    assert_eq!(resp.status_code(), 404);
}
