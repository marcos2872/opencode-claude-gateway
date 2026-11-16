//! E2E de `GET /usage`: a cota do OpenCode Go (janelas de 5h/semanal/mensal +
//! créditos do console) lida pela credencial que o gateway já resolve.
//!
//! O upstream é um mock em 127.0.0.1 (as URLs reais são trocadas por
//! `OCG_USAGE_URL`/`OCG_BILLING_URL`, lidas do ambiente do processo) e a
//! credencial vem de um SQLite temporário com a tabela `credential`, o mesmo
//! fallback offline de `infra::opencode::CredentialStore`. Sem binário
//! `opencode` e sem rede.
//!
//! Arquivo autocontido de propósito (mesma regra de `pair.rs` e
//! `cache_usage.rs`).

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use opencode_claude_gateway::api::server::{router, AppState};
use opencode_claude_gateway::config::AppConfig;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

const TOKEN: &str = "test-secret";
const GO_TOKEN: &str = "go-oauth-token";

/// `OCG_USAGE_URL` / `OCG_BILLING_URL` are process-wide: serialize the tests
/// that point them at a mock.
static URL_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn test_config() -> AppConfig {
    AppConfig {
        auth_token: TOKEN.to_string(),
        // Offline path: `/usage` resolves the credential straight from SQLite.
        opencode_pair: false,
        ..AppConfig::default()
    }
}

// ---------------------------------------------------------------------------
// Mock do console (usage + billing).
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ConsoleMock {
    /// Status answered by `/usage` (`200` = the happy path).
    usage_status: u16,
    usage_body: Value,
    billing_status: u16,
    billing_body: Value,
    /// Bearer seen by each endpoint, so the test can assert it was sent.
    seen: Arc<Mutex<Vec<String>>>,
    usage_calls: Arc<Mutex<usize>>,
}

impl Default for ConsoleMock {
    fn default() -> Self {
        Self {
            usage_status: 200,
            usage_body: json!({"usage": {
                "rolling": {"percent": 6.0, "status": "ok", "resetsAt": "2026-11-16T22:30:00Z"},
                "weekly": {"percent": 27.5, "resetsAt": 1763510400_u64},
                "monthly": {"percent": 13.0}
            }}),
            billing_status: 200,
            billing_body: json!({"availableMicroCents": 365, "creditLimitMicroCents": 1000}),
            seen: Arc::new(Mutex::new(vec![])),
            usage_calls: Arc::new(Mutex::new(0)),
        }
    }
}

fn bearer(headers: &HeaderMap) -> String {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

async fn usage(State(mock): State<ConsoleMock>, headers: HeaderMap) -> Response {
    *mock.usage_calls.lock().await += 1;
    mock.seen.lock().await.push(bearer(&headers));
    if mock.usage_status != 200 {
        return (StatusCode::from_u16(mock.usage_status).unwrap(), "nope").into_response();
    }
    Json(mock.usage_body.clone()).into_response()
}

async fn billing(State(mock): State<ConsoleMock>, headers: HeaderMap) -> Response {
    mock.seen.lock().await.push(bearer(&headers));
    if mock.billing_status != 200 {
        return (StatusCode::from_u16(mock.billing_status).unwrap(), "nope").into_response();
    }
    Json(mock.billing_body.clone()).into_response()
}

/// Spawns the mock and points both override vars at it. Returns the guard the
/// caller must hold for the whole test (env is process-wide).
async fn mock_console(mock: ConsoleMock) -> (tokio::sync::MutexGuard<'static, ()>, ConsoleMock) {
    let guard = URL_ENV_LOCK.lock().await;
    let app = Router::new()
        .route("/usage", get(usage))
        .route("/billing", get(billing))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    std::env::set_var("OCG_USAGE_URL", format!("http://{addr}/usage"));
    std::env::set_var("OCG_BILLING_URL", format!("http://{addr}/billing"));
    (guard, mock)
}

// ---------------------------------------------------------------------------
// Credencial em SQLite (o fallback offline do gateway).
// ---------------------------------------------------------------------------

/// Writes a `credential` row the way OpenCode v2 stores it: an OAuth envelope
/// whose `access` field carries the token.
fn write_credential_db(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let db = dir.join("opencode.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE credential (
             integration_id TEXT NOT NULL,
             value TEXT NOT NULL,
             active INTEGER
         );",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO credential (integration_id, value, active) VALUES (?1, ?2, 1)",
        rusqlite::params![
            "opencode",
            format!("{{\"type\":\"oauth\",\"access\":\"{GO_TOKEN}\"}}")
        ],
    )
    .unwrap();
    db
}

fn temp_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "ocg-usage-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ))
}

fn state_with_db(db: PathBuf) -> AppState {
    AppState::new(test_config(), db)
}

// ---------------------------------------------------------------------------
// Testes.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn usage_reports_go_windows_and_credits() {
    let (_guard, mock) = mock_console(ConsoleMock::default()).await;
    let db = write_credential_db(&temp_dir("ok"));
    let server = axum_test::TestServer::new(router(state_with_db(db))).unwrap();

    let resp = server.get("/usage").add_header("x-api-key", TOKEN).await;
    assert_eq!(resp.status_code(), 200);
    let body: Value = resp.json();

    assert_eq!(body["provider"], "opencode-go");
    assert_eq!(body["error"], Value::Null);
    assert_eq!(body["windows"]["rolling"]["percent"], 6.0);
    assert_eq!(
        body["windows"]["rolling"]["resets_at"],
        "2026-11-16T22:30:00Z"
    );
    assert_eq!(body["windows"]["weekly"]["percent"], 27.5);
    assert_eq!(body["windows"]["monthly"]["percent"], 13.0);
    assert_eq!(body["credits"]["available_micro_cents"], 365);
    assert_eq!(body["credits"]["limit_micro_cents"], 1000);
    assert!(body["fetched_at"].as_u64().unwrap() > 0);

    // The Go credential reached both endpoints as a bearer token.
    let seen = mock.seen.lock().await;
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0], format!("Bearer {GO_TOKEN}"));
    assert_eq!(seen[1], format!("Bearer {GO_TOKEN}"));
}

#[tokio::test]
async fn usage_is_cached_between_polls() {
    let (_guard, mock) = mock_console(ConsoleMock::default()).await;
    let db = write_credential_db(&temp_dir("cache"));
    let server = axum_test::TestServer::new(router(state_with_db(db))).unwrap();

    for _ in 0..3 {
        let resp = server.get("/usage").add_header("x-api-key", TOKEN).await;
        assert_eq!(resp.status_code(), 200);
    }
    assert_eq!(
        *mock.usage_calls.lock().await,
        1,
        "three polls inside the TTL share one upstream call"
    );
}

#[tokio::test]
async fn usage_keeps_windows_when_billing_fails() {
    let (_guard, _mock) = mock_console(ConsoleMock {
        billing_status: 500,
        ..ConsoleMock::default()
    })
    .await;
    let db = write_credential_db(&temp_dir("billing"));
    let server = axum_test::TestServer::new(router(state_with_db(db))).unwrap();

    let body: Value = server
        .get("/usage")
        .add_header("x-api-key", TOKEN)
        .await
        .json();
    assert_eq!(body["error"], Value::Null, "credits are best effort");
    assert_eq!(body["windows"]["rolling"]["percent"], 6.0);
    assert_eq!(body["credits"], Value::Null);
}

#[tokio::test]
async fn usage_reports_an_expired_credential() {
    let (_guard, _mock) = mock_console(ConsoleMock {
        // An expired credential is rejected by both endpoints.
        usage_status: 401,
        billing_status: 401,
        ..ConsoleMock::default()
    })
    .await;
    let db = write_credential_db(&temp_dir("401"));
    let server = axum_test::TestServer::new(router(state_with_db(db))).unwrap();

    let resp = server.get("/usage").add_header("x-api-key", TOKEN).await;
    assert_eq!(
        resp.status_code(),
        200,
        "a quota failure is not a gateway error"
    );
    let body: Value = resp.json();
    assert_eq!(body["status"], 401);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("opencode auth login"));
    assert_eq!(body["windows"], Value::Null);
    assert_eq!(body["credits"], Value::Null);
}

#[tokio::test]
async fn usage_without_a_credential_points_at_auth_login() {
    let (_guard, _mock) = mock_console(ConsoleMock::default()).await;
    let server = axum_test::TestServer::new(router(state_with_db(PathBuf::from(
        "/nonexistent-ocg-usage-test/opencode.db",
    ))))
    .unwrap();

    let body: Value = server
        .get("/usage")
        .add_header("x-api-key", TOKEN)
        .await
        .json();
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("opencode auth login"));
    assert_eq!(body["windows"], Value::Null);
}

#[tokio::test]
async fn usage_requires_the_gateway_token() {
    let (_guard, _mock) = mock_console(ConsoleMock::default()).await;
    let db = write_credential_db(&temp_dir("auth"));
    let state = state_with_db(db);
    let server = axum_test::TestServer::new(router(state)).unwrap();

    let resp = server.get("/usage").await;
    assert_eq!(resp.status_code(), 401);
    let body: Value = resp.json();
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "authentication_error");
}
