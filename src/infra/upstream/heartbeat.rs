//! SSE framing (`sse`, `sse_error`) and the heartbeat wrapper that injects
//! `event: ping` during upstream silence.

use serde_json::Value;

pub fn sse(v: &Value) -> String {
    format!(
        "event: {}\ndata: {}\n\n",
        v.get("type").and_then(|t| t.as_str()).unwrap_or("message"),
        v
    )
}
/// Anthropic `error` SSE frame. The Messages API allows an `error` event
/// mid-stream (after `message_start`), so a translator can report an upstream
/// failure instead of ending the stream silently.
pub fn sse_error(err_type: &str, message: &str) -> String {
    sse(&serde_json::json!({
        "type": "error",
        "error": {"type": err_type, "message": message}
    }))
}
/// Terminal failure frame for the `/v1/responses` (Codex) edge. Codex's
/// parser treats a bare `error` event as a no-op (it would then wait the
/// full 300s idle timeout), so a mid-stream failure must surface as
/// `response.failed` — with `response.error.{code,message}` and an `id`,
/// which the parser requires on the response object — as the last frame
/// before the stream closes.
pub fn responses_sse_error(code: &str, message: &str) -> String {
    sse(&serde_json::json!({
        "type": "response.failed",
        "response": {
            "id": "resp_ocg_upstream_failed",
            "object": "response",
            "status": "failed",
            "error": {"code": code, "message": message}
        }
    }))
}
/// Wrap a byte stream, injecting `event: ping` SSE frames whenever the
/// upstream stays silent longer than `idle`. Keeps the client's stream
/// watchdog fed during long reasoning pauses.
pub fn with_heartbeat<S>(
    stream: S,
    idle: std::time::Duration,
) -> impl futures::Stream<Item = Result<Vec<u8>, std::io::Error>>
where
    S: futures::Stream<Item = Result<Vec<u8>, std::io::Error>>,
{
    async_stream::stream! {
        let mut inner = Box::pin(stream);
        loop {
            match tokio::time::timeout(idle, futures::StreamExt::next(&mut inner)).await {
                Ok(Some(item)) => yield item,
                Ok(None) => break,
                Err(_) => {
                    yield Ok::<_, std::io::Error>(
                        "event: ping\ndata: {\"type\": \"ping\"}\n\n".as_bytes().to_vec(),
                    );
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    #[tokio::test]
    async fn heartbeat_fires_during_silence() {
        use futures::StreamExt;
        use std::time::Duration;
        let slow = async_stream::stream! {
            tokio::time::sleep(Duration::from_millis(180)).await;
            yield Ok::<_, std::io::Error>(b"data".to_vec());
        };
        let mut hb = Box::pin(with_heartbeat(slow, Duration::from_millis(50)));
        // ~50/100/150ms: pings; then the payload.
        for _ in 0..3 {
            let item = hb.next().await.unwrap().unwrap();
            assert!(String::from_utf8_lossy(&item).contains("ping"));
        }
        let item = hb.next().await.unwrap().unwrap();
        assert_eq!(item, b"data".to_vec());
        assert!(hb.next().await.is_none());
    }

    #[test]
    fn responses_sse_error_is_a_terminal_failed_frame() {
        let frame = responses_sse_error("stream_closed", "upstream went away");
        assert!(frame.starts_with("event: response.failed\n"), "{frame}");
        let payload = frame
            .lines()
            .find(|l| l.starts_with("data:"))
            .unwrap()
            .trim_start_matches("data:")
            .trim();
        let v: Value = serde_json::from_str(payload).unwrap();
        assert_eq!(v["type"], "response.failed");
        assert_eq!(v["response"]["id"], "resp_ocg_upstream_failed");
        assert_eq!(v["response"]["status"], "failed");
        assert_eq!(v["response"]["error"]["code"], "stream_closed");
        assert_eq!(v["response"]["error"]["message"], "upstream went away");
    }
}
