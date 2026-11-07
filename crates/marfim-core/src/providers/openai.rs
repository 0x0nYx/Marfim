//! OpenAI-compatible wire (chat/completions): streaming SSE, tool_calls
//! deltas, finish_reason pass-through, `max_completion_tokens` +
//! `thinking` round-trip. Differential-tested against fixtures.

use super::*;

pub struct OpenAiCompat {
    pub client: reqwest::Client,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub max_retries: u32,
}

#[derive(serde::Deserialize)]
pub(crate) struct OaiChunk {
    choices: Vec<OaiChoice>,
    usage: Option<OaiUsage>,
}

#[derive(serde::Deserialize)]
struct OaiChoice {
    #[serde(default)]
    delta: OaiDelta,
    finish_reason: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct OaiDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<OaiToolCallDelta>,
}

#[derive(serde::Deserialize)]
struct OaiToolCallDelta {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<OaiFn>,
}

#[derive(serde::Deserialize)]
struct OaiFn {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(serde::Deserialize)]
pub(crate) struct OaiUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    prompt_tokens_details: Option<PromptDetails>,
    // DeepSeek dialect (distillation §P, OMP openai-shared.ts:526): the
    // cache split arrives as its own pair; miss tokens are the BILLED
    // (non-cached) input.
    #[serde(default)]
    prompt_cache_hit_tokens: Option<u64>,
    #[serde(default)]
    prompt_cache_miss_tokens: Option<u64>,
    // OpenRouter dialect: cache WRITES are billed inside prompt_tokens;
    // they must be split out, not double-counted.
    #[serde(default)]
    cache_write_tokens: Option<u64>,
}

/// Usage de-dup math (distillation §P): every dialect's prompt_tokens
/// INCLUDES its cache components; the normalized Anthropic-shaped Usage
/// counts input_tokens as the NON-cached billed part. Double-counting
/// cache tokens inflates both cost and the compaction trigger.
pub(crate) fn normalize_usage(u: &OaiUsage) -> Usage {
    // DeepSeek pair: hit + miss sum to prompt_tokens, so split exactly.
    if let (Some(hit), Some(miss)) = (u.prompt_cache_hit_tokens, u.prompt_cache_miss_tokens) {
        return Usage {
            input_tokens: miss,
            output_tokens: u.completion_tokens,
            cache_read_tokens: hit,
            cache_creation_tokens: 0,
        };
    }
    let cache_read = u
        .prompt_tokens_details
        .as_ref()
        .map(|d| d.cached_tokens)
        .unwrap_or(0);
    let cache_write = u.cache_write_tokens.unwrap_or(0);
    Usage {
        input_tokens: u
            .prompt_tokens
            .saturating_sub(cache_read)
            .saturating_sub(cache_write),
        output_tokens: u.completion_tokens,
        cache_read_tokens: cache_read,
        cache_creation_tokens: cache_write,
    }
}

#[derive(serde::Deserialize)]
struct PromptDetails {
    #[serde(default)]
    cached_tokens: u64,
}

/// Body builder — shared by the fixture tests so the exact wire shape is
/// asserted, not just parsed.
pub fn build_body(req: &ChatRequest, model: &str) -> serde_json::Value {
    let mut msgs = Vec::new();
    if let Some(sys) = &req.system {
        msgs.push(serde_json::json!({"role": "system", "content": sys}));
    }
    for m in &req.messages {
        match (m.role.as_str(), &m.tool_call_id, m.is_error) {
            ("tool", Some(id), is_err) => {
                // OpenAI wire: tool content is a plain string; errors are
                // marked in-band, not with a content array (D10).
                let content = if is_err {
                    serde_json::Value::String(format!("[error] {}", m.content))
                } else {
                    serde_json::Value::String(m.content.clone())
                };
                msgs.push(
                    serde_json::json!({"role": "tool", "tool_call_id": id, "content": content}),
                );
            }
            ("assistant", _, _) if !m.tool_calls.is_empty() => {
                let mut v = serde_json::json!({"role": "assistant", "content": m.content});
                v["tool_calls"] = serde_json::json!(m
                    .tool_calls
                    .iter()
                    .map(|tc| serde_json::json!({
                        "id": tc.id, "type": "function",
                        "function": {"name": tc.name, "arguments": tc.arguments_json}
                    }))
                    .collect::<Vec<_>>());
                msgs.push(v);
            }
            (role, _, _) => msgs.push(serde_json::json!({"role": role, "content": m.content})),
        }
    }
    let mut body = serde_json::json!({"model": model, "messages": msgs, "stream": true});
    if let Some(mct) = req.max_completion_tokens {
        body["max_completion_tokens"] = serde_json::json!(mct);
    } else {
        body["max_tokens"] = serde_json::json!(8192);
    }
    if let Some(t) = req.temperature {
        body["temperature"] = serde_json::json!(t);
    }
    if let Some(e) = req.effort {
        // Canonical ladder → chat-completions dialect (distillation §R1).
        body["reasoning_effort"] = serde_json::json!(super::effort::openai_reasoning_effort(e));
    } else if let Some(tb) = req.thinking_budget {
        body["reasoning_effort"] = serde_json::json!(reasoning_effort(tb));
    }
    if !req.tools.is_empty() {
        // Normalize tool schemas to the OpenAI function format (D10): the
        // registry emits {name, description, input_schema}; OpenAI wants
        // {type: "function", function: {name, description, parameters}}.
        let tools: Vec<serde_json::Value> = req
            .tools
            .iter()
            .map(|t| {
                if t.get("function").is_some() {
                    t.clone() // already OpenAI-shaped
                } else {
                    let mut f = serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t["name"].clone(),
                            "parameters": if t["input_schema"].is_object() {
                                t["input_schema"].clone()
                            } else {
                                serde_json::json!({"type": "object"})
                            }
                        }
                    });
                    if let Some(d) = t.get("description") {
                        f["function"]["description"] = d.clone();
                    }
                    f
                }
            })
            .collect();
        body["tools"] = serde_json::json!(tools);
    }
    body
}

fn reasoning_effort(budget: u64) -> &'static str {
    match budget {
        0..=2048 => "low",
        4096..=8192 => "medium",
        _ => "high",
    }
}

fn finish_to_stop(fr: &str) -> Stop {
    match fr {
        "tool_calls" => Stop::ToolUse,
        "length" => Stop::MaxTokens,
        "content_filter" => Stop::EndTurn, // normalized per Node drift.js
        _ => Stop::EndTurn,
    }
}

impl OpenAiCompat {
    fn url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    /// Perform the POST with the single retry layer. Returns the raw
    /// response on success (caller streams). Errors carry retryability.
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
                            truncate(&text, 600)
                        )));
                    }
                    if attempt >= self.max_retries {
                        let text = r.text().await.unwrap_or_default();
                        return Err(CoreError::Provider(format!(
                            "HTTP {status} after {attempt} retries: {}",
                            truncate(&text, 600)
                        )));
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

    pub(crate) fn parse_chunk(ch: &crate::providers::openai::OaiChunk, acc: &mut Step) {
        for choice in &ch.choices {
            if let Some(t) = &choice.delta.content {
                if !t.is_empty() {
                    acc.blocks.push(Block::Text(t.clone()));
                }
            }
            if let Some(t) = &choice.delta.reasoning_content {
                if !t.is_empty() {
                    acc.blocks.push(Block::Thinking(t.clone()));
                }
            }
            for tc in &choice.delta.tool_calls {
                let idx = tc.index;
                while acc.tool_calls().len() <= idx {
                    acc.blocks.push(Block::ToolCall(ToolCall {
                        id: String::new(),
                        name: String::new(),
                        arguments_json: String::new(),
                    }));
                }
                let Some(slot) = acc
                    .blocks
                    .iter_mut()
                    .filter_map(|b| match b {
                        Block::ToolCall(t) => Some(t),
                        _ => None,
                    })
                    .nth(idx)
                else {
                    continue;
                };
                if let Some(id) = &tc.id {
                    slot.id = id.clone();
                }
                if let Some(f) = &tc.function {
                    if let Some(n) = &f.name {
                        slot.name.push_str(n);
                    }
                    if let Some(a) = &f.arguments {
                        slot.arguments_json.push_str(a);
                    }
                }
            }
            if let Some(fr) = &choice.finish_reason {
                acc.stop = Some(finish_to_stop(fr));
            }
        }
        if let Some(u) = &ch.usage {
            acc.usage = normalize_usage(u);
        }
    }
}

pub(crate) fn truncate(s: &str, n: usize) -> &str {
    if s.len() <= n {
        s
    } else {
        let mut cut = n;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        &s[..cut]
    }
}

#[async_trait::async_trait]
impl Provider for OpenAiCompat {
    fn name(&self) -> &str {
        "openai-compat"
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
        super::validate_openai_body(&body).map_err(|e| {
            CoreError::Provider(format!("outbound body rejected by vendored schema: {e}"))
        })?;
        let res = self.send_with_retry(&body).await?;
        let mut acc = Step::default();
        let mut stream = res.bytes_stream();
        let mut buf = String::new();
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
                let data = data.trim();
                if data == "[DONE]" {
                    return Ok(acc);
                }
                if let Ok(ch) = serde_json::from_str::<OaiChunk>(data) {
                    let events = captured_events(&ch);
                    Self::parse_chunk(&ch, &mut acc);
                    for ev in events {
                        on_event(ev);
                    }
                }
            }
        }
        Ok(acc)
    }
}

/// Emit text/thinking events (used so stream consumers see the same text
/// the final step contains).
fn captured_events(ch: &OaiChunk) -> Vec<StreamEvent> {
    let mut out = Vec::new();
    for c in &ch.choices {
        if let Some(t) = &c.delta.content {
            if !t.is_empty() {
                out.push(StreamEvent::Text(t.clone()));
            }
        }
        if let Some(t) = &c.delta.reasoning_content {
            if !t.is_empty() {
                out.push(StreamEvent::Thinking(t.clone()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(v: serde_json::Value) -> OaiUsage {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn dedup_plain_openai_cache_read_stays_inside_prompt() {
        // OpenAI dialect: prompt_tokens INCLUDES cached_tokens; the
        // normalized shape must not double-count.
        let u = usage(serde_json::json!({
            "prompt_tokens": 1000,
            "completion_tokens": 50,
            "prompt_tokens_details": {"cached_tokens": 400}
        }));
        let n = normalize_usage(&u);
        assert_eq!(n.input_tokens, 600);
        assert_eq!(n.cache_read_tokens, 400);
        assert_eq!(n.output_tokens, 50);
        assert_eq!(n.cache_creation_tokens, 0);
    }

    #[test]
    fn dedup_deepseek_pair_splits_hit_and_miss() {
        let u = usage(serde_json::json!({
            "prompt_tokens": 1000,
            "completion_tokens": 50,
            "prompt_cache_hit_tokens": 700,
            "prompt_cache_miss_tokens": 300
        }));
        let n = normalize_usage(&u);
        assert_eq!(n.input_tokens, 300, "miss = billed non-cached input");
        assert_eq!(n.cache_read_tokens, 700);
        assert_eq!(n.output_tokens, 50);
    }

    #[test]
    fn dedup_openrouter_cache_write_splits_out() {
        let u = usage(serde_json::json!({
            "prompt_tokens": 1200,
            "completion_tokens": 50,
            "cache_write_tokens": 200,
            "prompt_tokens_details": {"cached_tokens": 500}
        }));
        let n = normalize_usage(&u);
        assert_eq!(n.input_tokens, 500);
        assert_eq!(n.cache_read_tokens, 500);
        assert_eq!(n.cache_creation_tokens, 200);
    }

    #[test]
    fn dedup_no_cache_fields_is_identity() {
        let u = usage(serde_json::json!({"prompt_tokens": 100, "completion_tokens": 7}));
        let n = normalize_usage(&u);
        assert_eq!(n.input_tokens, 100);
        assert_eq!(n.cache_read_tokens, 0);
        assert_eq!(n.cache_creation_tokens, 0);
        assert_eq!(n.output_tokens, 7);
    }
}
