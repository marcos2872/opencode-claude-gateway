//! OpenCode v2 pair API client.
//!
//! The gateway connects to the local OpenCode v2 server the same way the web
//! app and Desktop do:
//!
//! 1. `opencode api post /api/pair` (the CLI is already authenticated against
//!    the selected service) returns a short-lived, single-use pairing code.
//! 2. `GET /auth/connect/{code}` with `Accept: application/json` redeems it
//!    for a session token.
//! 3. Every API call then authenticates with HTTP Basic
//!    `opencode:<token>` (the same scheme the server advertises in
//!    `www-authenticate`).
//!
//! The token is cached in the gateway data dir (0600) and re-acquired on 401,
//! so pairing is a one-time cost per token lifetime. The catalog, provider
//! map and credentials come from `/api/model`, `/api/provider` and
//! `/api/credential` respectively; each has an offline fallback (CLI for the
//! catalog, SQLite for credentials) so a stopped server does not brick the
//! gateway.

use crate::config::AppConfig;
use crate::domain::CatalogEntry;
use crate::infra::opencode::{
    fetch_catalog_cli, parse_catalog_json, unwrap_envelope, CredentialStore,
};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};

/// Session / credential cache lifetimes. Providers change rarely; credentials
/// (OAuth access tokens) rotate, so keep that window short.
const PROVIDERS_TTL: Duration = Duration::from_secs(300);
const CREDENTIALS_TTL: Duration = Duration::from_secs(30);
const PAIR_HTTP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
struct Session {
    base_url: String,
    token: SecretString,
}

#[derive(Debug, Clone)]
struct Cached<T> {
    expires: Instant,
    value: T,
}

impl<T> Cached<T> {
    fn fresh(value: T, ttl: Duration) -> Self {
        Self {
            expires: Instant::now() + ttl,
            value,
        }
    }

    fn is_fresh(&self) -> bool {
        Instant::now() < self.expires
    }
}

#[derive(Debug, Clone, Default)]
struct ProviderInfo {
    integration_id: Option<String>,
}

#[derive(Debug)]
enum PairError {
    Unauthorized,
    Other(String),
}

impl std::fmt::Display for PairError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PairError::Unauthorized => write!(f, "pair session unauthorized"),
            PairError::Other(e) => write!(f, "{e}"),
        }
    }
}

/// Client for the OpenCode v2 HTTP API, scoped to what the gateway needs.
pub struct OpenCodeClient {
    bin: String,
    explicit_url: String,
    pair_enabled: bool,
    db_path: PathBuf,
    http: reqwest::Client,
    session: Mutex<Option<Session>>,
    providers: RwLock<Option<Cached<HashMap<String, ProviderInfo>>>>,
    credentials: RwLock<Option<Cached<HashMap<String, SecretString>>>>,
}

impl OpenCodeClient {
    pub fn new(config: &AppConfig, db_path: PathBuf) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(PAIR_HTTP_TIMEOUT)
            .user_agent(format!("ocg/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("pair http client");
        Self {
            bin: config.opencode_bin.clone(),
            explicit_url: config.opencode_url.trim().to_string(),
            pair_enabled: config.opencode_pair,
            db_path,
            http,
            session: Mutex::new(None),
            providers: RwLock::new(None),
            credentials: RwLock::new(None),
        }
    }

    /// Enabled model catalog. Prefers `/api/model` over the pair session and
    /// falls back to the CLI fetch when pairing is off or the server is down.
    pub async fn catalog(&self) -> Result<Vec<CatalogEntry>, String> {
        if self.pair_enabled {
            match self.get_json("/api/model").await {
                Ok(v) => return parse_catalog_json(v),
                Err(e) => tracing::warn!(
                    error = %e,
                    "pair API catalog fetch failed; falling back to `opencode api get /api/model`"
                ),
            }
        }
        let bin = self.bin.clone();
        tokio::task::spawn_blocking(move || fetch_catalog_cli(&bin))
            .await
            .map_err(|e| format!("catalog task failed: {e}"))?
    }

    /// Upstream bearer for a catalog entry.
    ///
    /// Resolution order:
    /// 1. `settings.apiKey` from the catalog row (inline key, also used by
    ///    tests and local providers).
    /// 2. Provider info (`/api/provider`) → integration id → active
    ///    credential from `/api/credential` (pair) or SQLite (fallback).
    /// 3. No credential: `Ok(None)` for a localhost provider (Ollama) or the
    ///    legacy `opencode` public key; `Err` otherwise, so the caller can
    ///    answer a clear 401 instead of forwarding an unauthenticated call.
    pub async fn bearer_for(&self, entry: &CatalogEntry) -> Result<Option<SecretString>, String> {
        if let Some(k) = entry.settings.api_key.as_ref().filter(|k| !k.is_empty()) {
            return Ok(Some(SecretString::from(k.clone())));
        }
        let providers = self.provider_map().await;
        let provider = providers.get(&entry.provider_id);
        let integration = provider
            .and_then(|p| p.integration_id.clone())
            .unwrap_or_else(|| default_integration(&entry.provider_id).to_string());
        if let Some(secret) = self.credential_for(&integration).await {
            return Ok(Some(secret));
        }
        if integration != entry.provider_id {
            if let Some(secret) = self.credential_for(&entry.provider_id).await {
                return Ok(Some(secret));
            }
        }
        // Local runtimes (Ollama) need no auth. A pair-disabled run has no
        // provider metadata, so only the well-known provider id qualifies;
        // everything else keeps the old "missing credential" 401.
        let no_auth_provider =
            entry.provider_id == "ollama" || provider.is_some_and(|p| p.integration_id.is_none());
        if no_auth_provider && is_local_url(entry.base_url()) {
            return Ok(None);
        }
        if entry.provider_id == "opencode" {
            // Legacy Console free-tier fallback. Those rows are hidden from
            // /v1/models unless `include_free_tier` is enabled.
            return Ok(Some(SecretString::from("public".to_string())));
        }
        Err(format!(
            "no stored credential for '{}' (run `opencode auth login`)",
            integration
        ))
    }

    // -- pair session ------------------------------------------------------

    async fn get_json(&self, path: &str) -> Result<Value, PairError> {
        let session = self.session().await.map_err(PairError::Other)?;
        match self.get_json_with(&session, path).await {
            Ok(v) => Ok(v),
            Err(PairError::Unauthorized) => {
                tracing::warn!("pair session rejected; pairing again");
                *self.session.lock().await = None;
                // Drop the on-disk token too: otherwise `pair()` would read
                // the same stale token back and loop on 401.
                let _ = std::fs::remove_file(session_file());
                let session = self.session().await.map_err(PairError::Other)?;
                self.get_json_with(&session, path).await
            }
            Err(e) => Err(e),
        }
    }

    async fn get_json_with(&self, session: &Session, path: &str) -> Result<Value, PairError> {
        let url = format!("{}{}", session.base_url.trim_end_matches('/'), path);
        let resp = self
            .http
            .get(&url)
            .basic_auth("opencode", Some(session.token.expose_secret()))
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| PairError::Other(format!("GET {path}: {e}")))?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(PairError::Unauthorized);
        }
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PairError::Other(format!(
                "GET {path}: HTTP {} {}",
                status.as_u16(),
                text.trim()
            )));
        }
        resp.json::<Value>()
            .await
            .map_err(|e| PairError::Other(format!("GET {path}: invalid JSON: {e}")))
    }

    async fn session(&self) -> Result<Session, String> {
        let mut guard = self.session.lock().await;
        if let Some(s) = guard.as_ref() {
            return Ok(s.clone());
        }
        let session = self.pair().await?;
        *guard = Some(session.clone());
        Ok(session)
    }

    /// Acquire a session token: cached file first, then the documented pair
    /// flow (create code through the CLI, redeem it over HTTP).
    async fn pair(&self) -> Result<Session, String> {
        let base_url = self.resolve_server_url()?;
        if let Some(cached) = read_cached_session(&base_url) {
            tracing::debug!(url = %base_url, "using cached pair session");
            return Ok(cached);
        }
        let code =
            tokio::time::timeout(Duration::from_secs(20), self.create_pairing_code(&base_url))
                .await
                .map_err(|_| "timed out creating a pairing code".to_string())??;
        let url = format!("{}/auth/connect/{}", base_url.trim_end_matches('/'), code);
        let resp = self
            .http
            .get(&url)
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| format!("pair redeem failed: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("pair redeem failed: HTTP {status} {}", text.trim()));
        }
        let v: Value = resp
            .json()
            .await
            .map_err(|e| format!("pair redeem returned invalid JSON: {e}"))?;
        let token = v
            .get("token")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| "pair redeem returned no token".to_string())?;
        let session = Session {
            base_url,
            token: SecretString::from(token.to_string()),
        };
        write_cached_session(&session);
        tracing::info!(url = %session.base_url, "paired with OpenCode v2 service");
        Ok(session)
    }

    async fn create_pairing_code(&self, base_url: &str) -> Result<String, String> {
        let bin = self.bin.clone();
        // The CLI reads the selected service's stored password when no
        // `--server` is given. Only pin the URL when our target differs from
        // that service (e.g. `opencode serve --standalone`); in that case the
        // CLI needs `OPENCODE_PASSWORD` in the environment, which it inherits.
        let pin = state_service_url()
            .map(|u| u.trim_end_matches('/') != base_url.trim_end_matches('/'))
            .unwrap_or(false);
        let url = base_url.to_string();
        let out = tokio::task::spawn_blocking(move || {
            let mut cmd = Command::new(&bin);
            cmd.arg("api");
            if pin {
                cmd.arg("--server").arg(&url);
            }
            cmd.args(["post", "/api/pair"]).output()
        })
        .await
        .map_err(|e| format!("pair task failed: {e}"))?
        .map_err(|e| format!("failed to run `{} api post /api/pair`: {e}", self.bin))?;
        if !out.status.success() {
            return Err(format!(
                "`opencode api post /api/pair` failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let v: Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("invalid /api/pair JSON: {e}"))?;
        v.get("code")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty())
            .map(str::to_string)
            .ok_or_else(|| "`/api/pair` returned no code".to_string())
    }

    fn resolve_server_url(&self) -> Result<String, String> {
        if !self.explicit_url.is_empty() {
            return Ok(self.explicit_url.clone());
        }
        if let Ok(url) = std::env::var("OCG_OPENCODE_URL") {
            if !url.trim().is_empty() {
                return Ok(url.trim().to_string());
            }
        }
        if let Some(url) = state_service_url() {
            return Ok(url);
        }
        // Last resort: ask the CLI (it knows the running service and its
        // auth). `/api/info` carries the advertised URLs.
        let out = Command::new(&self.bin)
            .args(["api", "get", "/api/info"])
            .output()
            .map_err(|e| format!("cannot discover OpenCode server URL: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "cannot discover OpenCode server URL: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let v: Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("invalid /api/info JSON: {e}"))?;
        v.get("urls")
            .and_then(Value::as_array)
            .and_then(|urls| urls.first())
            .and_then(Value::as_str)
            .filter(|u| !u.is_empty())
            .map(str::to_string)
            .ok_or_else(|| "OpenCode /api/info returned no URL".to_string())
    }

    // -- provider / credential snapshots ----------------------------------

    async fn provider_map(&self) -> HashMap<String, ProviderInfo> {
        if !self.pair_enabled {
            return HashMap::new();
        }
        {
            let guard = self.providers.read().await;
            if let Some(c) = guard.as_ref() {
                if c.is_fresh() {
                    return c.value.clone();
                }
            }
        }
        let (map, ttl) = match self.get_json("/api/provider").await {
            Ok(v) => (parse_providers(&v), PROVIDERS_TTL),
            Err(e) => {
                tracing::warn!(error = %e, "pair API provider fetch failed");
                // Retry soon: while this snapshot is empty, provider →
                // integration mapping falls back to the provider id, which is
                // wrong for `opencode-go` (integration `opencode`).
                (HashMap::new(), CREDENTIALS_TTL)
            }
        };
        *self.providers.write().await = Some(Cached::fresh(map.clone(), ttl));
        map
    }

    async fn credential_for(&self, integration_id: &str) -> Option<SecretString> {
        if self.pair_enabled {
            if let Some(secret) = self.credential_map().await.get(integration_id).cloned() {
                return Some(secret);
            }
        }
        CredentialStore::new(self.db_path.clone()).get(integration_id)
    }

    async fn credential_map(&self) -> HashMap<String, SecretString> {
        {
            let guard = self.credentials.read().await;
            if let Some(c) = guard.as_ref() {
                if c.is_fresh() {
                    return c.value.clone();
                }
            }
        }
        let map = match self.get_json("/api/credential").await {
            Ok(v) => parse_credentials(&v),
            Err(e) => {
                tracing::warn!(error = %e, "pair API credential fetch failed");
                HashMap::new()
            }
        };
        *self.credentials.write().await = Some(Cached::fresh(map.clone(), CREDENTIALS_TTL));
        map
    }
}

/// Provider → integration fallback used when `/api/provider` is unavailable
/// (pair disabled or server down). v2.3 serves both the free `opencode` tier
/// and `opencode-go` inference from the same integration credential.
fn default_integration(provider_id: &str) -> &str {
    match provider_id {
        "opencode-go" => "opencode",
        other => other,
    }
}

fn is_local_url(base: Option<&str>) -> bool {
    let Some(base) = base else {
        return false;
    };
    let lower = base.to_ascii_lowercase();
    lower.contains("127.0.0.1") || lower.contains("localhost") || lower.contains("[::1]")
}

/// Extract `integrationID` from `/api/provider` rows.
fn parse_providers(v: &Value) -> HashMap<String, ProviderInfo> {
    let mut out = HashMap::new();
    if let Some(rows) = v.get("data").and_then(Value::as_array) {
        for row in rows {
            let Some(id) = row.get("id").and_then(Value::as_str) else {
                continue;
            };
            out.insert(
                id.to_string(),
                ProviderInfo {
                    integration_id: row
                        .get("integrationID")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                },
            );
        }
    }
    out
}

/// Build `integrationID -> secret` from `/api/credential`, preferring active
/// rows. Credential values may be the v2 envelope (`{"type","access","key"}`)
/// or a bare string.
fn parse_credentials(v: &Value) -> HashMap<String, SecretString> {
    let mut out: HashMap<String, SecretString> = HashMap::new();
    if let Some(rows) = v.get("data").and_then(Value::as_array) {
        for row in rows {
            let Some(integration) = row.get("integrationID").and_then(Value::as_str) else {
                continue;
            };
            let active = row.get("active").and_then(Value::as_bool).unwrap_or(true);
            if !active && out.contains_key(integration) {
                continue;
            }
            if let Some(secret) = row.get("value").and_then(secret_from_value) {
                out.insert(integration.to_string(), secret);
            }
        }
    }
    out
}

fn secret_from_value(v: &Value) -> Option<SecretString> {
    match v {
        Value::String(s) if !s.is_empty() => Some(SecretString::from(unwrap_envelope(s))),
        Value::Object(_) => {
            for field in ["access", "key"] {
                if let Some(s) = v
                    .get(field)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    return Some(SecretString::from(s.to_string()));
                }
            }
            None
        }
        _ => None,
    }
}

// -- session persistence ---------------------------------------------------

fn session_file() -> PathBuf {
    // Env override keeps integration tests away from the real state dir.
    if let Ok(p) = std::env::var("OCG_PAIR_SESSION") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    AppConfig::data_dir().join("ocg.pair")
}

fn read_cached_session(base_url: &str) -> Option<Session> {
    let text = std::fs::read_to_string(session_file()).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let cached_url = v.get("url").and_then(Value::as_str)?;
    if cached_url.trim_end_matches('/') != base_url.trim_end_matches('/') {
        return None;
    }
    let token = v
        .get("token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())?;
    Some(Session {
        base_url: cached_url.to_string(),
        token: SecretString::from(token.to_string()),
    })
}

fn write_cached_session(session: &Session) {
    let path = session_file();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = serde_json::json!({
        "url": session.base_url,
        "token": session.token.expose_secret(),
    });
    if let Err(e) = crate::daemon::write_private(path, &body.to_string()) {
        // A read-only data dir must not break requests: the in-memory session
        // still works for this process.
        tracing::warn!(error = %e, "could not persist pair session");
    }
}

/// URL advertised by the selected OpenCode service
/// (`~/.local/state/opencode/service.json`, 0600).
fn state_service_url() -> Option<String> {
    let path = dirs::state_dir()?.join("opencode").join("service.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    v.get("url")
        .and_then(Value::as_str)
        .filter(|u| !u.is_empty())
        .map(str::to_string)
}

/// Shared handle used by `AppState`.
pub type SharedOpenCode = Arc<OpenCodeClient>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn provider_integration_from_catalog_payload() {
        let v = json!({
            "data": [
                {"id": "opencode-go", "integrationID": "opencode"},
                {"id": "github-copilot", "integrationID": "github-copilot"},
                {"id": "ollama"}
            ]
        });
        let map = parse_providers(&v);
        assert_eq!(
            map["opencode-go"].integration_id.as_deref(),
            Some("opencode")
        );
        assert_eq!(
            map["github-copilot"].integration_id.as_deref(),
            Some("github-copilot")
        );
        assert!(map["ollama"].integration_id.is_none());
    }

    #[test]
    fn credentials_use_v2_envelope_and_prefer_active() {
        let v = json!({
            "data": [
                {"integrationID": "opencode", "active": false,
                 "value": {"type": "oauth", "access": "stale"}},
                {"integrationID": "opencode", "active": true,
                 "value": {"type": "oauth", "refresh": "r", "access": "fresh"}},
                {"integrationID": "github-copilot", "active": true,
                 "value": {"type": "key", "key": "gho_key"}}
            ]
        });
        let map = parse_credentials(&v);
        assert_eq!(map["opencode"].expose_secret(), "fresh", "active row wins");
        assert_eq!(map["github-copilot"].expose_secret(), "gho_key");
    }

    #[test]
    fn credential_value_accepts_plain_string() {
        let v = json!({
            "data": [
                {"integrationID": "plain", "active": true,
                 "value": "{\"type\":\"key\",\"key\":\"sk-plain\"}"}
            ]
        });
        let map = parse_credentials(&v);
        assert_eq!(map["plain"].expose_secret(), "sk-plain");
    }

    #[test]
    fn offline_integration_mapping_covers_go_tier() {
        assert_eq!(default_integration("opencode-go"), "opencode");
        assert_eq!(default_integration("opencode"), "opencode");
        assert_eq!(default_integration("github-copilot"), "github-copilot");
    }

    #[test]
    fn local_urls_need_no_credential() {
        assert!(is_local_url(Some("http://127.0.0.1:11434/v1")));
        assert!(is_local_url(Some("http://localhost:11434/v1")));
        assert!(is_local_url(Some("http://[::1]:8080")));
        assert!(!is_local_url(Some("https://opencode.ai/inference/go")));
        assert!(!is_local_url(None));
    }
}
