//! E2E do pareamento com o OpenCode v2 (`POST /api/pair` +
//! `/auth/connect/:code`) e do consumo da API HTTP pareada: catálogo,
//! providers e credenciais. O binário `opencode` é um shim que só devolve o
//! código de pareamento; o servidor v2 é um mock local.
//!
//! Arquivo autocontido de propósito (mesma regra de `cache_usage.rs`).

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use opencode_claude_gateway::api::server::AppState;
use opencode_claude_gateway::config::AppConfig;
use secrecy::ExposeSecret;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

/// `base64("opencode:paired-token")` / `base64("opencode:stale")`.
const EXPECTED_BASIC: &str = "b3BlbmNvZGU6cGFpcmVkLXRva2Vu";
const STALE_BASIC: &str = "b3BlbmNvZGU6c3RhbGU=";

/// `OCG_PAIR_SESSION` is process-wide: serialize the tests that change it.
static PAIR_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone, Default)]
struct PairMock {
    auth: Arc<Mutex<Vec<Option<String>>>>,
    connect_codes: Arc<Mutex<Vec<String>>>,
    providers_fail: bool,
    catalog_fail: bool,
}

fn expected_basic() -> String {
    format!("Basic {EXPECTED_BASIC}")
}

fn require_basic(headers: &HeaderMap, mock: &PairMock) -> bool {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let ok = auth.as_deref() == Some(expected_basic().as_str());
    if let Ok(mut a) = mock.auth.try_lock() {
        a.push(auth);
    }
    ok
}

async fn connect(State(mock): State<PairMock>, Path(code): Path<String>) -> Json<Value> {
    mock.connect_codes.lock().await.push(code);
    Json(json!({"token": "paired-token"}))
}

async fn models(State(mock): State<PairMock>, headers: HeaderMap) -> Response {
    if !require_basic(&headers, &mock) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"_tag": "UnauthorizedError", "message": "Authentication required"})),
        )
            .into_response();
    }
    if mock.catalog_fail {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "catalog unavailable"})),
        )
            .into_response();
    }
    Json(catalog_entry_value()).into_response()
}

async fn providers(State(mock): State<PairMock>, headers: HeaderMap) -> Response {
    if !require_basic(&headers, &mock) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if mock.providers_fail {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "providers unavailable"})),
        )
            .into_response();
    }
    Json(json!({
        "data": [
            {"id": "opencode-go", "integrationID": "opencode"},
            {"id": "ollama", "integrationID": null}
        ]
    }))
    .into_response()
}

async fn credentials(State(mock): State<PairMock>, headers: HeaderMap) -> Response {
    if !require_basic(&headers, &mock) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({
        "data": [{
            "id": "cred_1",
            "integrationID": "opencode",
            "label": "Default",
            "active": true,
            "value": {"type": "oauth", "access": "paired-upstream-key"}
        }]
    }))
    .into_response()
}

async fn spawn_pair_server() -> (String, PairMock) {
    spawn_pair_server_with(PairMock::default()).await
}

async fn spawn_pair_server_with(mock: PairMock) -> (String, PairMock) {
    let app = Router::new()
        .route("/auth/connect/:code", get(connect))
        .route("/api/model", get(models))
        .route("/api/provider", get(providers))
        .route("/api/credential", get(credentials))
        // The real server's pair code is created through the CLI; the shim
        // below answers that call, so this route only documents the shape.
        .route(
            "/api/pair",
            post(|| async { Json(json!({"code": "unused"})) }),
        )
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), mock)
}

/// Shim for `opencode api post /api/pair`: prints the pairing code.
fn write_pair_shim(dir: &std::path::Path) -> String {
    write_shim(dir, None)
}

/// Shim that also answers the offline catalog fallback
/// (`opencode api get /api/model`) with `catalog`.
fn write_pair_shim_with_catalog(dir: &std::path::Path, catalog: &Value) -> String {
    write_shim(dir, Some(catalog))
}

fn write_shim(dir: &std::path::Path, catalog: Option<&Value>) -> String {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join("opencode");
    let catalog_arm = match catalog {
        Some(c) => format!("  *\"/api/model\"*) cat <<'EOM'\n{c}\nEOM\n    ;;\n"),
        None => String::new(),
    };
    let script = format!(
        "#!/bin/sh\n\
         case \"$*\" in\n\
         \x20 *\"/api/pair\"*) echo '{{\"code\":\"pair-code-1\",\"expires_in\":300}}' ;;\n\
         {catalog_arm}\
         \x20 *) exit 1 ;;\n\
         esac\n"
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    path.to_string_lossy().to_string()
}

/// Serialize tests that mutate `OCG_PAIR_SESSION`, return the temp session
/// path and a ready-to-use lock guard.
async fn pair_session() -> (tokio::sync::MutexGuard<'static, ()>, PathBuf) {
    let guard = PAIR_ENV_LOCK.lock().await;
    let session = std::env::temp_dir().join(format!(
        "ocg-pair-session-{}-{:?}.json",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&session);
    std::env::set_var("OCG_PAIR_SESSION", &session);
    (guard, session)
}

fn catalog_entry_value() -> Value {
    json!({
        "data": [{
            "id": "kimi-k2.7-code",
            "modelID": "kimi-k2.7-code",
            "providerID": "opencode-go",
            "name": "Kimi K2.7 Code",
            "package": "@opencode/ai/providers/openai-compatible",
            "settings": {"baseURL": "https://example.invalid/go/openai/v1"},
            "enabled": true,
            "variants": []
        }]
    })
}

fn config(base: &str, bin: &str) -> AppConfig {
    AppConfig {
        opencode_pair: true,
        opencode_url: base.to_string(),
        opencode_bin: bin.to_string(),
        ..AppConfig::default()
    }
}

#[tokio::test]
async fn pair_flow_reads_catalog_providers_and_credentials() {
    let (_env_guard, session) = pair_session().await;

    let (base, mock) = spawn_pair_server().await;
    let shim_dir = std::env::temp_dir().join(format!(
        "ocg-pair-shim-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let bin = write_pair_shim(&shim_dir);

    let state = AppState::new(
        config(&base, &bin),
        PathBuf::from("/nonexistent-ocg-test/opencode.db"),
    );
    let n = state.refresh().await.expect("catalog via pair API");
    assert_eq!(n, 1);
    let aliases = state.aliases.read().await;
    assert!(
        aliases
            .iter()
            .any(|a| a.opencode_ref == "opencode-go/kimi-k2.7-code"),
        "{aliases:?}"
    );
    drop(aliases);

    // Pair code came from the CLI shim; the mock recorded the redemption.
    assert_eq!(
        mock.connect_codes.lock().await.as_slice(),
        &["pair-code-1".to_string()]
    );
    // Both metadata requests used Basic `opencode:<pair-token>`.
    let auth = mock.auth.lock().await;
    assert!(!auth.is_empty(), "no authenticated API calls");
    let expected = expected_basic();
    assert!(
        auth.iter().all(|a| a.as_deref() == Some(expected.as_str())),
        "{auth:?}"
    );
    drop(auth);

    // Provider `opencode-go` maps to integration `opencode`, whose active
    // credential becomes the upstream bearer.
    let entry = state
        .catalog
        .read()
        .await
        .iter()
        .find(|e| e.qualified() == "opencode-go/kimi-k2.7-code")
        .cloned()
        .expect("catalog entry");
    let bearer = state
        .bearer_for(&entry)
        .await
        .expect("credential lookup")
        .expect("credential present");
    assert_eq!(bearer.expose_secret(), "paired-upstream-key");

    // Session token is cached 0600 for the next run.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&session).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{}", session.display());
    }
    let _ = std::fs::remove_file(&session);
    let _ = std::fs::remove_dir_all(&shim_dir);
}

#[tokio::test]
async fn stale_pair_token_is_renewed_on_401() {
    let (_env_guard, session) = pair_session().await;
    let (base, mock) = spawn_pair_server().await;
    // Simulate a token that the server no longer accepts (expired/revoked).
    std::fs::write(&session, json!({"url": base, "token": "stale"}).to_string()).unwrap();
    let shim_dir = std::env::temp_dir().join(format!(
        "ocg-pair-stale-shim-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let bin = write_pair_shim(&shim_dir);

    let state = AppState::new(
        config(&base, &bin),
        PathBuf::from("/nonexistent-ocg-test/opencode.db"),
    );
    let n = state.refresh().await.expect("re-pair after 401");
    assert_eq!(n, 1);

    let auth = mock.auth.lock().await;
    let stale = format!("Basic {STALE_BASIC}");
    let expected = expected_basic();
    assert!(
        auth.iter().any(|a| a.as_deref() == Some(stale.as_str())),
        "stale token was not tried first: {auth:?}"
    );
    assert!(
        auth.iter().any(|a| a.as_deref() == Some(expected.as_str())),
        "re-paired token was not used: {auth:?}"
    );
    drop(auth);
    // One pairing code, one redemption: the 401 was recovered in place.
    assert_eq!(mock.connect_codes.lock().await.len(), 1);
    // The on-disk cache now holds the new token (not the stale one).
    let cached: Value = serde_json::from_str(&std::fs::read_to_string(&session).unwrap()).unwrap();
    assert_eq!(cached["token"], "paired-token");

    let _ = std::fs::remove_file(&session);
    let _ = std::fs::remove_dir_all(&shim_dir);
}

#[tokio::test]
async fn provider_metadata_failure_falls_back_to_default_integration() {
    let (_env_guard, session) = pair_session().await;
    let (base, _mock) = spawn_pair_server_with(PairMock {
        providers_fail: true,
        ..PairMock::default()
    })
    .await;
    let shim_dir = std::env::temp_dir().join(format!(
        "ocg-pair-providers-shim-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let bin = write_pair_shim(&shim_dir);
    let state = AppState::new(
        config(&base, &bin),
        PathBuf::from("/nonexistent-ocg-test/opencode.db"),
    );
    state.refresh().await.expect("catalog");

    let entry = state
        .catalog
        .read()
        .await
        .iter()
        .find(|e| e.qualified() == "opencode-go/kimi-k2.7-code")
        .cloned()
        .expect("catalog entry");
    // `/api/provider` failed, so the static v2.3 mapping
    // (opencode-go -> opencode) must still find the credential.
    let bearer = state
        .bearer_for(&entry)
        .await
        .expect("credential lookup")
        .expect("credential present");
    assert_eq!(bearer.expose_secret(), "paired-upstream-key");

    let _ = std::fs::remove_file(&session);
    let _ = std::fs::remove_dir_all(&shim_dir);
}

#[tokio::test]
async fn pair_catalog_failure_falls_back_to_cli() {
    let (_env_guard, session) = pair_session().await;
    let (base, mock) = spawn_pair_server_with(PairMock {
        catalog_fail: true,
        ..PairMock::default()
    })
    .await;
    let shim_dir = std::env::temp_dir().join(format!(
        "ocg-pair-catalog-shim-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let bin = write_pair_shim_with_catalog(&shim_dir, &catalog_entry_value());
    let state = AppState::new(
        config(&base, &bin),
        PathBuf::from("/nonexistent-ocg-test/opencode.db"),
    );
    let n = state.refresh().await.expect("CLI catalog fallback");
    assert_eq!(n, 1);
    assert!(state
        .aliases
        .read()
        .await
        .iter()
        .any(|a| a.opencode_ref == "opencode-go/kimi-k2.7-code"));
    // Pairing itself succeeded; only the catalog endpoint failed.
    assert_eq!(mock.connect_codes.lock().await.len(), 1);

    let _ = std::fs::remove_file(&session);
    let _ = std::fs::remove_dir_all(&shim_dir);
}

#[tokio::test]
async fn local_provider_without_integration_needs_no_bearer() {
    // Pair disabled: no HTTP, no metadata — only the well-known local id.
    let state = AppState::new(
        AppConfig {
            opencode_pair: false,
            ..AppConfig::default()
        },
        PathBuf::from("/nonexistent-ocg-test/opencode.db"),
    );
    let mut entry = opencode_claude_gateway::domain::CatalogEntry {
        id: "qwen3.5:4b".to_string(),
        model_id: "qwen3.5:4b".to_string(),
        provider_id: "ollama".to_string(),
        name: "Qwen".to_string(),
        package: "@opencode/ai/providers/openai-compatible".to_string(),
        settings: opencode_claude_gateway::domain::CatalogSettings {
            base_url: Some("http://127.0.0.1:11434/v1".to_string()),
            ..Default::default()
        },
        limit: None,
        enabled: true,
        variants: vec![],
        headers: None,
        body: None,
    };
    assert!(state.bearer_for(&entry).await.unwrap().is_none());
    // A remote provider with no credential still fails loudly.
    entry.provider_id = "opencode-go".to_string();
    entry.settings.base_url = Some("https://example.invalid/v1".to_string());
    assert!(state.bearer_for(&entry).await.is_err());
}
