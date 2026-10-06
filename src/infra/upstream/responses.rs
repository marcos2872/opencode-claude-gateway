//! Anthropic <-> OpenAI Responses API translation.

use super::shared::{
    block_text, floor_output_tokens, image_part_to_responses, parse_args, tool_result_parts,
};
use serde_json::Value;
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// Anthropic -> OpenAI Responses API (`/responses`: Go GPT/Grok/Muse rows)
// ---------------------------------------------------------------------------

/// Convert an Anthropic `/v1/messages` body into an OpenAI `/responses` body.
pub fn anthropic_to_responses(body: &Value, upstream_model: &str) -> Value {
    // `tool_use` ids dropped for having no name (see `anthropic_to_openai`): the
    // matching `tool_result` must be dropped too, or Responses sees an orphan
    // `function_call_output`.
    let mut dropped_tool_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut input: Vec<Value> = vec![];
    // The zen backend rejects a `role:"user"` item sitting between
    // `function_call`s and their `function_call_output`s when 2+ calls are
    // pending (`400 The request contains invalid parameters`) — Claude Code's
    // mid-turn user injections (`The user sent a new message while you were
    // working`) land exactly there. System items in the same position are
    // fine, and 1 pending call passes, but the safe universal rule is: hold
    // user items back until every seen call has its output, then flush.
    // (Empirically bisected against opencode-go/muse-spark; chat path unaffected.)
    let mut pending: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut deferred_user: Vec<Value> = vec![];
    // User-role message items go after pending outputs when calls are open;
    // anything else (assistant text, system) stays where the client put it.
    macro_rules! push_msg {
        ($item:expr) => {{
            let item = $item;
            if pending.is_empty() {
                input.push(item);
            } else {
                deferred_user.push(item);
            }
        }};
    }
    // Outputs close their call; the first output that empties the pending set
    // also releases every deferred user item, keeping them after the outputs.
    macro_rules! extend_outputs {
        ($outputs:expr) => {{
            for o in &$outputs {
                if let Some(id) = o.get("call_id").and_then(Value::as_str) {
                    pending.remove(id);
                }
            }
            input.extend($outputs);
            if pending.is_empty() && !deferred_user.is_empty() {
                input.append(&mut deferred_user);
            }
        }};
    }
    if let Some(msgs) = body.get("messages").and_then(|m| m.as_array()) {
        for m in msgs {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            match m.get("content") {
                Some(Value::String(s)) => {
                    let kind = if role == "assistant" {
                        "output_text"
                    } else {
                        "input_text"
                    };
                    let item = serde_json::json!({
                        "role": role,
                        "content": [{"type": kind, "text": s}]
                    });
                    if role == "user" {
                        push_msg!(item);
                    } else {
                        input.push(item);
                    }
                }
                Some(Value::Array(blocks)) => {
                    let mut texts: Vec<String> = vec![];
                    let mut images: Vec<Value> = vec![];
                    let mut calls: Vec<Value> = vec![];
                    let mut outputs: Vec<Value> = vec![];
                    for b in blocks {
                        match b.get("type").and_then(|t| t.as_str()) {
                            Some("text") => {
                                if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                                    texts.push(t.to_string());
                                }
                            }
                            Some("image") => {
                                if let Some(img) = image_part_to_responses(b) {
                                    images.push(img);
                                }
                            }
                            Some("tool_use") => {
                                let name = b.get("name").and_then(|n| n.as_str()).unwrap_or("");
                                if name.is_empty() {
                                    if let Some(id) = b.get("id").and_then(|x| x.as_str()) {
                                        if !id.is_empty() {
                                            dropped_tool_ids.insert(id.to_string());
                                        }
                                    }
                                } else {
                                    calls.push(serde_json::json!({
                                        "type": "function_call",
                                        "call_id": b.get("id").cloned().unwrap_or(Value::String("call_0".into())),
                                        "name": name,
                                        "arguments": serde_json::to_string(b.get("input").unwrap_or(&Value::Object(Default::default()))).unwrap_or_else(|_| "{}".into())
                                    }));
                                }
                            }
                            Some("tool_result") => {
                                let id = b
                                    .get("tool_use_id")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                if dropped_tool_ids.contains(&id) {
                                    continue;
                                }
                                let (txt, _, nested_images) = tool_result_parts(b);
                                outputs.push(serde_json::json!({
                                    "type": "function_call_output",
                                    "call_id": id,
                                    "output": txt
                                }));
                                // Responses has no error flag and no place for
                                // images inside a function_call_output, so they
                                // follow as a user input_image message. The
                                // ordered input list keeps them in place.
                                if !nested_images.is_empty() {
                                    push_msg!(serde_json::json!({
                                        "role": "user",
                                        "content": nested_images
                                    }));
                                }
                            }
                            _ => {}
                        }
                    }
                    if role == "assistant" {
                        if !texts.join("\n").trim().is_empty() {
                            input.push(serde_json::json!({
                                "role": "assistant",
                                "content": [{"type": "output_text", "text": texts.join("\n")}]
                            }));
                        }
                        for c in &calls {
                            if let Some(id) = c.get("call_id").and_then(Value::as_str) {
                                pending.insert(id.to_string());
                            }
                        }
                        input.extend(calls);
                        extend_outputs!(outputs);
                    } else {
                        if !texts.join("\n").trim().is_empty() || !images.is_empty() {
                            let mut content: Vec<Value> = vec![];
                            let t = texts.join("\n");
                            if !t.is_empty() {
                                content.push(serde_json::json!({"type": "input_text", "text": t}));
                            }
                            content.extend(images);
                            push_msg!(serde_json::json!({"role": "user", "content": content}));
                        }
                        extend_outputs!(outputs);
                    }
                }
                _ => {}
            }
        }
    }
    // Calls that never got an output (orphan) keep user items deferred until
    // here; release them so the orphan itself is what the backend rejects.
    if !deferred_user.is_empty() {
        input.append(&mut deferred_user);
    }

    let mut out = serde_json::json!({
        "model": upstream_model,
        "input": input,
    });
    if let Some(sys) = body.get("system") {
        let text = match sys {
            Value::String(s) => s.clone(),
            Value::Array(arr) => arr
                .iter()
                .filter_map(block_text)
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        if !text.is_empty() {
            out["instructions"] = Value::String(text);
        }
    }
    if let Some(tools) = body.get("tools").and_then(|t| t.as_array()) {
        let mapped: Vec<Value> = tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "name": t.get("name").cloned().unwrap_or(Value::String("".into())),
                    "description": t.get("description").cloned().unwrap_or(Value::String("".into())),
                    "parameters": t.get("input_schema").cloned().unwrap_or(serde_json::json!({"type":"object"}))
                })
            })
            .collect();
        out["tools"] = Value::Array(mapped);
        if let Some(tc) = body.get("tool_choice") {
            // Responses API accepts "auto" | "required" | "none" | {"type":"function",...}.
            out["tool_choice"] = match tc.get("type").and_then(|t| t.as_str()) {
                Some("any") => serde_json::json!("required"),
                Some("none") => serde_json::json!("none"),
                Some("tool") => {
                    if let Some(name) = tc.get("name").and_then(|n| n.as_str()) {
                        serde_json::json!({"type":"function","name":name})
                    } else {
                        serde_json::json!("required")
                    }
                }
                _ => serde_json::json!("auto"),
            };
        }
    }
    for key in ["temperature", "top_p"] {
        if let Some(v) = body.get(key) {
            out[key] = v.clone();
        }
    }
    if let Some(m) = body.get("max_tokens") {
        out["max_output_tokens"] = floor_output_tokens(m);
    }
    if body
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false)
    {
        out["stream"] = Value::Bool(true);
    }
    out
}
/// Convert an OpenAI Responses API response into an Anthropic Messages response.
pub fn responses_to_anthropic(resp: &Value, gateway_model: &str) -> Value {
    let msg_id = format!(
        "msg_{}",
        &uuid::Uuid::new_v4().to_string().replace('-', "")[..24]
    );
    let items = resp
        .get("output")
        .and_then(|o| o.as_array())
        .cloned()
        .unwrap_or_default();
    let mut content: Vec<Value> = vec![];
    for item in &items {
        match item.get("type").and_then(|t| t.as_str()) {
            Some("message") => {
                if let Some(parts) = item.get("content").and_then(|c| c.as_array()) {
                    for p in parts {
                        if let Some(t) = p.get("text").and_then(|x| x.as_str()) {
                            if !t.is_empty() {
                                content.push(serde_json::json!({"type": "text", "text": t}));
                            }
                        }
                    }
                }
            }
            Some("function_call") => {
                content.push(serde_json::json!({
                    "type": "tool_use",
                    "id": item.get("call_id").cloned().unwrap_or(Value::String("toolu_0".into())),
                    "name": item.get("name").cloned().unwrap_or(Value::String("".into())),
                    "input": parse_args(item.get("arguments").and_then(|a| a.as_str()).unwrap_or("{}"))
                }));
            }
            Some("reasoning") => {}
            _ => {}
        }
    }
    if content.is_empty() {
        content.push(serde_json::json!({"type": "text", "text": ""}));
    }
    let has_tools = content
        .iter()
        .any(|c| c.get("type").and_then(|t| t.as_str()) == Some("tool_use"));
    let status = resp
        .get("status")
        .and_then(|s| s.as_str())
        .unwrap_or("completed");
    let incomplete_max = resp
        .get("incomplete_details")
        .and_then(|d| d.get("reason"))
        .and_then(|r| r.as_str())
        == Some("max_output_tokens");
    let stop_reason = if has_tools {
        "tool_use"
    } else if status == "incomplete" || incomplete_max {
        "max_tokens"
    } else {
        "end_turn"
    };
    let usage = resp.get("usage");
    serde_json::json!({
        "id": msg_id,
        "type": "message",
        "role": "assistant",
        "model": gateway_model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": {
            "input_tokens": usage.and_then(|u| u.get("input_tokens")).and_then(|x| x.as_u64()).unwrap_or(0),
            "output_tokens": usage.and_then(|u| u.get("output_tokens")).and_then(|x| x.as_u64()).unwrap_or(0)
        }
    })
}

// ---------------------------------------------------------------------------
// Responses request / Anthropic response — Fase 2 (Codex edge on
// Chat/Anthropic upstreams). The canonical bridge is always Anthropic: the
// request lands as a `/messages` body (or goes through `anthropic_to_openai`
// for Chat rows) and every stream is reshaped back from Anthropic events.
// ---------------------------------------------------------------------------

/// Tool names the client declared with `type: "custom"` (Codex's apply_patch).
/// The request carries them as single-field `input` functions; the response
/// side needs the set back to rebuild `custom_tool_call` items.
pub fn codex_custom_tool_names(body: &Value) -> HashSet<String> {
    body.get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter(|t| t.get("type").and_then(Value::as_str) == Some("custom"))
                .filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Append one content block, coalescing consecutive same-role messages —
/// Anthropic messages must alternate, while Responses `input` is a flat item
/// list (`function_call_output` + follow-up `message` are both "user").
fn push_block(msgs: &mut Vec<Value>, role: &str, block: Value) {
    if let Some(last) = msgs.last_mut() {
        if last.get("role").and_then(Value::as_str) == Some(role) {
            if let Some(arr) = last.get_mut("content").and_then(|c| c.as_array_mut()) {
                arr.push(block);
                return;
            }
        }
    }
    msgs.push(serde_json::json!({"role": role, "content": [block]}));
}

/// Reverse of `image_part_to_responses`: a Responses `input_image` data URL
/// (or plain URL) back into an Anthropic image block.
fn input_image_to_anthropic(p: &Value) -> Option<Value> {
    let raw = match p.get("image_url") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(o)) => o.get("url").and_then(Value::as_str)?.to_string(),
        _ => return None,
    };
    if let Some(rest) = raw.strip_prefix("data:") {
        let (meta, data) = rest.split_once(',')?;
        let media_type = meta.split(';').next().unwrap_or("image/png");
        if data.is_empty() {
            return None;
        }
        Some(serde_json::json!({
            "type": "image",
            "source": {"type": "base64", "media_type": media_type, "data": data}
        }))
    } else if raw.starts_with("http://") || raw.starts_with("https://") {
        Some(serde_json::json!({
            "type": "image",
            "source": {"type": "url", "url": raw}
        }))
    } else {
        None
    }
}

/// Convert an OpenAI Responses request (the Codex edge) into a canonical
/// Anthropic Messages body for Chat/Anthropic upstreams (Fase 2).
///
/// Carried over: `instructions` → `system`, messages (with `developer` folded
/// into the system prompt), `function_call`/`function_call_output`,
/// `custom_tool_call`/`custom_tool_call_output` (custom tools become
/// single-field `input` functions), images, tools, `tool_choice`,
/// `max_output_tokens` → `max_tokens` (floored) and `stream`.
///
/// Deliberately dropped (documented in `docs/config-codex.md`):
/// `reasoning`/`include` (Anthropic has no round-trippable equivalent — the
/// catalog variant still applies), `store`/`prompt_cache_key`/`client_metadata`,
/// `parallel_tool_calls`, and non function/custom tools (`web_search`,
/// `namespace`, ...). `reasoning` and other item types in `input` are dropped:
/// upstreams never see them.
pub fn responses_to_anthropic_request(body: &Value, upstream_model: &str) -> Value {
    let mut system_parts: Vec<String> = vec![];
    if let Some(s) = body.get("instructions").and_then(Value::as_str) {
        if !s.trim().is_empty() {
            system_parts.push(s.to_string());
        }
    }
    let mut msgs: Vec<Value> = vec![];
    if let Some(items) = body.get("input").and_then(Value::as_array) {
        for item in items {
            match item.get("type").and_then(Value::as_str) {
                Some("message") => {
                    let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
                    let texts: Vec<String> = item
                        .get("content")
                        .and_then(Value::as_array)
                        .map(|parts| {
                            parts
                                .iter()
                                .filter_map(|p| match p.get("type").and_then(Value::as_str) {
                                    Some("input_text") | Some("output_text") | Some("text") => {
                                        p.get("text").and_then(Value::as_str).map(str::to_string)
                                    }
                                    _ => None,
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    if role == "developer" || role == "system" {
                        let joined = texts.join("\n");
                        if !joined.trim().is_empty() {
                            system_parts.push(joined);
                        }
                        continue;
                    }
                    let role = if role == "assistant" {
                        "assistant"
                    } else {
                        "user"
                    };
                    let mut blocks: Vec<Value> = vec![];
                    for p in item
                        .get("content")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        match p.get("type").and_then(Value::as_str) {
                            Some("input_text") | Some("output_text") | Some("text") => {
                                if let Some(t) = p.get("text").and_then(Value::as_str) {
                                    blocks.push(serde_json::json!({"type": "text", "text": t}));
                                }
                            }
                            Some("input_image") => {
                                if let Some(img) = input_image_to_anthropic(p) {
                                    blocks.push(img);
                                }
                            }
                            _ => {}
                        }
                    }
                    for b in blocks {
                        push_block(&mut msgs, role, b);
                    }
                }
                Some("function_call") => {
                    let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                    if name.is_empty() {
                        continue;
                    }
                    push_block(
                        &mut msgs,
                        "assistant",
                        serde_json::json!({
                            "type": "tool_use",
                            "id": item.get("call_id").cloned().unwrap_or(Value::String("call_0".into())),
                            "name": name,
                            "input": parse_args(item.get("arguments").and_then(Value::as_str).unwrap_or("{}"))
                        }),
                    );
                }
                Some("custom_tool_call") => {
                    let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                    if name.is_empty() {
                        continue;
                    }
                    push_block(
                        &mut msgs,
                        "assistant",
                        serde_json::json!({
                            "type": "tool_use",
                            "id": item.get("call_id").cloned().unwrap_or(Value::String("call_0".into())),
                            "name": name,
                            "input": {"input": item.get("input").and_then(Value::as_str).unwrap_or("")}
                        }),
                    );
                }
                Some("function_call_output") | Some("custom_tool_call_output") => {
                    push_block(
                        &mut msgs,
                        "user",
                        serde_json::json!({
                            "type": "tool_result",
                            "tool_use_id": item.get("call_id").cloned().unwrap_or(Value::String("call_0".into())),
                            "content": item.get("output").and_then(Value::as_str).unwrap_or("")
                        }),
                    );
                }
                // `reasoning`, `local_shell_call`, `web_search_call`, ...:
                // no Anthropic equivalent — dropped (documented).
                _ => {}
            }
        }
    }

    let mut out = serde_json::json!({
        "model": upstream_model,
        "messages": msgs,
    });
    let system = system_parts.join("\n\n");
    if !system.is_empty() {
        out["system"] = Value::String(system);
    }
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        let mut mapped: Vec<Value> = vec![];
        for t in tools {
            let name = t.get("name").and_then(Value::as_str).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            let schema = match t.get("type").and_then(Value::as_str) {
                Some("function") => t
                    .get("parameters")
                    .cloned()
                    .unwrap_or(serde_json::json!({"type": "object"})),
                // Custom tools (apply_patch) ride as a single `input` string;
                // `custom_tool_call` items wrap the freeform text to match.
                Some("custom") => serde_json::json!({
                    "type": "object",
                    "properties": {"input": {"type": "string"}},
                    "required": ["input"]
                }),
                // web_search / namespace / ...: no Anthropic equivalent.
                _ => continue,
            };
            mapped.push(serde_json::json!({
                "name": name,
                "description": t.get("description").cloned().unwrap_or(Value::String("".into())),
                "input_schema": schema
            }));
        }
        if !mapped.is_empty() {
            out["tools"] = Value::Array(mapped);
        }
        if let Some(tc) = body.get("tool_choice") {
            out["tool_choice"] = match tc {
                Value::String(s) => match s.as_str() {
                    "required" => serde_json::json!({"type": "any"}),
                    "none" => serde_json::json!({"type": "none"}),
                    _ => serde_json::json!({"type": "auto"}),
                },
                Value::Object(_) => {
                    let name = tc.get("name").and_then(Value::as_str);
                    match (tc.get("type").and_then(Value::as_str), name) {
                        (Some("function"), Some(n)) => {
                            serde_json::json!({"type": "tool", "name": n})
                        }
                        (Some("none"), _) => serde_json::json!({"type": "none"}),
                        (Some("required"), _) => serde_json::json!({"type": "any"}),
                        _ => serde_json::json!({"type": "auto"}),
                    }
                }
                _ => serde_json::json!({"type": "auto"}),
            };
        }
    }
    if let Some(m) = body.get("max_output_tokens") {
        out["max_tokens"] = floor_output_tokens(m);
    }
    for key in ["temperature", "top_p"] {
        if let Some(v) = body.get(key) {
            out[key] = v.clone();
        }
    }
    if body.get("stream").and_then(Value::as_bool).unwrap_or(false) {
        out["stream"] = Value::Bool(true);
    }
    out
}

/// Convert an Anthropic Messages response into an OpenAI Responses response
/// (non-streaming side of the Codex edge). `custom_tools` rebuilds
/// `custom_tool_call` items for the tools the client declared as custom.
pub fn anthropic_to_responses_response(
    resp: &Value,
    gateway_model: &str,
    custom_tools: &HashSet<String>,
) -> Value {
    let resp_id = format!(
        "resp_{}",
        &uuid::Uuid::new_v4().to_string().replace('-', "")[..24]
    );
    let mut output: Vec<Value> = vec![];
    let mut texts: Vec<String> = vec![];
    let content = resp.get("content").and_then(Value::as_array);
    if let Some(blocks) = content {
        for b in blocks {
            match b.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(t) = b.get("text").and_then(Value::as_str) {
                        texts.push(t.to_string());
                    }
                }
                Some("tool_use") => {
                    let name = b.get("name").and_then(Value::as_str).unwrap_or("");
                    let id = b
                        .get("id")
                        .cloned()
                        .unwrap_or(Value::String("call_0".into()));
                    if custom_tools.contains(name) {
                        let input = b.get("input").and_then(|i| i.get("input"));
                        let raw = match input {
                            Some(Value::String(s)) => s.clone(),
                            Some(other) => other.to_string(),
                            None => String::new(),
                        };
                        output.push(serde_json::json!({
                            "type": "custom_tool_call",
                            "call_id": id,
                            "name": name,
                            "input": raw
                        }));
                    } else {
                        output.push(serde_json::json!({
                            "type": "function_call",
                            "call_id": id,
                            "name": name,
                            "arguments": serde_json::to_string(
                                b.get("input").unwrap_or(&Value::Object(Default::default()))
                            ).unwrap_or_else(|_| "{}".into())
                        }));
                    }
                }
                // thinking / redacted_thinking: not round-trippable.
                _ => {}
            }
        }
    }
    let text = texts.join("\n");
    if !text.is_empty() || output.is_empty() {
        output.insert(
            0,
            serde_json::json!({
                "type": "message",
                "id": format!("msg_out_{}", &uuid::Uuid::new_v4().to_string().replace('-', "")[..16]),
                "status": "completed",
                "role": "assistant",
                "content": [{"type": "output_text", "text": text}]
            }),
        );
    }
    let stop_reason = resp
        .get("stop_reason")
        .and_then(Value::as_str)
        .unwrap_or("");
    let incomplete = stop_reason == "max_tokens";
    let usage = resp.get("usage");
    let input_tokens = usage
        .and_then(|u| u.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .and_then(|u| u.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut out = serde_json::json!({
        "id": resp_id,
        "object": "response",
        "model": gateway_model,
        "status": if incomplete { "incomplete" } else { "completed" },
        "output": output,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens
        }
    });
    if incomplete {
        out["incomplete_details"] = serde_json::json!({"reason": "max_output_tokens"});
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    #[test]
    fn converts_to_responses_request() {
        let body = serde_json::json!({
            "model": "x",
            "system": "be concise",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "looking"},
                    {"type": "tool_use", "id": "t1", "name": "Read", "input": {"path": "a"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "ok"}
                ]}
            ],
            "tools": [{"name": "Read", "description": "d", "input_schema": {"type": "object"}}],
            "max_tokens": 4096
        });
        let r = anthropic_to_responses(&body, "upstream");
        assert_eq!(r["model"], "upstream");
        assert_eq!(r["instructions"], "be concise");
        assert_eq!(r["max_output_tokens"], 4096);
        let input = r["input"].as_array().unwrap();
        assert!(input.iter().any(|i| i["type"] == "function_call"));
        assert!(input.iter().any(|i| i["type"] == "function_call_output"));
        assert_eq!(r["tools"][0]["name"], "Read");
    }
    #[test]
    fn converts_responses_response_with_tools() {
        let resp = serde_json::json!({
            "status": "completed",
            "output": [
                {"type": "message", "content": [{"type": "output_text", "text": "x"}]},
                {"type": "function_call", "call_id": "c1", "name": "Bash", "arguments": "{\"cmd\":\"ls\"}"}
            ],
            "usage": {"input_tokens": 5, "output_tokens": 7}
        });
        let a = responses_to_anthropic(&resp, "gw");
        assert_eq!(a["stop_reason"], "tool_use");
        assert_eq!(a["content"][1]["name"], "Bash");
        assert_eq!(a["usage"]["output_tokens"], 7);
    }
    #[test]
    fn responses_reasoning_item_is_omitted_not_fabricated() {
        // Responses `reasoning` items carry no Anthropic-valid form (no real
        // signature or redacted content), so they are dropped rather than
        // emitted as a fake `thinking` block.
        let resp = serde_json::json!({
            "id": "resp_1",
            "status": "completed",
            "output": [
                {"type": "reasoning", "summary": [{"type": "summary_text", "text": "why"}]},
                {"type": "message", "content": [{"type": "output_text", "text": "answer"}]}
            ],
            "usage": {"input_tokens": 1, "output_tokens": 2}
        });
        let a = responses_to_anthropic(&resp, "gw");
        let content = a["content"].as_array().unwrap();
        assert!(!content.iter().any(|c| c["type"] == "thinking"));
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "answer");
    }

    /// Order signature of the translated input: `fc:<name>`, `fco:<call_id>`,
    /// `role:<role>` — enough to assert interleaving without reading content.
    fn order_sig(r: &Value) -> Vec<String> {
        r["input"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                let s = |k: &str| i.get(k).and_then(Value::as_str).unwrap_or("?").to_string();
                match i.get("type").and_then(Value::as_str) {
                    Some("function_call") => format!("fc:{}", s("name")),
                    Some("function_call_output") => format!("fco:{}", s("call_id")),
                    _ => format!("role:{}", s("role")),
                }
            })
            .collect()
    }

    /// The exact shape the zen backend rejects with `400 invalid parameters`:
    /// a mid-turn user injection (Claude Code's "user sent a new message
    /// while you were working") lands between two pending calls and their
    /// outputs. The user item must be held back until the outputs land.
    #[test]
    fn user_text_between_pending_calls_flushes_after_outputs() {
        let body = serde_json::json!({
            "model": "x",
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "text", "text": "doing two things"},
                    {"type": "tool_use", "id": "t1", "name": "TaskCreate", "input": {"subject": "a"}},
                    {"type": "tool_use", "id": "t2", "name": "Bash", "input": {"command": "ls"}}
                ]},
                {"role": "user", "content": "The user sent a new message while you were working: ..."},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "Task #1 created"},
                    {"type": "tool_result", "tool_use_id": "t2", "content": "file list"}
                ]}
            ],
            "tools": []
        });
        let r = anthropic_to_responses(&body, "upstream");
        assert_eq!(
            order_sig(&r),
            vec![
                "role:assistant",
                "fc:TaskCreate",
                "fc:Bash",
                "fco:t1",
                "fco:t2",
                "role:user",
            ],
            "user injection must not sit between pending calls and outputs: {r}"
        );
    }

    /// A single user message mixing text with the tool results produces the
    /// same sandwich inside one translate step — same deferral applies.
    #[test]
    fn mixed_user_message_emits_outputs_before_deferred_text() {
        let body = serde_json::json!({
            "model": "x",
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "t1", "name": "A", "input": {}},
                    {"type": "tool_use", "id": "t2", "name": "B", "input": {}}
                ]},
                {"role": "user", "content": [
                    {"type": "text", "text": "context injection"},
                    {"type": "tool_result", "tool_use_id": "t1", "content": "r1"},
                    {"type": "tool_result", "tool_use_id": "t2", "content": "r2"}
                ]}
            ],
            "tools": []
        });
        let r = anthropic_to_responses(&body, "upstream");
        assert_eq!(
            order_sig(&r),
            vec!["fc:A", "fc:B", "fco:t1", "fco:t2", "role:user"],
            "text of a mixed user message must follow its outputs: {r}"
        );
    }

    /// System-role items between pending calls and outputs are accepted by
    /// the backend (empirically), so they keep their original position.
    #[test]
    fn system_items_stay_between_pending_calls_and_outputs() {
        let body = serde_json::json!({
            "model": "x",
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "t1", "name": "A", "input": {}},
                    {"type": "tool_use", "id": "t2", "name": "B", "input": {}}
                ]},
                {"role": "system", "content": "env note"},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "r1"},
                    {"type": "tool_result", "tool_use_id": "t2", "content": "r2"}
                ]}
            ],
            "tools": []
        });
        let r = anthropic_to_responses(&body, "upstream");
        assert_eq!(
            order_sig(&r),
            vec!["fc:A", "fc:B", "role:system", "fco:t1", "fco:t2",],
            "system items are not deferred: {r}"
        );
    }

    /// Without pending calls nothing moves: plain user turns keep order.
    #[test]
    fn user_text_without_pending_calls_is_not_reordered() {
        let body = serde_json::json!({
            "model": "x",
            "messages": [
                {"role": "user", "content": "first"},
                {"role": "assistant", "content": [{"type": "text", "text": "ok"}]},
                {"role": "user", "content": "second"}
            ],
            "tools": []
        });
        let r = anthropic_to_responses(&body, "upstream");
        assert_eq!(
            order_sig(&r),
            vec!["role:user", "role:assistant", "role:user"],
            "no pending calls means no reordering: {r}"
        );
    }

    // -- Fase 2: Codex edge -> canonical Anthropic (and back) ---------------

    fn codex_request() -> Value {
        serde_json::json!({
            "model": "claude-x",
            "instructions": "you are codex",
            "input": [
                {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "dev note"}]},
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "let me check"}]},
                {"type": "function_call", "call_id": "call_1", "name": "shell", "arguments": "{\"cmd\":\"ls\"}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "file.txt"},
                {"type": "custom_tool_call", "call_id": "call_2", "name": "apply_patch", "input": "*** Begin Patch"},
                {"type": "custom_tool_call_output", "call_id": "call_2", "output": "Done"},
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "thanks"}]},
                {"type": "reasoning", "summary": []}
            ],
            "tools": [
                {"type": "function", "name": "shell", "description": "d", "parameters": {"type": "object"}},
                {"type": "custom", "name": "apply_patch", "description": "p"},
                {"type": "web_search"},
                {"type": "namespace", "name": "mcp__x", "tools": []}
            ],
            "tool_choice": "auto",
            "max_output_tokens": 8,
            "stream": true
        })
    }

    #[test]
    fn responses_request_becomes_canonical_anthropic() {
        let out = responses_to_anthropic_request(&codex_request(), "up-model");
        assert_eq!(out["model"], "up-model");
        // instructions + developer fold into the system prompt.
        assert_eq!(out["system"], "you are codex\n\ndev note");
        let msgs = out["messages"].as_array().unwrap();
        // Flat input coalesces into alternating user/assistant turns.
        let roles: Vec<&str> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(
            roles,
            ["user", "assistant", "user", "assistant", "user"],
            "{roles:?}"
        );
        // function_call/output -> tool_use/tool_result with the call id.
        assert_eq!(msgs[1]["content"][1]["type"], "tool_use");
        assert_eq!(msgs[1]["content"][1]["id"], "call_1");
        assert_eq!(msgs[1]["content"][1]["input"]["cmd"], "ls");
        assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
        assert_eq!(msgs[2]["content"][0]["tool_use_id"], "call_1");
        // custom tool rides as a single-field function; its history wraps input.
        assert_eq!(msgs[3]["content"][0]["type"], "tool_use");
        assert_eq!(msgs[3]["content"][0]["input"]["input"], "*** Begin Patch");
        assert_eq!(msgs[4]["content"][0]["tool_use_id"], "call_2");
        let tools = out["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["shell", "apply_patch"], "{names:?}");
        assert_eq!(
            tools[1]["input_schema"]["properties"]["input"]["type"],
            "string"
        );
        // web_search / namespace have no Anthropic equivalent.
        assert_eq!(out["tool_choice"]["type"], "auto");
        assert_eq!(out["max_tokens"], 16); // floored at the backend minimum
        assert_eq!(out["stream"], true);
        // reasoning/store/client_metadata never leak into the canonical body.
        assert!(out.get("reasoning").is_none());
        assert!(out.get("store").is_none());
    }

    #[test]
    fn anthropic_response_becomes_responses_custom_aware() {
        let custom: HashSet<String> = ["apply_patch".to_string()].into_iter().collect();
        let resp = serde_json::json!({
            "content": [
                {"type": "text", "text": "patched"},
                {"type": "tool_use", "id": "toolu_1", "name": "shell", "input": {"cmd": "ls"}},
                {"type": "tool_use", "id": "toolu_2", "name": "apply_patch", "input": {"input": "*** End Patch"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 3, "output_tokens": 5}
        });
        let out = anthropic_to_responses_response(&resp, "claude-x", &custom);
        assert_eq!(out["status"], "completed");
        assert_eq!(out["model"], "claude-x");
        assert!(out["id"].as_str().unwrap().starts_with("resp_"));
        let items = out["output"].as_array().unwrap();
        assert_eq!(items[0]["type"], "message");
        assert_eq!(items[0]["content"][0]["text"], "patched");
        assert_eq!(items[1]["type"], "function_call");
        assert_eq!(items[1]["call_id"], "toolu_1");
        assert_eq!(items[1]["arguments"], "{\"cmd\":\"ls\"}");
        // Custom tools round-trip back to custom_tool_call with the raw input.
        assert_eq!(items[2]["type"], "custom_tool_call");
        assert_eq!(items[2]["call_id"], "toolu_2");
        assert_eq!(items[2]["input"], "*** End Patch");
        assert_eq!(out["usage"]["total_tokens"], 8);
    }

    #[test]
    fn anthropic_max_tokens_becomes_incomplete() {
        let resp = serde_json::json!({
            "content": [{"type": "text", "text": "cut off"}],
            "stop_reason": "max_tokens",
            "usage": {"input_tokens": 1, "output_tokens": 2}
        });
        let out = anthropic_to_responses_response(&resp, "m", &HashSet::new());
        assert_eq!(out["status"], "incomplete");
        assert_eq!(out["incomplete_details"]["reason"], "max_output_tokens");
    }
}
