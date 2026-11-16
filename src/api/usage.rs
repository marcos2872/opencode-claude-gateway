//! `GET /usage`: the OpenCode Go quota the local Claude Code mods poll.
//!
//! The gateway is the only process that already knows how to resolve the
//! `opencode` integration credential (pair session first, SQLite fallback), so
//! the mod never reads `opencode.db` and never sees the token: it asks
//! `127.0.0.1:<port>/usage` with its ordinary gateway credential
//! (`require_token` guards every route but `/health`).
//!
//! The upstream probes are cached for [`USAGE_TTL`] so a status line polling
//! every minute cannot hammer the quota endpoint. Failures are cached too, for
//! a shorter window, and always answered `200` with an `error` row: the mod
//! keeps showing its last known value instead of going blank.

use super::state::AppState;
use crate::infra::quota::QuotaProbe;
use axum::{extract::State, response::IntoResponse, Json};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Successful answers are reused for this long. The quota windows move in
/// hours, so a minute is generous; the mod's own poll is slower than this.
pub const USAGE_TTL: Duration = Duration::from_secs(60);
/// A failed probe is retried sooner than a successful one, so a credential
/// fixed in `opencode auth login` shows up within the next poll or two.
pub const USAGE_ERROR_TTL: Duration = Duration::from_secs(15);

/// Cached report plus the moment it stops being served. Cheap to clone
/// (shared inner state), because `AppState` is cloned per request.
#[derive(Clone, Default)]
pub struct UsageCache {
    slot: Arc<Mutex<Option<(Instant, Value)>>>,
}

impl UsageCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Serve the cached report when fresh, else probe upstream once. The lock
    /// is held across the probe on purpose: concurrent polls of the status
    /// line and a pane share one upstream call, and the endpoint is one
    /// caller's worth of load.
    pub async fn report(&self, s: &AppState) -> Value {
        let mut slot = self.slot.lock().await;
        if let Some((expires, value)) = slot.as_ref() {
            if Instant::now() < *expires {
                return value.clone();
            }
        }
        let (value, ttl) = build_report(s).await;
        *slot = Some((Instant::now() + ttl, value.clone()));
        value
    }
}

async fn build_report(s: &AppState) -> (Value, Duration) {
    let probe = QuotaProbe::new(&s.opencode, &s.http);
    let (usage, credits) = probe.opencode_go().await;
    let ok = usage.error.is_none();
    let windows = usage.value.get("windows").cloned().unwrap_or(Value::Null);
    let body = json!({
        "provider": "opencode-go",
        "windows": windows,
        "credits": credits,
        "error": usage.error,
        "status": usage.status,
        "fetched_at": unix_now(),
    });
    let ttl = if ok { USAGE_TTL } else { USAGE_ERROR_TTL };
    (body, ttl)
}

/// `GET /usage`: always `200`, one row per provider that could be read.
pub(crate) async fn usage(State(s): State<AppState>) -> impl IntoResponse {
    Json(s.usage.report(&s).await)
}

/// Seconds since the epoch: the report carries no formatted date (the gateway
/// has no date library), so the client formats it with its own locale.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
