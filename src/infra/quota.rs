//! Upstream quota probe for the `/usage` report: OpenCode Go only.
//!
//! Two read-only GETs, authenticated with the credential the gateway already
//! resolves for the `opencode` integration — the token never leaves this
//! module and never reaches a log line:
//!
//! - `GET https://opencode.ai/zen/go/v1/usage` — the rolling 5h, weekly and
//!   monthly windows (percent used, status, reset time).
//! - `GET https://opencode.ai/console/api/billing/status` — prepaid console
//!   credits in micro-cents (`availableMicroCents`). Best effort: the report
//!   is still useful without it.
//!
//! Failures are reported, never raised: the caller answers 200 with an
//! `error` row so `ocg /usage` and the status line can keep showing the last
//! known value instead of going blank.

use crate::infra::pair::OpenCodeClient;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{json, Value};
use std::time::Duration;

pub const OPENCODE_USAGE_URL: &str = "https://opencode.ai/zen/go/v1/usage";
pub const OPENCODE_BILLING_URL: &str = "https://opencode.ai/console/api/billing/status";

/// Endpoint override, read from the process env so tests (and a debugging
/// session against a mirror) never touch the real console.
fn usage_url() -> String {
    std::env::var("OCG_USAGE_URL").unwrap_or_else(|_| OPENCODE_USAGE_URL.to_string())
}

fn billing_url() -> String {
    std::env::var("OCG_BILLING_URL").unwrap_or_else(|_| OPENCODE_BILLING_URL.to_string())
}

/// Both endpoints are cheap; one shot per cache window (the report caches).
const QUOTA_HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// What the probe came to: the normalized payload, or the reason it could not
/// be read (`status` is the upstream HTTP code when there was one).
pub struct Probe {
    pub value: Value,
    pub error: Option<String>,
    pub status: Option<u16>,
}

impl Probe {
    fn ok(value: Value) -> Self {
        Self {
            value,
            error: None,
            status: None,
        }
    }

    fn err(message: impl Into<String>, status: Option<u16>) -> Self {
        Self {
            value: Value::Null,
            error: Some(message.into()),
            status,
        }
    }
}

/// Reads OpenCode Go quota with the credential the gateway already resolves.
pub struct QuotaProbe<'a> {
    opencode: &'a OpenCodeClient,
    http: &'a reqwest::Client,
}

impl<'a> QuotaProbe<'a> {
    pub fn new(opencode: &'a OpenCodeClient, http: &'a reqwest::Client) -> Self {
        Self { opencode, http }
    }

    /// Windows plus the best-effort console credits. A missing credential is
    /// an error row, not a failure: the endpoint answers what it knows.
    pub async fn opencode_go(&self) -> (Probe, Option<Value>) {
        let Some(secret) = self.opencode.credential_for("opencode").await else {
            return (
                Probe::err(
                    "no stored credential for 'opencode' (run `opencode auth login`)",
                    None,
                ),
                None,
            );
        };
        let probe = match self.get_json(&usage_url(), &secret).await {
            Ok(v) => match normalize_usage(&v) {
                Some(windows) if windows.as_object().is_some_and(|o| !o.is_empty()) => {
                    Probe::ok(json!({"windows": windows}))
                }
                _ => Probe::err("OpenCode Go returned no usage windows", None),
            },
            Err((msg, status)) => Probe::err(msg, status),
        };
        // Credits are independent: a billing failure keeps the windows.
        let credits = match self.get_json(&billing_url(), &secret).await {
            Ok(v) => normalize_credits(&v),
            Err(_) => None,
        };
        (probe, credits)
    }

    async fn get_json(
        &self,
        url: &str,
        secret: &SecretString,
    ) -> Result<Value, (String, Option<u16>)> {
        let resp = self
            .http
            .get(url)
            .timeout(QUOTA_HTTP_TIMEOUT)
            .bearer_auth(secret.expose_secret())
            .header("accept", "application/json")
            .header("user-agent", format!("ocg/{}", env!("CARGO_PKG_VERSION")))
            .send()
            .await
            .map_err(|e| (format!("GET {url}: {e}"), None))?;
        let status = resp.status();
        let code = status.as_u16();
        if !status.is_success() {
            return Err((failure_message(code, url), Some(code)));
        }
        resp.json::<Value>()
            .await
            .map_err(|e| (format!("GET {url}: invalid JSON: {e}"), Some(code)))
    }
}

/// Message per status, with the re-auth hint the user needs to act on.
fn failure_message(code: u16, url: &str) -> String {
    match code {
        401 => format!("HTTP 401 unauthorized — run `opencode auth login` again ({url})"),
        403 => format!("HTTP 403 forbidden — no Go plan on this credential ({url})"),
        429 => format!("HTTP 429 rate limited by the quota endpoint ({url})"),
        other => format!("HTTP {other} from {url}"),
    }
}

/// `{"usage":{"rolling":..,"weekly":..,"monthly":..}}` (or a bare `usage`)
/// with the fields the report keeps. Never invents a window.
fn normalize_usage(v: &Value) -> Option<Value> {
    let usage = v.get("usage").unwrap_or(v);
    let mut out = serde_json::Map::new();
    for key in ["rolling", "weekly", "monthly"] {
        let Some(row) = usage.get(key).filter(|r| r.is_object()) else {
            continue;
        };
        out.insert(
            key.to_string(),
            json!({
                "percent": row.get("percent").and_then(Value::as_f64),
                "status": row.get("status").and_then(Value::as_str),
                "resets_at": resets_at(row),
            }),
        );
    }
    Some(Value::Object(out))
}

/// Reset timestamp, accepting the spellings the endpoint has used
/// (`resetsAt`, `resets_at`, `resetAt`).
fn resets_at(row: &Value) -> Option<String> {
    for key in ["resetsAt", "resets_at", "resetAt"] {
        if let Some(s) = row.get(key) {
            match s {
                Value::String(s) if !s.is_empty() => return Some(s.clone()),
                Value::Number(n) => return Some(n.to_string()),
                _ => {}
            }
        }
    }
    None
}

/// Console credits in micro-cents (`availableMicroCents`), plus the limit.
/// The console sends both as *strings*, so accept either spelling.
fn normalize_credits(v: &Value) -> Option<Value> {
    let available = v
        .get("availableMicroCents")
        .or_else(|| v.get("balanceMicroCents"))
        .and_then(int_value)?;
    Some(json!({
        "available_micro_cents": available,
        "limit_micro_cents": v.get("creditLimitMicroCents").and_then(int_value),
    }))
}

/// An integer that may arrive as a JSON number or a numeric string.
fn int_value(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse::<i64>().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_keep_known_fields() {
        let v = json!({"usage": {
            "rolling": {"percent": 6.0, "status": "ok", "resetsAt": "2026-11-16T22:30:00Z"},
            "weekly": {"percent": 27.5, "resetsAt": 1763510400},
            "monthly": {"percent": 13}
        }});
        let out = normalize_usage(&v).expect("windows");
        assert_eq!(out["rolling"]["percent"], json!(6.0));
        assert_eq!(out["rolling"]["status"], json!("ok"));
        assert_eq!(out["rolling"]["resets_at"], json!("2026-11-16T22:30:00Z"));
        assert_eq!(out["weekly"]["resets_at"], json!("1763510400"));
        assert!(out["monthly"].get("resets_at").unwrap().is_null());
    }

    #[test]
    fn usage_accepts_bare_shape_and_rejects_empty() {
        let bare = json!({"rolling": {"percent": 1}});
        assert!(normalize_usage(&bare).unwrap().get("rolling").is_some());
        assert!(normalize_usage(&json!({"usage": {}}))
            .unwrap()
            .as_object()
            .unwrap()
            .is_empty());
        assert!(normalize_usage(&json!({"other": 1}))
            .unwrap()
            .as_object()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn credits_prefer_available_and_keep_limit() {
        // The console sends micro-cents as strings; accept numbers too.
        let v = json!({"availableMicroCents": "364914398", "creditLimitMicroCents": null});
        let out = normalize_credits(&v).expect("credits");
        assert_eq!(out["available_micro_cents"], json!(364_914_398));
        assert!(out["limit_micro_cents"].is_null());
        let numeric = json!({"availableMicroCents": 365, "creditLimitMicroCents": 1000});
        assert_eq!(
            normalize_credits(&numeric).unwrap()["available_micro_cents"],
            json!(365)
        );
        assert_eq!(
            normalize_credits(&json!({"balanceMicroCents": 7})).unwrap()["available_micro_cents"],
            json!(7),
            "balanceMicroCents is the legacy spelling"
        );
        assert!(normalize_credits(&json!({"mode": "prepaid"})).is_none());
    }

    #[test]
    fn failure_messages_are_actionable() {
        assert!(failure_message(401, "u").contains("opencode auth login"));
        assert!(failure_message(403, "u").contains("no Go plan"));
        assert!(failure_message(429, "u").contains("rate limited"));
        assert!(failure_message(500, "u").contains("500"));
    }
}
