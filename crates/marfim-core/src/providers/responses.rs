//! OpenAI Responses wire (`POST {base}/responses`): stateless SSE with
//! input items, reasoning deltas, flat function tools, output-item
//! assembly. Differential-tested against fixtures.
//!
//! Wire shape distilled from Oh My Pi's `openai-responses.ts` (see docs/
//! DISTILLATION.md §R2): request `{model, input[], instructions, stream,
//! store:false, reasoning{effort}, max_output_tokens, tools, tool_choice}`,
//! stream events `response.output_item.added|done`,
//! `response.output_text.delta`, `response.reasoning_summary_text.delta` /
//! `response.reasoning_text.delta`, `response.function_call_arguments.delta`,
//! terminal `response.completed|incomplete|failed|error`.
//!
//! Marfim runs stateless (`store:false`) and rebuilds full context each
//! request — the harness owns history, never `previous_response_id`.

use super::*;

pub struct OpenAiResponses {
    pub client: reqwest::Client,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub max_retries: u32,
}

/// Body builder — pure, so the fixture tests assert the exact wire shape.
pub fn build_body(req: &ChatRequest, model: &str) -> serde_json::Value {
    let mut input: Vec<serde_json::Value> = Vec::new();
    for m in &req.messages {
        match (m.role.as_str(), &m.tool_call_id, m.is_error) {
            ("tool", Some(id), is_err) => {
                // Responses wire: function results ride call_id; errors are
                // marked in-band exactly like the chat wire (D10).
                let output = if is_err {
                    format!("[error] {}", m.content)
                } else {
                    m.content.clone()
                };
                input.push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": id,
                    "output": output,
                }));
            }
            ("assistant", _, _) if !m.tool_calls.is_empty() => {
                if !m.content.is_empty() {
                    input.push(serde_json::json!({
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": m.content}]
                    }));
                }
                for tc in &m.tool_calls {
                    input.push(serde_json::json!({
                        "type": "function_call",
                        "call_id": tc.id,
                        "name": tc.name,
                        "arguments": tc.arguments_json,
                    }));
                }
            }
            (role, _, _) => {
                let content_type = if role == "assistant" {
                    "output_text"
                } else {
                    "input_text"
                };
                input.push(serde_json::json!({
                    "role": role,
                    "content": [{"type": content_type, "text": m.content}]
                }));
            }
        }
    }

    let mut body = serde_json::json!({
        "model": model,
        "input": input,
        "stream": true,
        // Stateless by contract: Marfim replays full history every turn.
        "store": false,
    });
    // §R2 prompt-cache affinity (QUEUED → ADOPTED): when the harness
    // supplies a session-derived key it reaches the wire verbatim; the
    // `pc_` prefix and ≤64 length are enforced by the producer.
    if let Some(k) = &req.prompt_cache_key {
        body["prompt_cache_key"] = serde_json::json!(k);
    }
    if let Some(sys) = &req.system {
        body["instructions"] = serde_json::json!(sys);
    }
    let max_out = req.max_completion_tokens.unwrap_or(8192);
    body["max_output_tokens"] = serde_json::json!(max_out);
    if let Some(e) = req.effort {
        body["reasoning"] =
            serde_json::json!({"effort": super::effort::responses_reasoning_effort(e)});
    } else if let Some(tb) = req.thinking_budget {
        // Legacy path: a raw budget still reaches the wire as an effort.
        let e = if tb <= 2_048 {
            super::effort::Effort::Low
        } else if tb <= 8_192 {
            super::effort::Effort::Medium
        } else {
            super::effort::Effort::High
        };
        body["reasoning"] =
            serde_json::json!({"effort": super::effort::responses_reasoning_effort(e)});
    }
    if !req.tools.is_empty() {
        // Responses tools are FLAT: {type:"function", name, description,
        // parameters} — not nested under `function` like chat/completions.
        let tools: Vec<serde_json::Value> = req
            .tools
            .iter()
            .map(|t| {
                if t.get("type").and_then(|v| v.as_str()) == Some("function")
                    && t.get("name").is_some()
                {
                    t.clone()
                } else {
                    let mut f = serde_json::json!({
                        "type": "function",
                        "name": t["name"].clone(),
                        "parameters": if t["input_schema"].is_object() {
                            t["input_schema"].clone()
                        } else {
                            serde_json::json!({"type": "object"})
                        },
                        // Strict mode off: Marfim re-requests unparseable
                        // args at the harness layer, not via strict JSON
                        // schema (same contract as the chat wire).
                        "strict": false,
                    });
                    if let Some(d) = t.get("description") {
                        f["description"] = d.clone();
                    }
                    f
                }
            })
            .collect();
        body["tools"] = serde_json::json!(tools);
    }
    body
}

/// Fail-fast outbound check (D10): reject a body that could never round
/// trip before it hits the network.
pub fn validate_body(body: &serde_json::Value) -> std::result::Result<(), String> {
    if body["model"].as_str().unwrap_or("").is_empty() {
        return Err("responses body missing `model`".into());
    }
    if !body["input"].is_array() {
        return Err("responses body missing `input` array".into());
    }
    for (i, item) in body["input"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .enumerate()
    {
        let ty = item["type"].as_str().unwrap_or("");
        match ty {
            "function_call_output" => {
                if item["call_id"].as_str().unwrap_or("").is_empty() {
                    return Err(format!("input[{i}] function_call_output missing call_id"));
                }
            }
            "function_call" => {
                if item["name"].as_str().unwrap_or("").is_empty() {
                    return Err(format!("input[{i}] function_call missing name"));
                }
            }
            "" => {
                if item["role"].as_str().is_none() {
                    return Err(format!("input[{i}] item has neither type nor role"));
                }
                if item["content"]
                    .as_array()
                    .map(|a| a.is_empty())
                    .unwrap_or(true)
                {
                    return Err(format!("input[{i}] message content must be non-empty"));
                }
            }
            _ => return Err(format!("input[{i}] unknown item type `{ty}`")),
        }
    }
    Ok(())
}

/// One parsed SSE payload of the Responses stream.
#[derive(serde::Deserialize)]
pub(crate) struct REvent {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub delta: Option<String>,
    #[serde(default)]
    pub output_index: Option<usize>,
    #[serde(default)]
    pub item: serde_json::Value,
    #[serde(default)]
    pub response: serde_json::Value,
}

/// Accumulates in-flight function-call items keyed by output_index.
pub(crate) struct ResponsesAcc {
    pub step: Step,
    calls: std::collections::BTreeMap<usize, ToolCall>,
    last_error: Option<String>,
}

impl ResponsesAcc {
    pub fn new() -> Self {
        Self {
            step: Step::default(),
            calls: std::collections::BTreeMap::new(),
            last_error: None,
        }
    }
}

/// Pure event application: mutates the accumulator, returns the stream
/// events a consumer should see (same contract as the other wires).
pub(crate) fn apply_event(ev: &REvent, st: &mut ResponsesAcc) -> Vec<StreamEvent> {
    let mut out = Vec::new();
    match ev.kind.as_str() {
        "response.output_text.delta" => {
            if let Some(t) = ev.delta.as_deref() {
                if !t.is_empty() {
                    st.step.blocks.push(Block::Text(t.into()));
                    out.push(StreamEvent::Text(t.into()));
                }
            }
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            if let Some(t) = ev.delta.as_deref() {
                if !t.is_empty() {
                    st.step.blocks.push(Block::Thinking(t.into()));
                    out.push(StreamEvent::Thinking(t.into()));
                }
            }
        }
        "response.output_item.added" => {
            if ev.item["type"].as_str() == Some("function_call") {
                let idx = ev.output_index.unwrap_or(usize::MAX);
                st.calls.insert(
                    idx,
                    ToolCall {
                        id: ev.item["call_id"].as_str().unwrap_or("").into(),
                        name: ev.item["name"].as_str().unwrap_or("").into(),
                        arguments_json: String::new(),
                        thought_signature: None,
                    },
                );
            }
        }
        "response.function_call_arguments.delta" => {
            let idx = ev.output_index.unwrap_or(usize::MAX);
            if let Some(slot) = st.calls.get_mut(&idx) {
                slot.arguments_json
                    .push_str(ev.delta.as_deref().unwrap_or(""));
            }
        }
        "response.output_item.done" => {
            if ev.item["type"].as_str() == Some("function_call") {
                let idx = ev.output_index.unwrap_or(usize::MAX);
                // The terminal item carries the COMPLETE arguments —
                // prefer them over the accumulated deltas (OMP's "final
                // parse unconditional" rule), then drop the slot.
                if let Some(mut slot) = st.calls.remove(&idx) {
                    if let Some(full) = ev.item["arguments"].as_str() {
                        slot.arguments_json = full.to_string();
                    }
                    st.step.blocks.push(Block::ToolCall(slot));
                }
            }
        }
        "response.completed" | "response.incomplete" => {
            let resp = &ev.response;
            let usage = &resp["usage"];
            let details_in = &usage["input_tokens_details"];
            st.step.usage = Usage {
                input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
                output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
                cache_read_tokens: details_in["cached_tokens"].as_u64().unwrap_or(0),
                cache_creation_tokens: 0,
            };
            // Terminal status: tool calls win over the raw status so the
            // harness executes the batch it just received.
            let has_calls = st
                .step
                .blocks
                .iter()
                .any(|b| matches!(b, Block::ToolCall(_)));
            st.step.stop = Some(if has_calls {
                Stop::ToolUse
            } else if ev.kind == "response.incomplete" {
                Stop::MaxTokens
            } else {
                Stop::EndTurn
            });
        }
        "response.failed" | "response.error" => {
            let msg = ev.response["error"]["message"]
                .as_str()
                .or_else(|| ev.response["message"].as_str())
                .unwrap_or("responses stream error");
            st.last_error = Some(msg.to_string());
        }
        _ => {}
    }
    out
}

fn finish_to_stop(has_calls: bool) -> Stop {
    if has_calls {
        Stop::ToolUse
    } else {
        Stop::EndTurn
    }
}

impl OpenAiResponses {
    fn url(&self) -> String {
        format!("{}/responses", self.base_url.trim_end_matches('/'))
    }

    /// Same single retry layer as the other wires (INV-06).
    pub(crate) async fn send_with_retry(
        &self,
        body: &serde_json::Value,
    ) -> Result<reqwest::Response> {
        let mut attempt: u32 = 0;
        loop {
            let res = self
                .client
                .post(self.url())
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("User-Agent", crate::user_agent())
                .json(body)
                .send()
                .await;
            match res {
                Ok(r) if r.status().is_success() => return Ok(r),
                Ok(r) => {
                    let status = r.status().as_u16();
                    if !retry_policy(status) {
                        let text = r.text().await.unwrap_or_default();
                        return Err(CoreError::Provider(format!(
                            "HTTP {status}: {}",
                            super::openai::truncate(&text, 600)
                        )));
                    }
                    if attempt >= self.max_retries {
                        return Err(CoreError::Provider(format!("HTTP {status} after retries")));
                    }
                    let wait = retry_after_ms(r.headers()).unwrap_or_else(|| backoff_ms(attempt));
                    tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
                    attempt += 1;
                }
                Err(e) => {
                    if attempt >= self.max_retries {
                        return Err(CoreError::Provider(format!("network error: {e}")));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms(attempt))).await;
                    attempt += 1;
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl Provider for OpenAiResponses {
    fn name(&self) -> &str {
        "openai-responses"
    }

    async fn chat_step(&self, req: &ChatRequest) -> Result<Step> {
        self.chat_step_stream(req, &mut |_| {}).await
    }

    async fn chat_step_stream(
        &self,
        req: &ChatRequest,
        on_event: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<Step> {
        let body = build_body(req, &self.model);
        validate_body(&body)
            .map_err(|e| CoreError::Provider(format!("outbound responses body rejected: {e}")))?;
        let res = self.send_with_retry(&body).await?;
        let mut st = ResponsesAcc::new();
        let mut buf = String::new();
        let mut stream = res.bytes_stream();
        // §F stream watchdog: first-event + idle deadlines (300s defaults).
        let mut wd = StreamWatchdog::from_env();
        while let Some(chunk) = wd.next(&mut stream).await? {
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buf.find('\n') {
                let line: String = buf.drain(..=pos).collect();
                let line = line.trim();
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                if let Ok(ev) = serde_json::from_str::<REvent>(data.trim()) {
                    let events = apply_event(&ev, &mut st);
                    for e in events {
                        on_event(e);
                    }
                }
            }
        }
        // Any open call slot that never got its done event still counts:
        // an interrupted stream must not silently drop a tool call.
        for (_, slot) in std::mem::take(&mut st.calls) {
            st.step.blocks.push(Block::ToolCall(slot));
        }
        if st.step.stop.is_none() && !st.step.blocks.is_empty() {
            let has_calls = st
                .step
                .blocks
                .iter()
                .any(|b| matches!(b, Block::ToolCall(_)));
            st.step.stop = Some(finish_to_stop(has_calls));
        }
        if st.step.blocks.is_empty() {
            if let Some(msg) = st.last_error {
                return Err(CoreError::Provider(msg));
            }
        }
        Ok(st.step)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> ChatRequest {
        ChatRequest {
            messages: vec![
                ChatMessage::user("hello"),
                ChatMessage {
                    role: "assistant".into(),
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "c1".into(),
                        name: "Read".into(),
                        arguments_json: r#"{"path":"a"}"#.into(),
                        thought_signature: None,
                    }],
                    tool_call_id: None,
                    is_error: false,
                },
                ChatMessage::tool_result("c1", "file body", false),
            ],
            tools: vec![serde_json::json!({
                "name": "Read",
                "description": "read a file",
                "input_schema": {"type": "object"}
            })],
            max_completion_tokens: Some(1024),
            temperature: None,
            thinking_budget: None,
            cache_breakpoints: false,
            system: Some("be terse".into()),
            effort: Some(super::super::effort::Effort::High),
            ..Default::default()
        }
    }

    #[test]
    fn body_shape_matches_responses_wire() {
        let b = build_body(&req(), "gpt-5");
        assert_eq!(b["model"], "gpt-5");
        assert_eq!(b["store"], false);
        assert_eq!(b["stream"], true);
        assert_eq!(b["instructions"], "be terse");
        assert_eq!(b["max_output_tokens"], 1024);
        assert_eq!(b["reasoning"]["effort"], "high");
        let input = b["input"].as_array().unwrap();
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["call_id"], "c1");
        assert_eq!(input[1]["name"], "Read");
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[2]["call_id"], "c1");
        assert_eq!(input[2]["output"], "file body");
        let tools = b["tools"].as_array().unwrap();
        assert_eq!(tools[0]["type"], "function", "responses tools are flat");
        assert_eq!(tools[0]["name"], "Read");
        assert_eq!(tools[0]["description"], "read a file");
        assert_eq!(tools[0]["strict"], false);
        assert!(tools[0].get("function").is_none(), "no chat-style nesting");
        assert!(validate_body(&b).is_ok());
    }

    #[test]
    fn error_tool_results_are_marked_in_band() {
        let mut r = req();
        r.messages[2] = ChatMessage::tool_result("c1", "boom", true);
        let b = build_body(&r, "gpt-5");
        assert_eq!(b["input"][2]["output"], "[error] boom");
    }

    #[test]
    fn validator_rejects_broken_bodies() {
        let mut b = build_body(&req(), "gpt-5");
        assert!(validate_body(&b).is_ok());
        b["model"] = serde_json::json!("");
        assert!(validate_body(&b).is_err(), "empty model fails closed");
        let mut b2 = build_body(&req(), "gpt-5");
        b2["input"][1]["name"] = serde_json::json!("");
        assert!(
            validate_body(&b2).is_err(),
            "nameless function_call fails closed"
        );
        let mut b3 = build_body(&req(), "gpt-5");
        b3["input"][0]["content"] = serde_json::json!([]);
        assert!(validate_body(&b3).is_err(), "empty content fails closed");
    }

    #[test]
    fn event_stream_assembles_step() {
        let mut st = ResponsesAcc::new();
        let mut evs = Vec::new();
        let feed = |s: &str, st: &mut ResponsesAcc, evs: &mut Vec<StreamEvent>| {
            let e: REvent = serde_json::from_str(s).unwrap();
            evs.extend(apply_event(&e, st));
        };
        feed(
            r#"{"type":"response.reasoning_summary_text.delta","delta":"think"}"#,
            &mut st,
            &mut evs,
        );
        feed(
            r#"{"type":"response.output_text.delta","delta":"Hi"}"#,
            &mut st,
            &mut evs,
        );
        feed(
            r#"{"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","call_id":"call9","name":"Bash"}}"#,
            &mut st,
            &mut evs,
        );
        feed(
            r#"{"type":"response.function_call_arguments.delta","output_index":2,"delta":"{\"cmd\":\"ls\"}"}"#,
            &mut st,
            &mut evs,
        );
        feed(
            r#"{"type":"response.output_item.done","output_index":2,"item":{"type":"function_call","call_id":"call9","name":"Bash","arguments":"{\"cmd\":\"ls -la\"}"}}"#,
            &mut st,
            &mut evs,
        );
        feed(
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":100,"output_tokens":40,"input_tokens_details":{"cached_tokens":60}}}}"#,
            &mut st,
            &mut evs,
        );
        assert!(matches!(evs[0], StreamEvent::Thinking(ref t) if t == "think"));
        assert!(matches!(evs[1], StreamEvent::Text(ref t) if t == "Hi"));
        assert_eq!(st.step.text(), "Hi");
        let calls = st.step.tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call9");
        assert_eq!(calls[0].name, "Bash");
        // terminal item's COMPLETE arguments win over accumulated deltas
        assert_eq!(calls[0].arguments_json, r#"{"cmd":"ls -la"}"#);
        assert_eq!(st.step.stop, Some(Stop::ToolUse));
        assert_eq!(st.step.usage.input_tokens, 100);
        assert_eq!(st.step.usage.output_tokens, 40);
        assert_eq!(st.step.usage.cache_read_tokens, 60);
    }

    #[test]
    fn incomplete_without_calls_is_max_tokens() {
        let mut st = ResponsesAcc::new();
        let e: REvent = serde_json::from_str(
            r#"{"type":"response.incomplete","response":{"usage":{"input_tokens":1,"output_tokens":2}}}"#,
        )
        .unwrap();
        apply_event(&e, &mut st);
        assert_eq!(st.step.stop, Some(Stop::MaxTokens));
    }

    #[test]
    fn failed_stream_without_output_is_an_error() {
        let mut st = ResponsesAcc::new();
        let e: REvent = serde_json::from_str(
            r#"{"type":"response.failed","response":{"error":{"message":"overloaded"}}}"#,
        )
        .unwrap();
        apply_event(&e, &mut st);
        assert!(st.step.blocks.is_empty());
        assert_eq!(st.last_error.as_deref(), Some("overloaded"));
    }
}
