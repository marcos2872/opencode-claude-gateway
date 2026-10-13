//! Streaming translators: Responses SSE and Chat SSE -> Anthropic SSE.

use super::heartbeat::{responses_sse_error, sse, sse_error};
use serde_json::Value;
use std::collections::HashSet;

/// Stateful translator: OpenAI Responses SSE -> Anthropic SSE event lines.
#[derive(Debug, Default)]
pub struct ResponsesTranslator {
    pub gateway_model: String,
    pub msg_id: String,
    pub text_open: bool,
    pub text_closed: bool,
    pub tool_blocks: Vec<ResponsesToolBlock>,
    pub message_started: bool,
    pub input_tokens: u64,
    /// Prefix-cache hits reported by the upstream `usage` (`response.completed`).
    /// Set by the forward loop before `finish`; `0` (miss/unknown) keeps the
    /// `message_delta` usage shape byte-identical to before.
    pub cache_read_tokens: u64,
}

#[derive(Debug, Default, Clone)]
pub struct ResponsesToolBlock {
    pub key: String,
    pub block_index: usize,
    pub id: String,
    pub name: String,
    pub started: bool,
    pub closed: bool,
}

impl ResponsesTranslator {
    pub fn new(gateway_model: &str) -> Self {
        Self {
            gateway_model: gateway_model.to_string(),
            msg_id: format!(
                "msg_{}",
                &uuid::Uuid::new_v4().to_string().replace('-', "")[..24]
            ),
            ..Default::default()
        }
    }

    pub fn prefix(&mut self) -> Vec<String> {
        if self.message_started {
            return vec![];
        }
        self.message_started = true;
        vec![sse(&serde_json::json!({
            "type": "message_start",
            "message": {"id": self.msg_id, "type": "message", "role": "assistant",
                "model": self.gateway_model, "content": [],
                "stop_reason": null, "stop_sequence": null,
                "usage": {"input_tokens": self.input_tokens, "output_tokens": 0}}
        }))]
    }

    fn ensure_text(&mut self, out: &mut Vec<String>) {
        if !self.text_open {
            self.text_open = true;
            out.push(sse(&serde_json::json!({
                "type": "content_block_start", "index": 0,
                "content_block": {"type": "text", "text": ""}
            })));
        }
    }

    fn tool_block(&mut self, key: &str) -> usize {
        if let Some(pos) = self.tool_blocks.iter().position(|b| b.key == key) {
            return pos;
        }
        let block_index = 1 + self.tool_blocks.len();
        self.tool_blocks.push(ResponsesToolBlock {
            key: key.to_string(),
            block_index,
            ..Default::default()
        });
        self.tool_blocks.len() - 1
    }

    fn start_tool(&mut self, pos: usize, out: &mut Vec<String>) {
        if self.tool_blocks[pos].started {
            return;
        }
        if !self.text_open {
            self.ensure_text(out);
        }
        if self.tool_blocks[pos].id.is_empty() {
            self.tool_blocks[pos].id = format!("toolu_{pos}");
        }
        self.tool_blocks[pos].started = true;
        let b = &self.tool_blocks[pos];
        out.push(sse(&serde_json::json!({
            "type": "content_block_start", "index": b.block_index,
            "content_block": {"type": "tool_use", "id": b.id, "name": b.name, "input": {}}
        })));
    }

    /// Feed one parsed Responses event payload, return Anthropic `data:` lines.
    pub fn feed(&mut self, ev: &Value) -> Vec<String> {
        let mut out = vec![];
        match ev.get("type").and_then(|t| t.as_str()) {
            Some("response.output_text.delta") => {
                if let Some(t) = ev.get("delta").and_then(|d| d.as_str()) {
                    if !t.is_empty() {
                        self.ensure_text(&mut out);
                        out.push(sse(&serde_json::json!({
                            "type": "content_block_delta", "index": 0,
                            "delta": {"type": "text_delta", "text": t}
                        })));
                    }
                }
            }
            Some("response.output_item.added") => {
                if let Some(item) = ev.get("item") {
                    if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                        let key = ev
                            .get("output_index")
                            .map(|i| i.to_string())
                            .unwrap_or_else(|| self.tool_blocks.len().to_string());
                        let pos = self.tool_block(&key);
                        if let Some(id) = item.get("call_id").and_then(|x| x.as_str()) {
                            if !id.is_empty() {
                                self.tool_blocks[pos].id = id.to_string();
                            }
                        }
                        if let Some(name) = item.get("name").and_then(|x| x.as_str()) {
                            self.tool_blocks[pos].name = name.to_string();
                        }
                        self.start_tool(pos, &mut out);
                    }
                }
            }
            Some("response.function_call_arguments.delta") => {
                let key = ev
                    .get("output_index")
                    .map(|i| i.to_string())
                    .unwrap_or_else(|| "0".to_string());
                let pos = self.tool_block(&key);
                self.start_tool(pos, &mut out);
                if let Some(args) = ev.get("delta").and_then(|d| d.as_str()) {
                    if !args.is_empty() {
                        let bi = self.tool_blocks[pos].block_index;
                        out.push(sse(&serde_json::json!({
                            "type": "content_block_delta", "index": bi,
                            "delta": {"type": "input_json_delta", "partial_json": args}
                        })));
                    }
                }
            }
            Some("response.output_item.done") => {
                if let Some(item) = ev.get("item") {
                    if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                        let key = ev
                            .get("output_index")
                            .map(|i| i.to_string())
                            .unwrap_or_default();
                        if let Some(pos) = self.tool_blocks.iter().position(|b| b.key == key) {
                            if let Some(id) = item.get("call_id").and_then(|x| x.as_str()) {
                                if !id.is_empty() {
                                    self.tool_blocks[pos].id = id.to_string();
                                }
                            }
                            if let Some(name) = item.get("name").and_then(|x| x.as_str()) {
                                if !name.is_empty() {
                                    self.tool_blocks[pos].name = name.to_string();
                                }
                            }
                            self.tool_blocks[pos].closed = true;
                            let bi = self.tool_blocks[pos].block_index;
                            out.push(sse(
                                &serde_json::json!({"type": "content_block_stop", "index": bi}),
                            ));
                        }
                    }
                }
            }
            // The upstream reports a mid-stream failure. Surface it as an
            // Anthropic `error` event; the caller also appends one after
            // `message_stop` for `response.failed`.
            Some("error") => {
                let e = ev.get("error").unwrap_or(ev);
                let msg = e
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("upstream error");
                let ty = e
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("api_error");
                out.push(sse_error(ty, msg));
            }
            _ => {}
        }
        out
    }

    pub fn has_tools(&self) -> bool {
        self.tool_blocks.iter().any(|b| b.started)
    }

    pub fn finish(
        &mut self,
        stop_reason: &str,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Vec<String> {
        let mut out = vec![];
        if self.text_open && !self.text_closed {
            self.text_closed = true;
            out.push(sse(
                &serde_json::json!({"type": "content_block_stop", "index": 0}),
            ));
        }
        for b in &self.tool_blocks {
            if b.started && !b.closed {
                out.push(sse(
                    &serde_json::json!({"type": "content_block_stop", "index": b.block_index}),
                ));
            }
        }
        let mut usage =
            serde_json::json!({"input_tokens": input_tokens, "output_tokens": output_tokens});
        // Only present on a real prefix-cache hit: the miss case keeps the
        // `message_delta` usage shape byte-identical to before.
        if self.cache_read_tokens > 0 {
            usage["cache_read_input_tokens"] = serde_json::Value::from(self.cache_read_tokens);
        }
        out.push(sse(&serde_json::json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason, "stop_sequence": null},
            "usage": usage
        })));
        out.push(sse(&serde_json::json!({"type": "message_stop"})));
        out
    }
}
// ---------------------------------------------------------------------------
// OpenAI SSE chunk -> Anthropic SSE event lines
// ---------------------------------------------------------------------------

/// Stateful translator for one streaming response.
#[derive(Debug, Default)]
pub struct StreamTranslator {
    pub gateway_model: String,
    pub msg_id: String,
    pub text_open: bool,
    pub text_closed: bool,
    pub tool_blocks: Vec<ToolBlock>,
    pub message_started: bool,
    pub input_tokens: u64,
    /// Prefix-cache hits reported by the upstream `usage` chunk.
    /// Set by the forward loop before `finish`; `0` (miss/unknown) keeps the
    /// `message_delta` usage shape byte-identical to before.
    pub cache_read_tokens: u64,
}

#[derive(Debug, Default, Clone)]
pub struct ToolBlock {
    pub index: usize, // anthropic block index (1-based after text block 0)
    pub id: String,
    pub name: String,
    pub started: bool,
    pub closed: bool,
    /// `arguments` seen before the block opened (name not known yet). Flushed
    /// as one `input_json_delta` when the name arrives.
    pub pending_args: String,
}

impl StreamTranslator {
    pub fn new(gateway_model: &str) -> Self {
        Self {
            gateway_model: gateway_model.to_string(),
            msg_id: format!(
                "msg_{}",
                &uuid::Uuid::new_v4().to_string().replace('-', "")[..24]
            ),
            ..Default::default()
        }
    }

    pub fn prefix(&mut self) -> Vec<String> {
        if self.message_started {
            return vec![];
        }
        self.message_started = true;
        vec![sse(&serde_json::json!({
            "type": "message_start",
            "message": {"id": self.msg_id, "type": "message", "role": "assistant",
                "model": self.gateway_model, "content": [],
                "stop_reason": null, "stop_sequence": null,
                "usage": {"input_tokens": self.input_tokens, "output_tokens": 0}}
        }))]
    }

    fn ensure_text(&mut self, out: &mut Vec<String>) {
        if !self.text_open {
            self.text_open = true;
            out.push(sse(&serde_json::json!({
                "type": "content_block_start", "index": 0,
                "content_block": {"type": "text", "text": ""}
            })));
        }
    }

    /// Feed one parsed OpenAI chunk (`data:` JSON), return Anthropic `data:` lines.
    pub fn feed(&mut self, chunk: &Value) -> Vec<String> {
        let mut out = vec![];
        let choice = chunk
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first());
        let Some(choice) = choice else { return out };
        let delta = choice.get("delta").cloned().unwrap_or(Value::Null);

        // text
        if let Some(t) = delta.get("content").and_then(|c| c.as_str()) {
            if !t.is_empty() {
                self.ensure_text(&mut out);
                out.push(sse(&serde_json::json!({
                    "type": "content_block_delta", "index": 0,
                    "delta": {"type": "text_delta", "text": t}
                })));
            }
        }
        // tool calls
        if let Some(calls) = delta.get("tool_calls").and_then(|c| c.as_array()) {
            for tc in calls {
                let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                let has_identity = tc
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())
                    || tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.is_empty())
                    || tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.is_empty());
                if has_identity && !self.text_open {
                    self.ensure_text(&mut out);
                }
                while self.tool_blocks.len() <= idx {
                    let n = self.tool_blocks.len();
                    self.tool_blocks.push(ToolBlock {
                        index: 1 + n,
                        ..Default::default()
                    });
                }
                let block = &mut self.tool_blocks[idx];
                if let Some(id) = tc.get("id").and_then(|x| x.as_str()) {
                    if !id.is_empty() {
                        block.id = id.to_string();
                    }
                }
                if let Some(name) = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|x| x.as_str())
                {
                    if !name.is_empty() && block.name.is_empty() {
                        block.name = name.to_string();
                    }
                }
                let args = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|a| a.as_str());
                if !block.started {
                    // Anthropic `tool_use` requires a non-empty name, so never
                    // open the block until the name is known even when the id or
                    // arguments arrive first. Arguments seen beforehand are
                    // buffered; opening here flushes them in order. If the
                    // upstream never names the tool, `finish` drops the block so
                    // the client never records a nameless `tool_use` (which a
                    // strict upstream would reject as `tool_calls[].function.name`
                    // on the next turn).
                    if !block.name.is_empty() {
                        block.started = true;
                        if block.id.is_empty() {
                            block.id = format!("toolu_{idx}");
                        }
                        // zen-style upstreams emit id + name + complete
                        // arguments in ONE chunk; the args riding along here
                        // must join the buffer before the flush, or the whole
                        // call goes out with `arguments: "{}"`.
                        if let Some(a) = args {
                            if !a.is_empty() {
                                block.pending_args.push_str(a);
                            }
                        }
                        let bi = block.index;
                        let id = block.id.clone();
                        let name = block.name.clone();
                        out.push(sse(&serde_json::json!({
                            "type": "content_block_start", "index": bi,
                            "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}
                        })));
                        if !block.pending_args.is_empty() {
                            let partial = std::mem::take(&mut block.pending_args);
                            out.push(sse(&serde_json::json!({
                                "type": "content_block_delta", "index": bi,
                                "delta": {"type": "input_json_delta", "partial_json": partial}
                            })));
                        }
                    } else if let Some(a) = args {
                        if !a.is_empty() {
                            block.pending_args.push_str(a);
                        }
                    }
                } else if let Some(a) = args {
                    if !a.is_empty() {
                        let bi = block.index;
                        out.push(sse(&serde_json::json!({
                            "type": "content_block_delta", "index": bi,
                            "delta": {"type": "input_json_delta", "partial_json": a}
                        })));
                    }
                }
            }
        }
        out
    }

    pub fn finish(
        &mut self,
        stop_reason: &str,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Vec<String> {
        let mut out = vec![];
        if self.text_open && !self.text_closed {
            self.text_closed = true;
            out.push(sse(
                &serde_json::json!({"type": "content_block_stop", "index": 0}),
            ));
        }
        for b in &self.tool_blocks {
            // Blocks never started (a nameless `tool_use` we refused to open)
            // emit nothing: closing a block that never began is invalid.
            if b.started && !b.closed {
                out.push(sse(
                    &serde_json::json!({"type": "content_block_stop", "index": b.index}),
                ));
            }
        }
        let mut usage =
            serde_json::json!({"input_tokens": input_tokens, "output_tokens": output_tokens});
        // Only present on a real prefix-cache hit: the miss case keeps the
        // `message_delta` usage shape byte-identical to before.
        if self.cache_read_tokens > 0 {
            usage["cache_read_input_tokens"] = serde_json::Value::from(self.cache_read_tokens);
        }
        out.push(sse(&serde_json::json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason, "stop_sequence": null},
            "usage": usage
        })));
        out.push(sse(&serde_json::json!({"type": "message_stop"})));
        out
    }
}

// ---------------------------------------------------------------------------
// Anthropic SSE events -> Responses SSE — outbound dialect of the Codex edge
// (Fase 2: Chat/Anthropic upstreams behind `POST /v1/responses`).
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
struct OutBlock {
    /// Anthropic `content_block` index this block maps from.
    anth_index: usize,
    output_index: u64,
    /// False for `thinking`/`redacted_thinking` and anything unknown: dropped
    /// wholesale (Anthropic reasoning has no Responses equivalent here).
    emit: bool,
    is_tool: bool,
    /// Tool the client declared as `custom` (apply_patch): the round trip
    /// rebuilds `custom_tool_call` items instead of `function_call`.
    is_custom: bool,
    item_id: String,
    name: String,
    call_id: String,
    text: String,
    args: String,
    closed: bool,
}

/// Stateful translator: Anthropic message events -> Responses SSE frames.
///
/// Contract honored (the Codex parser is strict):
/// - a terminal `response.completed` / `response.incomplete` / `response.failed`
///   **before EOF** — [`ResponsesOutTranslator::finish_abrupt`] synthesizes
///   `response.failed` when the upstream dies first;
/// - tool arguments complete inside `response.output_item.done` (the
///   `function_call_arguments.delta` events are no-ops for Codex, so they are
///   not even emitted — the accumulated string lands on `.done`);
/// - `response.completed` carries `response.id` plus integer
///   `input_tokens`/`output_tokens`/`total_tokens`.
#[derive(Debug)]
pub struct ResponsesOutTranslator {
    pub model: String,
    response_id: String,
    seq: u64,
    created: bool,
    pub finished: bool,
    next_output_index: u64,
    blocks: Vec<OutBlock>,
    input_tokens: u64,
    output_tokens: u64,
    stop_reason: Option<String>,
    output_items: Vec<Value>,
    custom_tools: HashSet<String>,
}

impl ResponsesOutTranslator {
    pub fn new(gateway_model: &str, custom_tools: HashSet<String>) -> Self {
        Self {
            model: gateway_model.to_string(),
            response_id: format!(
                "resp_{}",
                &uuid::Uuid::new_v4().to_string().replace('-', "")[..24]
            ),
            seq: 0,
            created: false,
            finished: false,
            next_output_index: 0,
            blocks: vec![],
            input_tokens: 0,
            output_tokens: 0,
            stop_reason: None,
            output_items: vec![],
            custom_tools,
        }
    }

    fn frame(&mut self, mut v: Value) -> String {
        self.seq += 1;
        if let Some(obj) = v.as_object_mut() {
            obj.insert("sequence_number".to_string(), Value::from(self.seq));
        }
        sse(&v)
    }

    fn ensure_created(&mut self, out: &mut Vec<String>) {
        if self.created {
            return;
        }
        self.created = true;
        let frame = self.frame(serde_json::json!({
            "type": "response.created",
            "response": {
                "id": self.response_id,
                "object": "response",
                "status": "in_progress",
                "model": self.model,
                "output": []
            }
        }));
        out.push(frame);
    }

    /// Close any block the upstream never stopped (defensive: a terminal
    /// frame must still carry complete items).
    fn close_open_blocks(&mut self, out: &mut Vec<String>) {
        for i in 0..self.blocks.len() {
            if self.blocks[i].closed || !self.blocks[i].emit {
                continue;
            }
            let block = self.blocks[i].clone();
            self.blocks[i].closed = true;
            out.push(self.done_frame(&block));
        }
    }

    fn done_frame(&mut self, block: &OutBlock) -> String {
        let item = if block.is_custom {
            // Custom tools carry the freeform text in `input`; upstream saw a
            // single-field function, so unwrap `{input: "..."}` back out.
            let raw = if block.args.trim().is_empty() {
                String::new()
            } else {
                serde_json::from_str::<Value>(&block.args)
                    .ok()
                    .and_then(|v| v.get("input").and_then(Value::as_str).map(str::to_string))
                    .unwrap_or_else(|| block.args.clone())
            };
            serde_json::json!({
                "type": "custom_tool_call",
                "call_id": block.call_id,
                "name": block.name,
                "input": raw,
                "status": "completed"
            })
        } else if block.is_tool {
            let arguments = if block.args.trim().is_empty() {
                "{}".to_string()
            } else {
                block.args.clone()
            };
            serde_json::json!({
                "type": "function_call",
                "id": block.item_id,
                "call_id": block.call_id,
                "name": block.name,
                "arguments": arguments,
                "status": "completed"
            })
        } else {
            serde_json::json!({
                "type": "message",
                "id": block.item_id,
                "status": "completed",
                "role": "assistant",
                "content": [{"type": "output_text", "text": block.text}]
            })
        };
        self.output_items.push(item.clone());
        self.frame(serde_json::json!({
            "type": "response.output_item.done",
            "output_index": block.output_index,
            "item": item
        }))
    }

    /// Terminal frame for a normal end (completed / incomplete).
    fn terminal_frame(&mut self) -> String {
        let incomplete = self.stop_reason.as_deref() == Some("max_tokens");
        let mut response = serde_json::json!({
            "id": self.response_id,
            "object": "response",
            "model": self.model,
            "status": if incomplete { "incomplete" } else { "completed" },
            "output": self.output_items,
            "usage": {
                "input_tokens": self.input_tokens,
                "output_tokens": self.output_tokens,
                "total_tokens": self.input_tokens + self.output_tokens
            }
        });
        if incomplete {
            response["incomplete_details"] = serde_json::json!({"reason": "max_output_tokens"});
        }
        let kind = if incomplete {
            "response.incomplete"
        } else {
            "response.completed"
        };
        self.frame(serde_json::json!({"type": kind, "response": response}))
    }

    /// Feed one parsed Anthropic event payload, return Responses frames.
    pub fn feed(&mut self, ev: &Value) -> Vec<String> {
        if self.finished {
            return vec![];
        }
        let mut out = vec![];
        match ev.get("type").and_then(Value::as_str) {
            Some("error") => {
                let msg = ev
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("upstream stream error")
                    .to_string();
                self.finished = true;
                out.push(responses_sse_error("upstream_error", &msg));
                return out;
            }
            Some("message_start") => {
                self.ensure_created(&mut out);
                if let Some(u) = ev.get("message").and_then(|m| m.get("usage")) {
                    if let Some(n) = u.get("input_tokens").and_then(Value::as_u64) {
                        self.input_tokens = n;
                    }
                }
            }
            Some("content_block_start") => {
                self.ensure_created(&mut out);
                let Some(index) = ev.get("index").and_then(Value::as_u64) else {
                    return out;
                };
                let Some(cb) = ev.get("content_block") else {
                    return out;
                };
                let kind = cb.get("type").and_then(Value::as_str).unwrap_or("");
                let emit = matches!(kind, "text" | "tool_use");
                let is_tool = kind == "tool_use";
                let name = cb.get("name").and_then(Value::as_str).unwrap_or("");
                let is_custom = is_tool && self.custom_tools.contains(name);
                let output_index = self.next_output_index;
                self.next_output_index += 1;
                let mut block = OutBlock {
                    anth_index: index as usize,
                    output_index,
                    emit,
                    is_tool,
                    is_custom,
                    item_id: if is_tool {
                        format!("fc_{index}")
                    } else {
                        format!("msg_{index}")
                    },
                    name: name.to_string(),
                    call_id: cb
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    ..Default::default()
                };
                if block.call_id.is_empty() && is_tool {
                    block.call_id = block.item_id.clone();
                }
                if emit {
                    let added = if is_custom {
                        serde_json::json!({
                            "type": "custom_tool_call",
                            "call_id": block.call_id,
                            "name": block.name,
                            "input": "",
                            "status": "in_progress"
                        })
                    } else if is_tool {
                        serde_json::json!({
                            "type": "function_call",
                            "id": block.item_id,
                            "call_id": block.call_id,
                            "name": block.name,
                            "arguments": "",
                            "status": "in_progress"
                        })
                    } else {
                        serde_json::json!({
                            "type": "message",
                            "id": block.item_id,
                            "status": "in_progress",
                            "role": "assistant",
                            "content": []
                        })
                    };
                    out.push(self.frame(serde_json::json!({
                        "type": "response.output_item.added",
                        "output_index": block.output_index,
                        "item": added
                    })));
                }
                self.blocks.push(block);
            }
            Some("content_block_delta") => {
                let Some(index) = ev.get("index").and_then(Value::as_u64) else {
                    return out;
                };
                let Some(delta) = ev.get("delta") else {
                    return out;
                };
                let pos = self
                    .blocks
                    .iter()
                    .position(|b| b.anth_index == index as usize && !b.closed);
                let Some(pos) = pos else {
                    return out;
                };
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let text = delta.get("text").and_then(Value::as_str).unwrap_or("");
                        if !text.is_empty() && self.blocks[pos].emit && !self.blocks[pos].is_tool {
                            self.blocks[pos].text.push_str(text);
                            let block = self.blocks[pos].clone();
                            out.push(self.frame(serde_json::json!({
                                "type": "response.output_text.delta",
                                "item_id": block.item_id,
                                "output_index": block.output_index,
                                "delta": text
                            })));
                        }
                    }
                    Some("input_json_delta") => {
                        // Codex ignores argument deltas entirely; accumulate
                        // and land the full string on `output_item.done`.
                        let partial = delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if !partial.is_empty() && self.blocks[pos].emit && self.blocks[pos].is_tool
                        {
                            self.blocks[pos].args.push_str(partial);
                        }
                    }
                    // thinking / signature deltas: dropped with their block.
                    _ => {}
                }
            }
            Some("content_block_stop") => {
                let Some(index) = ev.get("index").and_then(Value::as_u64) else {
                    return out;
                };
                let pos = self
                    .blocks
                    .iter()
                    .position(|b| b.anth_index == index as usize && !b.closed);
                if let Some(pos) = pos {
                    self.blocks[pos].closed = true;
                    if self.blocks[pos].emit {
                        let frame = self.done_frame(&self.blocks[pos].clone());
                        out.push(frame);
                    }
                }
            }
            Some("message_delta") => {
                if let Some(d) = ev.get("delta") {
                    if let Some(r) = d.get("stop_reason").and_then(Value::as_str) {
                        self.stop_reason = Some(r.to_string());
                    }
                }
                if let Some(u) = ev.get("usage") {
                    if let Some(n) = u.get("input_tokens").and_then(Value::as_u64) {
                        // Real Anthropic carries input on `message_start`; the
                        // Chat bridge's intermediate translator only knows it
                        // at the end, so accept it here too.
                        self.input_tokens = n;
                    }
                    if let Some(n) = u.get("output_tokens").and_then(Value::as_u64) {
                        self.output_tokens = n;
                    }
                }
            }
            Some("message_stop") => {
                self.ensure_created(&mut out);
                self.close_open_blocks(&mut out);
                self.finished = true;
                out.push(self.terminal_frame());
            }
            _ => {}
        }
        out
    }

    /// Synthetic terminal frame for an upstream that ended (EOF or read
    /// error) without `message_stop`: Codex would otherwise wait its full
    /// 300s idle timeout on a stream that already closed.
    pub fn finish_abrupt(&mut self, message: &str) -> Vec<String> {
        if self.finished {
            return vec![];
        }
        self.finished = true;
        vec![responses_sse_error("stream_closed", message)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    #[test]
    fn responses_error_event_is_surfaced() {
        let mut tr = ResponsesTranslator::new("gw");
        let _ = tr.prefix();
        let out = tr.feed(&serde_json::json!({
            "type": "error",
            "error": {"type": "api_error", "message": "overloaded"}
        }));
        assert!(out.iter().any(|e| e.contains("event: error")));
        assert!(out.iter().any(|e| e.contains("overloaded")));
    }
    #[test]
    fn stream_translator_text_and_tool() {
        let mut t = StreamTranslator::new("gw");
        assert_eq!(t.prefix().len(), 1);
        let c1 = serde_json::json!({"choices": [{"delta": {"content": "hi"}}]});
        let e1 = t.feed(&c1);
        assert!(e1.iter().any(|e| e.contains("text_delta")));
        let c2 = serde_json::json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "c1", "function": {"name": "Read", "arguments": "{\"p\":"}}]}}]});
        let e2 = t.feed(&c2);
        assert!(e2.iter().any(|e| e.contains("tool_use")));
        let end = t.finish("tool_use", 0, 3);
        assert!(end.iter().any(|e| e.contains("message_stop")));
    }
    #[test]
    fn responses_stream_translator_text_and_tool() {
        let mut t = ResponsesTranslator::new("gw");
        assert_eq!(t.prefix().len(), 1);
        let d1 = serde_json::json!({"type": "response.output_text.delta", "delta": "hi"});
        assert!(t.feed(&d1).iter().any(|e| e.contains("text_delta")));
        let added = serde_json::json!({
            "type": "response.output_item.added", "output_index": 1,
            "item": {"type": "function_call", "call_id": "c1", "name": "Read"}
        });
        assert!(t.feed(&added).iter().any(|e| e.contains("tool_use")));
        let d2 = serde_json::json!({
            "type": "response.function_call_arguments.delta",
            "output_index": 1, "delta": "{\"p\":"
        });
        assert!(t.feed(&d2).iter().any(|e| e.contains("input_json_delta")));
        assert!(t.has_tools());
        let end = t.finish("tool_use", 0, 3);
        assert!(end.iter().any(|e| e.contains("message_stop")));
    }
    #[test]
    fn stream_translator_buffers_args_until_name_arrives() {
        // Strict gateways may stream arguments before the id/name. Anthropic
        // `tool_use` requires a name, so the block must NOT open on args alone;
        // the arguments are buffered and flushed once the name shows up.
        let mut t = StreamTranslator::new("gw");
        let _ = t.prefix();
        let ev = t.feed(&serde_json::json!({
            "choices": [{"delta": {"tool_calls": [
                {"index": 0, "function": {"arguments": "{\"p\":"}}
            ]}}]
        }));
        assert!(
            !ev.iter().any(|e| e.contains("tool_use")),
            "nameless tool block must not open: {ev:?}"
        );
        // The name arrives (name-only delta, no args): block opens and the
        // buffered arguments are flushed right after the start.
        let ev = t.feed(&serde_json::json!({
            "choices": [{"delta": {"tool_calls": [
                {"index": 0, "function": {"name": "Read"}}
            ]}}]
        }));
        let started = ev
            .iter()
            .find(|e| e.contains("content_block_start") && e.contains("tool_use"))
            .expect("tool block opens once named");
        assert!(started.contains("\"index\":1"), "{started}");
        assert!(started.contains("\"name\":\"Read\""), "{started}");
        assert!(
            ev.iter()
                .any(|e| e.contains("input_json_delta") && e.contains("{\\\"p\\\":")),
            "buffered args flushed on open: {ev:?}"
        );
        // Second (text) delta still lands on index 0, not on the tool block.
        let ev = t.feed(&serde_json::json!({"choices": [{"delta": {"content": "x"}}]}));
        assert!(ev
            .iter()
            .any(|e| e.contains("\"index\":0") && e.contains("text_delta")));
    }
    #[test]
    fn stream_translator_drops_tool_never_named() {
        // If the name never arrives, the block is never opened and `finish`
        // emits no tool block at all — the client must not record a nameless
        // `tool_use` that a strict upstream would reject next turn.
        let mut t = StreamTranslator::new("gw");
        let _ = t.prefix();
        let ev = t.feed(&serde_json::json!({
            "choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "call_1", "function": {"arguments": "{\"a\":1}"}}
            ]}}]
        }));
        assert!(!ev.iter().any(|e| e.contains("tool_use")), "{ev:?}");
        assert!(!t.tool_blocks[0].started);
        let end = t.finish("tool_use", 0, 3);
        assert!(
            !end.iter().any(|e| e.contains("content_block_start"))
                && !end.iter().any(|e| e.contains("\"type\":\"tool_use\"")),
            "finish must not surface a nameless tool block: {end:?}"
        );
    }
    #[test]
    fn stream_translator_indices_are_monotonic() {
        let mut t = StreamTranslator::new("gw");
        let _ = t.prefix();
        let ev = t.feed(&serde_json::json!({
            "choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "a", "function": {"name": "Read", "arguments": "{}"}},
                {"index": 1, "id": "b", "function": {"name": "Bash", "arguments": "{}"}}
            ]}}]
        }));
        let indices: Vec<usize> = ev
            .iter()
            .filter(|e| e.contains("content_block_start"))
            .filter_map(|e| {
                let json = e.split_once("data: ").map(|(_, d)| d.trim())?;
                serde_json::from_str::<Value>(json)
                    .ok()?
                    .get("index")
                    .and_then(Value::as_u64)
                    .map(|n| n as usize)
            })
            .collect();
        assert_eq!(indices, vec![0, 1, 2], "{ev:?}");
        assert!(indices.windows(2).all(|w| w[0] < w[1]));
    }
    #[test]
    fn responses_stream_usage_lands_in_message_delta() {
        let mut t = ResponsesTranslator::new("gw");
        let _ = t.prefix();
        t.input_tokens = 11;
        let end = t.finish("end_turn", 11, 4);
        let delta = end
            .iter()
            .find(|e| e.contains("message_delta"))
            .expect("message_delta");
        assert!(delta.contains("\"input_tokens\":11"), "{delta}");
        assert!(delta.contains("\"output_tokens\":4"), "{delta}");
    }

    // -- Fase 2: Anthropic events -> Responses (Codex outbound) -------------

    fn out_tr() -> ResponsesOutTranslator {
        ResponsesOutTranslator::new("claude-x", HashSet::new())
    }

    #[test]
    fn out_translator_text_stream_to_completed() {
        let mut tr = out_tr();
        let created = tr.feed(&serde_json::json!({
            "type": "message_start",
            "message": {"id": "msg_1", "usage": {"input_tokens": 11, "output_tokens": 0}}
        }));
        assert!(created[0].contains("response.created"), "{}", created[0]);
        let added = tr.feed(&serde_json::json!({
            "type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}
        }));
        assert!(added[0].contains("output_item.added"), "{}", added[0]);
        let delta = tr.feed(&serde_json::json!({
            "type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "olá"}
        }));
        assert!(delta[0].contains("output_text.delta"), "{}", delta[0]);
        let stop = tr.feed(&serde_json::json!({"type": "content_block_stop", "index": 0}));
        assert!(stop[0].contains("output_item.done"), "{}", stop[0]);
        assert!(stop[0].contains("olá"), "{}", stop[0]);
        let _ = tr.feed(&serde_json::json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn"},
            "usage": {"input_tokens": 11, "output_tokens": 4}
        }));
        let term = tr.feed(&serde_json::json!({"type": "message_stop"}));
        assert!(tr.finished);
        let last = term.last().unwrap();
        assert!(last.contains("response.completed"), "{last}");
        assert!(last.contains("\"input_tokens\":11"), "{last}");
        assert!(last.contains("\"total_tokens\":15"), "{last}");
        // Terminal contract: nothing else is emitted after completion.
        assert!(tr.feed(&serde_json::json!({"type": "ping"})).is_empty());
    }

    #[test]
    fn out_translator_tool_arguments_land_on_done() {
        let mut tr = out_tr();
        let _ = tr.feed(&serde_json::json!({
            "type": "message_start", "message": {"id": "msg_1", "usage": {"input_tokens": 1}}
        }));
        let _ = tr.feed(&serde_json::json!({
            "type": "content_block_start", "index": 0,
            "content_block": {"type": "tool_use", "id": "toolu_9", "name": "shell"}
        }));
        // Codex no-ops argument deltas; only the accumulated `.done` matters.
        let d1 = tr.feed(&serde_json::json!({
            "type": "content_block_delta", "index": 0,
            "delta": {"type": "input_json_delta", "partial_json": "{\"cmd\":\"l"}
        }));
        assert!(d1.is_empty(), "{d1:?}");
        let _ = tr.feed(&serde_json::json!({
            "type": "content_block_delta", "index": 0,
            "delta": {"type": "input_json_delta", "partial_json": "s\"}"}
        }));
        let done = tr.feed(&serde_json::json!({"type": "content_block_stop", "index": 0}));
        assert!(done[0].contains("output_item.done"), "{}", done[0]);
        assert!(done[0].contains("{\\\"cmd\\\":\\\"ls\\\"}"), "{}", done[0]);
        assert!(done[0].contains("\"call_id\":\"toolu_9\""), "{}", done[0]);
        let _ = tr.feed(&serde_json::json!({
            "type": "message_delta", "delta": {"stop_reason": "tool_use"},
            "usage": {"input_tokens": 1, "output_tokens": 2}
        }));
        let term = tr.feed(&serde_json::json!({"type": "message_stop"}));
        assert!(term.last().unwrap().contains("response.completed"));
    }

    #[test]
    fn out_translator_thinking_is_dropped() {
        let mut tr = out_tr();
        let _ = tr.feed(&serde_json::json!({
            "type": "message_start", "message": {"id": "m", "usage": {"input_tokens": 1}}
        }));
        let start = tr.feed(&serde_json::json!({
            "type": "content_block_start", "index": 0,
            "content_block": {"type": "thinking", "thinking": "hidden"}
        }));
        assert!(start.is_empty(), "{start:?}");
        let delta = tr.feed(&serde_json::json!({
            "type": "content_block_delta", "index": 0,
            "delta": {"type": "thinking_delta", "thinking": "more"}
        }));
        assert!(delta.is_empty(), "{delta:?}");
        let stop = tr.feed(&serde_json::json!({"type": "content_block_stop", "index": 0}));
        assert!(stop.is_empty(), "{stop:?}");
        let _ = tr.feed(&serde_json::json!({
            "type": "message_delta", "delta": {"stop_reason": "end_turn"},
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }));
        let term = tr.feed(&serde_json::json!({"type": "message_stop"}));
        // No output items were ever produced (thinking only).
        assert!(
            term.last().unwrap().contains("\"output\":[]"),
            "{}",
            term.last().unwrap()
        );
    }

    #[test]
    fn out_translator_error_and_abrupt_end_fail() {
        let mut tr = out_tr();
        let failed = tr.feed(&serde_json::json!({
            "type": "error", "error": {"type": "api_error", "message": "overloaded"}
        }));
        assert!(failed[0].contains("response.failed"), "{}", failed[0]);
        assert!(failed[0].contains("overloaded"), "{}", failed[0]);
        assert!(tr.finished);
        // Terminal already sent: nothing more, even on EOF.
        assert!(tr.finish_abrupt("closed").is_empty());

        let mut tr2 = out_tr();
        let gap = tr2.finish_abrupt("upstream stream closed before response.completed");
        assert!(gap[0].contains("response.failed"), "{}", gap[0]);
        assert!(gap[0].contains("stream_closed"), "{}", gap[0]);
        assert!(tr2.finished);
    }

    #[test]
    fn out_translator_max_tokens_is_incomplete() {
        let mut tr = out_tr();
        let _ = tr.feed(&serde_json::json!({
            "type": "message_start", "message": {"id": "m", "usage": {"input_tokens": 2}}
        }));
        let _ = tr.feed(&serde_json::json!({
            "type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}
        }));
        let _ = tr.feed(&serde_json::json!({
            "type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "cut"}
        }));
        let _ = tr.feed(&serde_json::json!({"type": "content_block_stop", "index": 0}));
        let _ = tr.feed(&serde_json::json!({
            "type": "message_delta", "delta": {"stop_reason": "max_tokens"},
            "usage": {"output_tokens": 3}
        }));
        let term = tr.feed(&serde_json::json!({"type": "message_stop"}));
        let last = term.last().unwrap();
        assert!(last.contains("response.incomplete"), "{last}");
        assert!(last.contains("max_output_tokens"), "{last}");
    }

    #[test]
    fn stream_translator_keeps_args_that_arrive_with_the_name() {
        // zen-style upstreams emit the whole tool call in ONE chunk: id,
        // name and the complete arguments together. The name branch used to
        // drop the args riding in that same delta, so the client got
        // `arguments: "{}"` and failed tool parsing (`missing field cmd`).
        let mut t = StreamTranslator::new("gw");
        let _ = t.prefix();
        let ev = t.feed(&serde_json::json!({
            "choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "call_1",
                 "function": {"name": "exec_command", "arguments": "{\"cmd\":\"ls\"}"}}
            ]}}]
        }));
        assert!(
            ev.iter()
                .any(|e| e.contains("content_block_start") && e.contains("tool_use")),
            "tool block opens: {ev:?}"
        );
        assert!(
            ev.iter()
                .any(|e| e.contains("input_json_delta") && e.contains("cmd")),
            "args arriving with the name must flush: {ev:?}"
        );
    }
}
