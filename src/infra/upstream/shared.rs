//! Shared request-shaping helpers used by the Chat and Responses converters.

use serde_json::Value;

/// Floor for `max_tokens` on translated (non-Anthropic) upstreams: the
/// OpenCode zen backend rejects output limits below 16 — e.g.
/// `muse-spark-1.3-contributor` returns 400
/// `` `max_output_tokens` The number must be `>= 16` ``. Claude Code
/// verifies a model before switching to it mid-session with a
/// `max_tokens: 1` probe, so without the floor the switch fails with 400.
/// Real requests ask for thousands of tokens; raising a sub-16 value only
/// affects probes.
const MIN_UPSTREAM_OUTPUT_TOKENS: u64 = 16;

/// Raise a client `max_tokens` below [`MIN_UPSTREAM_OUTPUT_TOKENS`] to the
/// floor; anything else (including non-integer values) passes through.
pub(crate) fn floor_output_tokens(v: &Value) -> Value {
    match v.as_u64() {
        Some(n) if n < MIN_UPSTREAM_OUTPUT_TOKENS => Value::from(MIN_UPSTREAM_OUTPUT_TOKENS),
        _ => v.clone(),
    }
}

/// Cached input tokens reported by an upstream `usage` object, across the
/// wire shapes the gateway forwards. First hit wins, `0` when absent.
///
/// Shapes covered (no per-provider branching — every upstream that reports
/// prefix-cache hits in one of these lands here):
/// - DeepSeek-native `usage.prompt_cache_hit_tokens`
/// - Chat Completions `usage.prompt_tokens_details.cached_tokens`
/// - Responses `usage.input_tokens_details.cached_tokens`
/// - Anthropic `usage.cache_read_input_tokens` (verbatim/translated bodies
///   that already carry the Anthropic shape)
pub(crate) fn cached_input_tokens(usage: Option<&Value>) -> u64 {
    let Some(u) = usage else {
        return 0;
    };
    if let Some(n) = u.get("prompt_cache_hit_tokens").and_then(Value::as_u64) {
        return n;
    }
    for details_key in ["prompt_tokens_details", "input_tokens_details"] {
        if let Some(n) = u
            .get(details_key)
            .and_then(|d| d.get("cached_tokens"))
            .and_then(Value::as_u64)
        {
            return n;
        }
    }
    u.get("cache_read_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0)
}
// ---------------------------------------------------------------------------
// Anthropic -> OpenAI Chat Completions
// ---------------------------------------------------------------------------

pub(crate) fn image_part_to_openai(b: &Value) -> Option<Value> {
    let src = b.get("source")?;
    let mt = src
        .get("media_type")
        .and_then(Value::as_str)
        .unwrap_or("image/jpeg");
    let data = src.get("data").and_then(Value::as_str)?;
    (!data.is_empty()).then(|| {
        serde_json::json!({
            "type": "image_url",
            "image_url": {"url": format!("data:{mt};base64,{data}")}
        })
    })
}
pub(crate) fn image_part_to_responses(b: &Value) -> Option<Value> {
    let src = b.get("source")?;
    let mt = src
        .get("media_type")
        .and_then(Value::as_str)
        .unwrap_or("image/jpeg");
    let data = src.get("data").and_then(Value::as_str)?;
    (!data.is_empty()).then(|| {
        serde_json::json!({
            "type": "input_image",
            "image_url": format!("data:{mt};base64,{data}")
        })
    })
}
/// Split a `tool_result` into its text and the images nested in its content.
///
/// Neither Chat Completions nor Responses has an error flag on a tool output,
/// so a failed result (`is_error`) is made explicit in the payload text
/// instead of being silently flattened to its (possibly empty) content.
pub(crate) fn tool_result_parts(b: &Value) -> (String, Vec<Value>, Vec<Value>) {
    let is_error = b.get("is_error").and_then(Value::as_bool).unwrap_or(false);
    let mut text = block_text(b).unwrap_or_default();
    if is_error {
        text = if text.is_empty() {
            "Error: tool execution failed".to_string()
        } else {
            format!("Error: {text}")
        };
    }
    let mut images_oai: Vec<Value> = vec![];
    let mut images_resp: Vec<Value> = vec![];
    if let Some(parts) = b.get("content").and_then(Value::as_array) {
        for p in parts {
            if let Some(img) = image_part_to_openai(p) {
                images_oai.push(img);
            }
            if let Some(img) = image_part_to_responses(p) {
                images_resp.push(img);
            }
        }
    }
    (text, images_oai, images_resp)
}
pub(crate) fn block_text(b: &Value) -> Option<String> {
    match b.get("type").and_then(|t| t.as_str()) {
        Some("text") => b
            .get("text")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string()),
        Some("tool_result") => {
            let c = b.get("content");
            match c {
                Some(Value::String(s)) => Some(s.clone()),
                Some(Value::Array(arr)) => Some(
                    arr.iter()
                        .filter_map(|x| {
                            x.get("text")
                                .and_then(|t| t.as_str())
                                .map(|s| s.to_string())
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Parse a JSON object from a string, falling back to an empty object.
/// Used by both response converters for tool-call arguments.
pub(crate) fn parse_args(s: &str) -> Value {
    if s.trim().is_empty() {
        return Value::Object(Default::default());
    }
    serde_json::from_str(s).unwrap_or(Value::Object(Default::default()))
}
