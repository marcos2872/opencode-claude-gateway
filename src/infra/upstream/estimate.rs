//! Per-part token estimate for the optional count_tokens endpoint (no
//! tokenizer).

use serde_json::Value;

/// Per-part token estimate for the optional count_tokens endpoint (no
/// tokenizer: Anthropic itself documents its counts as an estimate). Text is
/// chars/4 (≈1 token per 4 ASCII chars); each message and tool adds a small
/// fixed overhead; images and base64 documents count their real size, so a
/// large attachment no longer inflates the count as if it were prose.
pub fn estimate_tokens(body: &Value) -> u64 {
    let mut tokens: u64 = 0;
    fn add_text(tokens: &mut u64, s: &str) {
        *tokens += (s.chars().count() as u64 / 4).max(1);
    }
    if let Some(sys) = body.get("system") {
        match sys {
            Value::String(s) => add_text(&mut tokens, s),
            Value::Array(arr) => {
                for b in arr {
                    if let Some(t) = text_of(b) {
                        add_text(&mut tokens, t);
                    }
                }
            }
            _ => {}
        }
        tokens += 3; // system wrapper.
    }
    if let Some(msgs) = body.get("messages").and_then(|m| m.as_array()) {
        for m in msgs {
            tokens += 3; // per-message overhead (Anthropic/OpenAI style).
            match m.get("content") {
                Some(Value::String(s)) => add_text(&mut tokens, s),
                Some(Value::Array(blocks)) => {
                    for b in blocks {
                        match block_kind(b) {
                            BlockKind::Text => {
                                if let Some(t) = text_of(b) {
                                    add_text(&mut tokens, t);
                                }
                            }
                            BlockKind::Image | BlockKind::Document => {
                                tokens += estimate_media(b);
                            }
                            BlockKind::ToolUse => tokens += 8, // id + name + JSON.
                            BlockKind::ToolResult => {
                                // Text lives under `content` here (a plain
                                // string or an array of content blocks), not
                                // under `text` like in `text` blocks.
                                match b.get("content") {
                                    Some(Value::String(s)) => add_text(&mut tokens, s),
                                    Some(Value::Array(blocks)) => {
                                        for inner in blocks {
                                            if let Some(x) = text_of(inner) {
                                                add_text(&mut tokens, x);
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                                tokens += 4;
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(tools) = body.get("tools").and_then(|t| t.as_array()) {
        for t in tools {
            tokens += 4; // tool wrapper.
            if let Some(n) = t.get("name").and_then(|n| n.as_str()) {
                add_text(&mut tokens, n);
            }
            if let Some(d) = t.get("description").and_then(|d| d.as_str()) {
                add_text(&mut tokens, d);
            }
            if let Some(schema) = t.get("input_schema") {
                tokens += (schema.to_string().len() as u64 / 4).max(1);
            }
        }
    }
    tokens.max(1)
}
enum BlockKind {
    Text,
    Image,
    Document,
    ToolUse,
    ToolResult,
    Other,
}

fn block_kind(b: &Value) -> BlockKind {
    match b.get("type").and_then(|t| t.as_str()) {
        Some("text") => BlockKind::Text,
        Some("image") => BlockKind::Image,
        Some("document") => BlockKind::Document,
        Some("tool_use") => BlockKind::ToolUse,
        Some("tool_result") => BlockKind::ToolResult,
        _ => BlockKind::Other,
    }
}

fn text_of(b: &Value) -> Option<&str> {
    b.get("text").and_then(|t| t.as_str())
}

/// Tokens for an image/document block: fixed overhead + a share of the
/// base64 payload (base64 inflates content by ~33%; per the API's
/// documentation images have a large base cost regardless of size).
fn estimate_media(b: &Value) -> u64 {
    let is_image = matches!(block_kind(b), BlockKind::Image);
    let mut tokens: u64 = if is_image { 800 } else { 400 }; // base cost.
    if let Some(src) = b.get("source") {
        if let Some(data) = src.get("data").and_then(|d| d.as_str()) {
            tokens += (data.len() as u64 / 4) / 3; // decoded bytes → tokens.
        }
        if let Some(url) = src.get("url").and_then(|u| u.as_str()) {
            tokens += (url.len() as u64 / 4).max(1);
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_body_counts_one() {
        assert_eq!(estimate_tokens(&json!({})), 1);
    }

    #[test]
    fn system_string_adds_wrapper_overhead() {
        // "abcd" is 4 chars -> 1 token, plus the 3-token system wrapper.
        assert_eq!(estimate_tokens(&json!({"system": "abcd"})), 4);
    }

    #[test]
    fn system_array_counts_text_blocks() {
        // 8 chars -> 2 tokens, plus the wrapper.
        let body = json!({"system": [{"type": "text", "text": "abcdefgh"}]});
        assert_eq!(estimate_tokens(&body), 5);
    }

    #[test]
    fn system_counts_chars_not_bytes() {
        // 8 chars (16 bytes in UTF-8) -> 2 tokens, plus the wrapper.
        let body = json!({"system": "éééééééé"});
        assert_eq!(estimate_tokens(&body), 5);
    }

    #[test]
    fn string_message_has_per_message_overhead() {
        // "hi" is 2 chars -> min 1 token, plus 3 per message.
        let body = json!({"messages": [{"role": "user", "content": "hi"}]});
        assert_eq!(estimate_tokens(&body), 4);
    }

    #[test]
    fn text_blocks_count_by_chars() {
        // "hello world!" is 12 chars -> 3 tokens, plus 3 per message.
        let body = json!({"messages": [{"role": "user", "content": [
            {"type": "text", "text": "hello world!"}
        ]}]});
        assert_eq!(estimate_tokens(&body), 6);
    }

    #[test]
    fn tool_use_block_has_fixed_cost() {
        let body = json!({"messages": [{"role": "user", "content": [
            {"type": "tool_use", "id": "t1", "name": "Read", "input": {}}
        ]}]});
        assert_eq!(estimate_tokens(&body), 3 + 8);
    }

    #[test]
    fn tool_result_counts_text_plus_overhead() {
        // Array content: "ok" dá 1 token + 4 do bloco + 3 da mensagem.
        let body = json!({"messages": [{"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "t1", "content": [
                {"type": "text", "text": "ok"}
            ]}
        ]}]});
        assert_eq!(estimate_tokens(&body), 3 + 1 + 4);
    }

    #[test]
    fn tool_result_string_content_counts() {
        let body = json!({"messages": [{"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "t1", "content": "abcdefgh"}
        ]}]});
        assert_eq!(estimate_tokens(&body), 3 + 2 + 4);
    }

    #[test]
    fn tool_result_without_content_counts_overhead_only() {
        let body = json!({"messages": [{"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "t1"}
        ]}]});
        assert_eq!(estimate_tokens(&body), 3 + 4);
    }

    #[test]
    fn image_block_prices_base64_payload() {
        // 1200 bytes of base64 -> 800 base + (1200/4)/3 payload, plus message.
        let data = "A".repeat(1200);
        let body = json!({"messages": [{"role": "user", "content": [
            {"type": "image", "source": {"type": "base64", "data": data}}
        ]}]});
        assert_eq!(estimate_tokens(&body), 3 + 800 + 100);
    }

    #[test]
    fn document_block_is_cheaper_than_image() {
        let data = "A".repeat(120);
        let body = json!({"messages": [{"role": "user", "content": [
            {"type": "document", "source": {"type": "base64", "data": data}}
        ]}]});
        assert_eq!(estimate_tokens(&body), 3 + 400 + 10);
    }

    #[test]
    fn image_url_counts_chars() {
        // "http://x" is 8 chars -> 2 tokens on top of the 800 base + message.
        let body = json!({"messages": [{"role": "user", "content": [
            {"type": "image", "source": {"type": "url", "url": "http://x"}}
        ]}]});
        assert_eq!(estimate_tokens(&body), 3 + 800 + 2);
    }

    #[test]
    fn unknown_block_kinds_are_free() {
        let body = json!({"messages": [{"role": "user", "content": [
            {"type": "thinking", "thinking": "hmm"}
        ]}]});
        assert_eq!(estimate_tokens(&body), 3);
    }

    #[test]
    fn tools_add_wrapper_name_description_and_schema() {
        // wrapper 4 + "Read" 1 + "Read a file" (11 chars) 2 + schema 17/4 = 4.
        let body = json!({"tools": [{
            "name": "Read",
            "description": "Read a file",
            "input_schema": {"type": "object"}
        }]});
        assert_eq!(estimate_tokens(&body), 4 + 1 + 2 + 4);
    }
}
