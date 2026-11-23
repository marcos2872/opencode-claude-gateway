//! `ocg --status` usage accounting: what the forwards record, `GET /status`,
//! and the file round trip. Self-contained like the other feature suites: a
//! seeded `AppState` over a loopback upstream, no real binary.
//!
//! `OCG_STATS_FILE` is process-wide, so the file-backed tests hold a lock and
//! point it at a temp path (the daemon's `serve()` resolves it once at boot).

use axum::{
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use opencode_claude_gateway::api::server::{router, AppState};
use opencode_claude_gateway::config::AppConfig;
use opencode_claude_gateway::domain::{AliasEntry, CatalogEntry, CatalogSettings};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

const AUTH: &str = "stats-secret";
const CHAT_PKG: &str = "@opencode/ai/providers/openai-compatible";
const ANTHROPIC_PKG: &str = "@opencode/ai/providers/anthropic";

fn entry(provider: &str, model: &str, package: &str, base_url: &str) -> CatalogEntry {
    CatalogEntry {
        id: model.to_string(),
        model_id: model.to_string(),
        provider_id: provider.to_string(),
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

fn alias(gateway: &str, reference: &str) -> AliasEntry {
    AliasEntry {
        gateway_id: gateway.to_string(),
        opencode_ref: reference.to_string(),
        display_name: gateway.to_string(),
        description: String::new(),
        context_window: None,
        family_tier: None,
        family_default: false,
    }
}

fn config() -> AppConfig {
    AppConfig {
        auth_token: AUTH.to_string(),
        ..AppConfig::default()
    }
}

/// Upstream that answers one non-streaming Chat completion with usage.
async fn chat_upstream() -> Json<Value> {
    Json(json!({
        "id": "chatcmpl-1",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 120, "completion_tokens": 30}
    }))
}

/// Upstream that answers one non-streaming Anthropic message with usage.
async fn anthropic_upstream() -> Json<Value> {
    Json(json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "model": "claude-x",
        "content": [{"type": "text", "text": "hi"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 200, "output_tokens": 50, "cache_read_input_tokens": 40}
    }))
}

/// Boot a loopback upstream holding both handlers and seed the gateway at it.
async fn seeded(alias_ref: &str, package: &str, upstream: Router) -> AppState {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

    let provider = alias_ref.split('/').next().unwrap();
    let model = alias_ref.split('/').nth(1).unwrap();
    let state = AppState::new(config(), PathBuf::from("/nonexistent-ocg-test/opencode.db"));
    let e = entry(provider, model, package, &base);
    state.catalog.write().await.push(e);
    state
        .aliases
        .write()
        .await
        .push(alias("claude-sonnet-5", alias_ref));
    state
}

fn message_body(model: &str) -> Value {
    json!({
        "model": model,
        "max_tokens": 64,
        "messages": [{"role": "user", "content": "Say hi"}]
    })
}

#[tokio::test]
async fn translated_chat_request_is_counted_under_alias_and_ref() {
    let state = seeded(
        "opencode-go/deepseek-v4.1-flash",
        CHAT_PKG,
        Router::new().route("/chat/completions", post(chat_upstream)),
    )
    .await;
    let app = axum_test::TestServer::new(router(state.clone())).unwrap();

    let resp = app
        .post("/v1/messages")
        .add_header("x-api-key", AUTH)
        .json(&message_body("claude-sonnet-5"))
        .await;
    resp.assert_status_ok();

    let snapshot = state.stats.snapshot(7);
    assert_eq!(snapshot.requests, 1);
    assert_eq!(snapshot.tokens(), 150);
    let row = &snapshot.rows[0];
    assert_eq!(row.gateway_model, "claude-sonnet-5");
    assert_eq!(row.opencode_ref, "opencode-go/deepseek-v4.1-flash");
}

#[tokio::test]
async fn anthropic_passthrough_counts_its_usage_including_cache() {
    let state = seeded(
        "anthropic-mirror/claude-passthrough",
        ANTHROPIC_PKG,
        Router::new().route("/messages", post(anthropic_upstream)),
    )
    .await;
    let app = axum_test::TestServer::new(router(state.clone())).unwrap();

    app.post("/v1/messages")
        .add_header("x-api-key", AUTH)
        .json(&message_body("claude-sonnet-5"))
        .await
        .assert_status_ok();

    let snapshot = state.stats.snapshot(7);
    assert_eq!(snapshot.requests, 1);
    assert_eq!(snapshot.tokens(), 250);
    assert_eq!(snapshot.cached, 40);
}

#[tokio::test]
async fn failed_request_is_not_counted() {
    async fn boom() -> Response {
        (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "nope").into_response()
    }
    let state = seeded(
        "opencode-go/deepseek-v4.1-flash",
        CHAT_PKG,
        Router::new().route("/chat/completions", post(boom)),
    )
    .await;
    let app = axum_test::TestServer::new(router(state.clone())).unwrap();

    app.post("/v1/messages")
        .add_header("x-api-key", AUTH)
        .json(&message_body("claude-sonnet-5"))
        .await
        .assert_status(axum::http::StatusCode::INTERNAL_SERVER_ERROR);

    assert_eq!(state.stats.snapshot(7).requests, 0);
}

#[tokio::test]
async fn status_endpoint_reports_the_same_counters() {
    let state = seeded(
        "opencode-go/deepseek-v4.1-flash",
        CHAT_PKG,
        Router::new().route("/chat/completions", post(chat_upstream)),
    )
    .await;
    let app = axum_test::TestServer::new(router(state.clone())).unwrap();

    app.post("/v1/messages")
        .add_header("x-api-key", AUTH)
        .json(&message_body("claude-sonnet-5"))
        .await
        .assert_status_ok();

    let resp = app.get("/status").add_header("x-api-key", AUTH).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["window_days"], 7);
    assert_eq!(body["requests"], 1);
    assert_eq!(body["input"], 120);
    assert_eq!(body["output"], 30);
    assert_eq!(body["rows"][0]["gateway"], "claude-sonnet-5");
    assert_eq!(body["rows"][0]["ref"], "opencode-go/deepseek-v4.1-flash");

    // /status is behind the same credential as everything but /health.
    app.get("/status").await.assert_status_unauthorized();
}

#[tokio::test]
async fn stats_survive_a_file_round_trip() {
    use opencode_claude_gateway::stats::UsageStats;

    let dir = std::env::temp_dir().join(format!("ocg-stats-it-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ocg.stats.json");

    let state = seeded(
        "opencode-go/deepseek-v4.1-flash",
        CHAT_PKG,
        Router::new().route("/chat/completions", post(chat_upstream)),
    )
    .await;
    let live = UsageStats::open(path.clone());
    let state = state.with_stats(live.clone());
    let app = axum_test::TestServer::new(router(state)).unwrap();
    app.post("/v1/messages")
        .add_header("x-api-key", AUTH)
        .json(&message_body("claude-sonnet-5"))
        .await
        .assert_status_ok();
    live.flush().unwrap();

    // A restarted daemon loads the file and sees the same week.
    let reopened = UsageStats::open(path);
    let snapshot = reopened.snapshot(7);
    assert_eq!(snapshot.requests, 1);
    assert_eq!(snapshot.tokens(), 150);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn usage_counter_lock_is_not_poisoned_by_a_panicking_record() {
    // Recording must never take the daemon down: the mutex recovers from a
    // poisoned lock instead of unwrapping (a panic elsewhere can't disable
    // accounting).
    let state = seeded(
        "opencode-go/deepseek-v4.1-flash",
        CHAT_PKG,
        Router::new().route("/chat/completions", post(chat_upstream)),
    )
    .await;
    let stats = Arc::new(state.stats.clone());
    let cloned = stats.clone();
    let _ = std::thread::spawn(move || {
        cloned.record("claude-x", "opencode-go/x", 1, 1, 0);
        panic!("poison the lock");
    })
    .join();
    stats.record("claude-x", "opencode-go/x", 2, 3, 0);
    assert_eq!(stats.snapshot(7).requests, 2);
}
