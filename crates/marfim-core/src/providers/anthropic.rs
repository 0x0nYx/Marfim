//! Anthropic wire (v1/messages): SSE content_block events, message_delta
//! usage merge, cache_control breakpoints, tool_use args as raw fragments
//! (parse errors surface to the harness for re-request — never executed).
//! Differential-tested against fixtures.

use super::*;

pub struct Anthropic {
    pub client: reqwest::Client,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub max_retries: u32,
}

pub fn build_body(req: &ChatRequest, model: &str) -> serde_json::Value {
    let mut system_val = serde_json::Value::Null;
    if let Some(sys) = &req.system {
        if req.cache_breakpoints {
            // W7.2: an explicit ttl (config `cache.ttl`) rides the SYSTEM
            // breakpoint only — tools keep the plain ephemeral marker.
            // "1h" is emitted verbatim; "5m" IS the wire default, so it is
            // omitted (the no-ttl body stays byte-identical).
            let mut marker = serde_json::json!({"type": "ephemeral"});
            if req.cache_ttl == Some(super::cache::CacheTtl::OneHour) {
                marker["ttl"] = serde_json::json!("1h");
            }
            system_val =
                serde_json::json!([{"type": "text", "text": sys, "cache_control": marker}]);
        } else {
            system_val = serde_json::json!(sys);
        }
    }
    // D10: tool results become tool_result blocks, and ALL results of one
    // batch are merged into a SINGLE user message (Anthropic wire contract).
    let mut msgs: Vec<serde_json::Value> = Vec::new();
    let mut pending_results: Vec<serde_json::Value> = Vec::new();
    let flush_results = |msgs: &mut Vec<serde_json::Value>,
                         pending: &mut Vec<serde_json::Value>| {
        if !pending.is_empty() {
            msgs.push(serde_json::json!({
                "role": "user",
                "content": std::mem::take(pending)
            }));
        }
    };
    for m in &req.messages {
        match (m.role.as_str(), &m.tool_call_id) {
            ("tool", Some(id)) => {
                let mut block = serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": [{"type": "text", "text": m.content}]
                });
                if m.is_error {
                    block["is_error"] = serde_json::json!(true);
                }
                pending_results.push(block);
            }
            ("assistant", _) if !m.tool_calls.is_empty() => {
                flush_results(&mut msgs, &mut pending_results);
                let mut content = Vec::new();
                if !m.content.is_empty() {
                    content.push(serde_json::json!({"type": "text", "text": m.content}));
                }
                for tc in &m.tool_calls {
                    let args: serde_json::Value =
                        serde_json::from_str(&tc.arguments_json).unwrap_or(serde_json::json!({}));
                    content.push(serde_json::json!({
                        "type": "tool_use", "id": tc.id, "name": tc.name, "input": args
                    }));
                }
                msgs.push(serde_json::json!({"role": "assistant", "content": content}));
            }
            (role, _) => {
                flush_results(&mut msgs, &mut pending_results);
                msgs.push(serde_json::json!({"role": role, "content": m.content}));
            }
        }
    }
    flush_results(&mut msgs, &mut pending_results);
    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": req.max_completion_tokens.unwrap_or(8192),
        "messages": msgs,
        "stream": true
    });
    // §P breakpoint accounting happens BEFORE system_val is moved into the
    // body (see the tool-block cap below).
    let system_breakpoint = req.cache_breakpoints && !system_val.is_null();
    // Reasoning effort (distillation §R1): an explicit thinking budget
    // wins; the canonical ladder converts through the Hermes budget table.
    // W7.1 capability registry (§R): the model row decides HOW thinking
    // engages — adaptive models ride `{"type":"adaptive"}`, mandatory
    // models get a default budget forced even when the user set nothing.
    // Anthropic requires temperature 1 and max_tokens >= budget + 4096
    // whenever thinking is enabled — enforce both here so the request is
    // well-formed by construction instead of failing on the wire.
    let caps = super::capabilities::capability_for(model);
    let thinking_budget = req
        .thinking_budget
        .or_else(|| req.effort.map(super::effort::anthropic_budget_tokens))
        .or_else(|| {
            matches!(caps.thinking, super::capabilities::ThinkingMode::Mandatory)
                .then(|| super::effort::anthropic_budget_tokens(super::effort::Effort::High))
        });
    if let Some(tb) = thinking_budget {
        body["thinking"] = match caps.thinking {
            super::capabilities::ThinkingMode::Adaptive => {
                serde_json::json!({"type": "adaptive", "budget_tokens": tb})
            }
            _ => serde_json::json!({"type": "enabled", "budget_tokens": tb}),
        };
        let floor = super::effort::anthropic_min_max_tokens(tb);
        if body["max_tokens"].as_u64().unwrap_or(0) < floor {
            body["max_tokens"] = serde_json::json!(floor);
        }
        body["temperature"] = serde_json::json!(1.0);
    }
    if !system_val.is_null() {
        body["system"] = system_val;
    }
    if let Some(t) = req.temperature {
        // Thinking pins temperature to 1 on the Anthropic wire; a user
        // temperature is honored only when thinking is off.
        if thinking_budget.is_none() {
            body["temperature"] = serde_json::json!(t);
        }
    }
    if !req.tools.is_empty() {
        // §P 4-breakpoint cap (distilled: OMP anthropic-wire.ts): Anthropic
        // accepts AT MOST 4 cache_control blocks per request. The system
        // block takes one, so at most the LAST 3 tools carry breakpoints —
        // deterministic placement (suffix of the tool list), and counting
        // happens HERE, not on the wire's rejection.
        const MAX_BREAKPOINTS: usize = 4;
        let system_bps = usize::from(system_breakpoint);
        let tool_bp_budget = MAX_BREAKPOINTS.saturating_sub(system_bps);
        let n_tools = req.tools.len();
        let bp_from = n_tools.saturating_sub(tool_bp_budget);
        let tools: Vec<serde_json::Value> = req
            .tools
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let mut c = t.clone();
                if req.cache_breakpoints && i >= bp_from {
                    c["cache_control"] = serde_json::json!({"type": "ephemeral"});
                }
                c
            })
            .collect();
        body["tools"] = serde_json::json!(tools);
    }
    body
}

#[derive(serde::Deserialize)]
pub(crate) struct AEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    index: usize,
    #[serde(default)]
    delta: serde_json::Value,
    #[serde(default)]
    content_block: serde_json::Value,
    #[serde(default)]
    usage: serde_json::Value,
    /// message_start: {message: {usage: {...}}}
    #[serde(default)]
    message: serde_json::Value,
}

pub(crate) fn stop_from_anthropic(s: &str) -> Stop {
    match s {
        "tool_use" => Stop::ToolUse,
        "max_tokens" => Stop::MaxTokens,
        "stop_sequence" => Stop::StopSequence,
        _ => Stop::EndTurn,
    }
}

pub(crate) fn parse_sse_event(
    ev: &AEvent,
    acc: &mut Step,
    tool_blocks: &mut std::collections::BTreeMap<usize, ToolCall>,
) -> Vec<StreamEvent> {
    let mut out = Vec::new();
    match ev.kind.as_str() {
        "content_block_start" => {
            let cb = &ev.content_block;
            if cb["type"] == "tool_use" {
                tool_blocks.insert(
                    ev.index,
                    ToolCall {
                        id: cb["id"].as_str().unwrap_or("").into(),
                        name: cb["name"].as_str().unwrap_or("").into(),
                        arguments_json: String::new(),
                        thought_signature: None,
                    },
                );
            }
        }
        "content_block_delta" => {
            let dt = ev.delta["type"].as_str().unwrap_or("");
            match dt {
                "text_delta" => {
                    let t = ev.delta["text"].as_str().unwrap_or("");
                    if !t.is_empty() {
                        acc.blocks.push(Block::Text(t.into()));
                        out.push(StreamEvent::Text(t.into()));
                    }
                }
                "thinking_delta" => {
                    let t = ev.delta["thinking"].as_str().unwrap_or("");
                    if !t.is_empty() {
                        acc.blocks.push(Block::Thinking(t.into()));
                        out.push(StreamEvent::Thinking(t.into()));
                    }
                }
                "input_json_delta" => {
                    if let Some(tb) = tool_blocks.get_mut(&ev.index) {
                        tb.arguments_json
                            .push_str(ev.delta["partial_json"].as_str().unwrap_or(""));
                    }
                }
                _ => {}
            }
        }
        "content_block_stop" => {
            if let Some(tb) = tool_blocks.remove(&ev.index) {
                acc.blocks.push(Block::ToolCall(tb));
            }
        }
        "message_start" => {
            if let Some(mu) = ev.message.get("usage") {
                acc.usage.input_tokens = mu["input_tokens"]
                    .as_u64()
                    .unwrap_or(acc.usage.input_tokens);
                acc.usage.cache_read_tokens = mu["cache_read_input_tokens"]
                    .as_u64()
                    .unwrap_or(acc.usage.cache_read_tokens);
                acc.usage.cache_creation_tokens = mu["cache_creation_input_tokens"]
                    .as_u64()
                    .unwrap_or(acc.usage.cache_creation_tokens);
            }
        }
        "message_delta" => {
            if let Some(sr) = ev.delta["stop_reason"].as_str() {
                acc.stop = Some(stop_from_anthropic(sr));
            }
            if ev.usage.is_object() {
                acc.usage.input_tokens = ev.usage["input_tokens"]
                    .as_u64()
                    .unwrap_or(acc.usage.input_tokens);
                acc.usage.output_tokens = ev.usage["output_tokens"]
                    .as_u64()
                    .unwrap_or(acc.usage.output_tokens);
                acc.usage.cache_read_tokens = ev.usage["cache_read_input_tokens"]
                    .as_u64()
                    .unwrap_or(acc.usage.cache_read_tokens);
                acc.usage.cache_creation_tokens = ev.usage["cache_creation_input_tokens"]
                    .as_u64()
                    .unwrap_or(acc.usage.cache_creation_tokens);
            }
        }
        _ => {}
    }
    out
}

impl Anthropic {
    fn url(&self) -> String {
        format!("{}/v1/messages", self.base_url.trim_end_matches('/'))
    }

    pub(crate) async fn send_with_retry(
        &self,
        body: &serde_json::Value,
    ) -> Result<reqwest::Response> {
        let mut attempt: u32 = 0;
        loop {
            let res = self
                .client
                .post(self.url())
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
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
impl Provider for Anthropic {
    fn name(&self) -> &str {
        "anthropic"
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
        // D10: fail fast on an invalid outbound body — never send it.
        super::validate_anthropic_body(&body).map_err(|e| {
            CoreError::Provider(format!("outbound body rejected by vendored schema: {e}"))
        })?;
        let res = self.send_with_retry(&body).await?;
        let mut acc = Step::default();
        let mut tool_blocks = std::collections::BTreeMap::new();
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
                if let Ok(ev) = serde_json::from_str::<AEvent>(data.trim()) {
                    let events = parse_sse_event(&ev, &mut acc, &mut tool_blocks);
                    for e in events {
                        on_event(e);
                    }
                }
            }
        }
        Ok(acc)
    }
}
