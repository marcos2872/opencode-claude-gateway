//! Outgoing session headers for the OpenCode backend.
//!
//! Go routes on `x-opencode-session`; the client's own session id is
//! preferred, with a persisted fallback for headerless callers.

use crate::config::AppConfig;
use axum::http::HeaderMap;
use std::sync::OnceLock;

/// Stable fallback session id (persisted) for clients that send no session
/// header (e.g. curl smoke tests). Go uses it for routing/prompt caching.
static FALLBACK_SESSION: OnceLock<String> = OnceLock::new();

fn fallback_session_id() -> String {
    FALLBACK_SESSION
        .get_or_init(|| {
            let path = AppConfig::data_dir().join("ocg.session");
            if let Ok(s) = std::fs::read_to_string(&path) {
                let s = s.trim().to_string();
                if !s.is_empty() {
                    return s;
                }
            }
            let id = format!("ocg-{}", uuid::Uuid::new_v4());
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = crate::daemon::write_private(path, &id);
            id
        })
        .clone()
}

/// Outgoing Go session headers derived from the incoming client headers.
/// Go recognizes Claude Code's native session header; always also send
/// `x-opencode-session` (required for routing).
///
/// Codex sends neither Claude header (`session-id` / `thread-id` instead —
/// both uuids, `thread-id` identifying the conversation), so those are
/// consulted only after the existing headers: the Claude Code path is
/// byte-identical, and the Codex edge still gets a stable per-conversation
/// value instead of the persisted fallback.
pub(crate) fn session_headers(incoming: &HeaderMap) -> Vec<(String, String)> {
    let mut out = vec![];
    let claude = incoming
        .get("x-claude-code-session-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let direct = incoming
        .get("x-opencode-session")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !claude.is_empty() {
        out.push(("x-claude-code-session-id".to_string(), claude.clone()));
    }
    let session = if !direct.is_empty() {
        direct
    } else if !claude.is_empty() {
        claude
    } else {
        // Codex: `session-id` (per request) first, then `thread-id`
        // (conversation) — the Go docs ask for a consistent key per
        // conversation for prompt caching.
        let codex = incoming
            .get("session-id")
            .or_else(|| incoming.get("thread-id"))
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if codex.is_empty() {
            fallback_session_id()
        } else {
            codex.to_string()
        }
    };
    out.push(("x-opencode-session".to_string(), session));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn hdrs(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        h
    }

    #[test]
    fn claude_session_header_wins_and_is_echoed() {
        let h = hdrs(&[("x-claude-code-session-id", "claude-1")]);
        let out = session_headers(&h);
        assert!(out.contains(&("x-claude-code-session-id".into(), "claude-1".into())));
        assert!(out.contains(&("x-opencode-session".into(), "claude-1".into())));
    }

    #[test]
    fn codex_session_id_maps_to_opencode_session() {
        let h = hdrs(&[("session-id", "codex-sess")]);
        let out = session_headers(&h);
        assert_eq!(
            out,
            vec![("x-opencode-session".into(), "codex-sess".into())]
        );
    }

    #[test]
    fn codex_thread_id_is_the_second_choice() {
        let h = hdrs(&[("thread-id", "codex-thread")]);
        let out = session_headers(&h);
        assert_eq!(
            out,
            vec![("x-opencode-session".into(), "codex-thread".into())]
        );
    }

    #[test]
    fn explicit_opencode_session_still_beats_codex_headers() {
        let h = hdrs(&[("x-opencode-session", "explicit"), ("session-id", "codex")]);
        let out = session_headers(&h);
        assert_eq!(out, vec![("x-opencode-session".into(), "explicit".into())]);
    }
}
