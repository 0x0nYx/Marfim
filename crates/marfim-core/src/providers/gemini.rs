//! Native Gemini wire (`POST {base}/models/{model}:streamGenerateContent?alt=sse`).
//! Differential-tested against fixtures.
//!
//! Wire shape distilled from Oh My Pi's `google-shared.ts`
//! (`buildGoogleGenerateContentParams`, see docs/DISTILLATION.md §R3):
//! - system prompt rides `systemInstruction:{parts:[{text}]}` (never a
//!   `system` role inside `contents`);
//! - tools ride `tools:{functionDeclarations:[{name, description,
//!   parameters}]}` with `toolConfig.functionCallingConfig.mode`;
//! - assistant tool calls arrive as structured `functionCall` parts and
//!   round-trip as `functionResponse` parts on a USER turn (Gemini has no
//!   tool role — this matches Oh My Pi's conversion);
//! - thinking arrives as `thought:true` parts;
//! - usage rides `usageMetadata` with separate thought-token accounting.

use super::*;
use futures_util::StreamExt;

pub struct Gemini {
    pub client: reqwest::Client,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub max_retries: u32,
}

/// Body builder — pure, so the fixture tests assert the exact wire shape.
pub fn build_body(req: &ChatRequest, model_id: &str) -> serde_json::Value {
    // Gemini functionResponses reference the CALLED FUNCTION BY NAME —
    // build the id→name map from the assistant turns in the history.
    let mut fn_names: std::collections::BTreeMap<String, String> = Default::default();
    for m in &req.messages {
        for tc in &m.tool_calls {
            fn_names
                .entry(tc.id.clone())
                .or_insert_with(|| tc.name.clone());
        }
    }

    let mut contents: Vec<serde_json::Value> = Vec::new();
    for m in &req.messages {
        match (m.role.as_str(), &m.tool_call_id) {
            ("tool", Some(id)) => {
                let name = fn_names.get(id).cloned().unwrap_or_else(|| id.clone());
                let response = if m.is_error {
                    serde_json::json!({"error": {"message": m.content}})
                } else {
                    serde_json::json!({"result": m.content})
                };
                contents.push(serde_json::json!({
                    "role": "user",
                    "parts": [{"functionResponse": {"name": name, "response": response}}]
                }));
            }
            ("assistant", _) if !m.tool_calls.is_empty() => {
                let mut parts = Vec::new();
                if !m.content.is_empty() {
                    parts.push(serde_json::json!({"text": m.content}));
                }
                for tc in &m.tool_calls {
                    let args: serde_json::Value =
                        serde_json::from_str(&tc.arguments_json).unwrap_or(serde_json::json!({}));
                    parts
                        .push(serde_json::json!({"functionCall": {"name": tc.name, "args": args}}));
                }
                contents.push(serde_json::json!({"role": "model", "parts": parts}));
            }
            (role, _) => {
                let grole = if role == "assistant" { "model" } else { "user" };
                contents.push(serde_json::json!({"role": grole, "parts": [{"text": m.content}]}));
            }
        }
    }

    let mut body = serde_json::json!({"contents": contents});
    if let Some(sys) = &req.system {
        body["systemInstruction"] = serde_json::json!({"parts": [{"text": sys}]});
    }
    let mut gen_cfg = serde_json::json!({
        "maxOutputTokens": req.max_completion_tokens.unwrap_or(8192)
    });
    if let Some(t) = req.temperature {
        gen_cfg["temperature"] = serde_json::json!(t);
    }
    if let Some(e) = req.effort {
        // Reasoning on: expose the thought stream AND pin the level.
        gen_cfg["thinkingConfig"] = serde_json::json!({
            "includeThoughts": true,
            "thinkingLevel": super::effort::gemini_thinking_level(e)
        });
    } else if let Some(tb) = req.thinking_budget {
        gen_cfg["thinkingConfig"] = serde_json::json!({
            "includeThoughts": true,
            "thinkingBudget": tb
        });
    }
    body["generationConfig"] = gen_cfg;
    if !req.tools.is_empty() {
        let decls: Vec<serde_json::Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut d = serde_json::json!({
                    "name": t["name"].clone(),
                    "parameters": if t["input_schema"].is_object() {
                        t["input_schema"].clone()
                    } else {
                        serde_json::json!({"type": "object"})
                    }
                });
                if let Some(desc) = t.get("description") {
                    d["description"] = desc.clone();
                }
                d
            })
            .collect();
        body["tools"] = serde_json::json!({"functionDeclarations": decls});
        body["toolConfig"] = serde_json::json!({"functionCallingConfig": {"mode": "AUTO"}});
    }
    let _ = model_id; // model rides the URL path, not the body
    body
}

/// Fail-fast outbound check (D10).
pub fn validate_body(body: &serde_json::Value) -> std::result::Result<(), String> {
    let contents = body["contents"]
        .as_array()
        .ok_or("gemini body missing `contents` array")?;
    if contents.is_empty() {
        return Err("gemini body has empty `contents`".into());
    }
    for (i, c) in contents.iter().enumerate() {
        if c["role"].as_str().is_none() {
            return Err(format!("contents[{i}] missing role"));
        }
        if !c["parts"].is_array() {
            return Err(format!("contents[{i}] missing parts array"));
        }
    }
    Ok(())
}

/// One parsed SSE payload (a GenerateContentResponse).
#[derive(serde::Deserialize)]
pub(crate) struct GEvent {
    #[serde(default)]
    pub candidates: Vec<GCandidate>,
    #[serde(default, rename = "usageMetadata")]
    pub usage_metadata: Option<GUsage>,
}

#[derive(serde::Deserialize)]
pub(crate) struct GCandidate {
    #[serde(default)]
    pub content: Option<GContent>,
    #[serde(default, rename = "finishReason")]
    pub finish_reason: Option<String>,
}

#[derive(serde::Deserialize)]
pub(crate) struct GContent {
    #[serde(default)]
    pub parts: Vec<GPart>,
}

#[derive(serde::Deserialize)]
pub(crate) struct GPart {
    #[serde(default)]
    pub text: Option<String>,
    /// True on thinking parts (`thought:true`).
    #[serde(default)]
    pub thought: bool,
    #[serde(default, rename = "functionCall")]
    pub function_call: Option<GFunctionCall>,
}

#[derive(serde::Deserialize)]
pub(crate) struct GFunctionCall {
    pub name: String,
    #[serde(default)]
    pub args: serde_json::Value,
}

#[derive(serde::Deserialize)]
pub(crate) struct GUsage {
    #[serde(default, rename = "promptTokenCount")]
    pub prompt_token_count: Option<u64>,
    #[serde(default, rename = "candidatesTokenCount")]
    pub candidates_token_count: Option<u64>,
    #[serde(default, rename = "thoughtsTokenCount")]
    pub thoughts_token_count: Option<u64>,
    #[serde(default, rename = "cachedContentTokenCount")]
    pub cached_content_token_count: Option<u64>,
}

pub(crate) fn finish_to_stop(fr: &str) -> Stop {
    match fr {
        "MAX_TOKENS" => Stop::MaxTokens,
        "STOP_SEQUENCE" => Stop::StopSequence,
        _ => Stop::EndTurn, // STOP / SAFETY / RECITATION / others → end of turn
    }
}

/// Synthetic tool-call id (Gemini v1beta functionCall parts carry no id):
/// deterministic `fc-<n>` so the harness can round-trip results.
pub(crate) fn synthetic_call_id(n: usize) -> String {
    format!("fc-{n}")
}

/// Pure event application: mutates the accumulator, returns the stream
/// events a consumer should see.
pub(crate) fn apply_event(ev: &GEvent, acc: &mut Step, call_n: &mut usize) -> Vec<StreamEvent> {
    let mut out = Vec::new();
    if let Some(c) = ev.candidates.first() {
        if let Some(content) = &c.content {
            for p in &content.parts {
                if let Some(fc) = &p.function_call {
                    let args = if fc.args.is_object() {
                        serde_json::to_string(&fc.args).unwrap_or_default()
                    } else {
                        String::new()
                    };
                    acc.blocks.push(Block::ToolCall(ToolCall {
                        id: synthetic_call_id(*call_n),
                        name: fc.name.clone(),
                        arguments_json: args,
                    }));
                    *call_n += 1;
                } else if let Some(t) = &p.text {
                    if !t.is_empty() {
                        if p.thought {
                            acc.blocks.push(Block::Thinking(t.clone()));
                            out.push(StreamEvent::Thinking(t.clone()));
                        } else {
                            acc.blocks.push(Block::Text(t.clone()));
                            out.push(StreamEvent::Text(t.clone()));
                        }
                    }
                }
            }
        }
        if let Some(fr) = &c.finish_reason {
            // Tool-call presence wins over the raw finish reason so the
            // harness executes the batch it just received.
            let has_calls = acc.blocks.iter().any(|b| matches!(b, Block::ToolCall(_)));
            acc.stop = Some(if has_calls {
                Stop::ToolUse
            } else {
                finish_to_stop(fr)
            });
        }
    }
    if let Some(u) = &ev.usage_metadata {
        acc.usage.input_tokens = u.prompt_token_count.unwrap_or(acc.usage.input_tokens);
        // Thought tokens are billed output (Hermes/OMP accounting): fold
        // them into output_tokens so cost math is honest.
        let out_tokens = u.candidates_token_count.unwrap_or(0);
        let think_tokens = u.thoughts_token_count.unwrap_or(0);
        if out_tokens + think_tokens > 0 {
            acc.usage.output_tokens = out_tokens.saturating_add(think_tokens);
        }
        acc.usage.cache_read_tokens = u
            .cached_content_token_count
            .unwrap_or(acc.usage.cache_read_tokens);
    }
    out
}

impl Gemini {
    fn url(&self) -> String {
        format!(
            "{}/models/{}:streamGenerateContent?alt=sse",
            self.base_url.trim_end_matches('/'),
            self.model
        )
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
                .header("x-goog-api-key", &self.api_key)
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
impl Provider for Gemini {
    fn name(&self) -> &str {
        "gemini"
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
            .map_err(|e| CoreError::Provider(format!("outbound gemini body rejected: {e}")))?;
        let res = self.send_with_retry(&body).await?;
        let mut acc = Step::default();
        let mut call_n: usize = 0;
        let mut buf = String::new();
        let mut stream = res.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| CoreError::Provider(format!("stream error: {e}")))?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buf.find('\n') {
                let line: String = buf.drain(..=pos).collect();
                let line = line.trim();
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                if let Ok(ev) = serde_json::from_str::<GEvent>(data.trim()) {
                    let events = apply_event(&ev, &mut acc, &mut call_n);
                    for e in events {
                        on_event(e);
                    }
                }
            }
        }
        if acc.stop.is_none() && !acc.blocks.is_empty() {
            let has_calls = acc.blocks.iter().any(|b| matches!(b, Block::ToolCall(_)));
            acc.stop = Some(if has_calls {
                Stop::ToolUse
            } else {
                Stop::EndTurn
            });
        }
        Ok(acc)
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
                        id: "fc-0".into(),
                        name: "Read".into(),
                        arguments_json: r#"{"path":"a"}"#.into(),
                    }],
                    tool_call_id: None,
                    is_error: false,
                },
                ChatMessage::tool_result("fc-0", "file body", false),
            ],
            tools: vec![serde_json::json!({
                "name": "Read",
                "description": "read a file",
                "input_schema": {"type": "object"}
            })],
            max_completion_tokens: Some(2048),
            temperature: Some(0.5),
            thinking_budget: None,
            cache_breakpoints: false,
            system: Some("be terse".into()),
            effort: Some(super::super::effort::Effort::Xhigh),
        }
    }

    #[test]
    fn body_shape_matches_gemini_wire() {
        let b = build_body(&req(), "gemini-2.5-pro");
        assert!(
            b.get("model").is_none(),
            "model rides the URL, not the body"
        );
        assert_eq!(b["systemInstruction"]["parts"][0]["text"], "be terse");
        let contents = b["contents"].as_array().unwrap();
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[1]["parts"][0]["functionCall"]["name"], "Read");
        assert_eq!(
            contents[2]["role"], "user",
            "functionResponse rides a user turn"
        );
        let fr = &contents[2]["parts"][0]["functionResponse"];
        assert_eq!(fr["name"], "Read", "response is keyed by function NAME");
        assert_eq!(fr["response"]["result"], "file body");
        assert_eq!(b["generationConfig"]["maxOutputTokens"], 2048);
        assert_eq!(b["generationConfig"]["temperature"], 0.5);
        assert_eq!(
            b["generationConfig"]["thinkingConfig"]["includeThoughts"],
            true
        );
        assert_eq!(
            b["generationConfig"]["thinkingConfig"]["thinkingLevel"], "HIGH",
            "xhigh collapses to HIGH (OMP mapping)"
        );
        let decls = b["tools"]["functionDeclarations"].as_array().unwrap();
        assert_eq!(decls[0]["name"], "Read");
        assert_eq!(decls[0]["description"], "read a file");
        assert_eq!(b["toolConfig"]["functionCallingConfig"]["mode"], "AUTO");
        assert!(validate_body(&b).is_ok());
    }

    #[test]
    fn error_tool_results_become_error_responses() {
        let mut r = req();
        r.messages[2] = ChatMessage::tool_result("fc-0", "boom", true);
        let b = build_body(&r, "gemini-2.5-pro");
        assert_eq!(
            b["contents"][2]["parts"][0]["functionResponse"]["response"]["error"]["message"],
            "boom"
        );
    }

    #[test]
    fn validator_rejects_broken_bodies() {
        let mut b = build_body(&req(), "gemini-2.5-pro");
        b["contents"] = serde_json::json!([]);
        assert!(validate_body(&b).is_err(), "empty contents fails closed");
        b["contents"] = serde_json::json!([{"parts": []}]);
        assert!(validate_body(&b).is_err(), "roleless content fails closed");
    }

    #[test]
    fn event_stream_assembles_step() {
        let mut acc = Step::default();
        let mut n = 0usize;
        let mut evs = Vec::new();
        let feed = |s: &str, acc: &mut Step, n: &mut usize, evs: &mut Vec<StreamEvent>| {
            let e: GEvent = serde_json::from_str(s).unwrap();
            evs.extend(apply_event(&e, acc, n));
        };
        feed(
            r#"{"candidates":[{"content":{"parts":[{"text":"plan","thought":true}]}}]}"#,
            &mut acc,
            &mut n,
            &mut evs,
        );
        feed(
            r#"{"candidates":[{"content":{"parts":[{"text":"Hi"},{"functionCall":{"name":"Bash","args":{"cmd":"ls"}}}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":100,"candidatesTokenCount":30,"thoughtsTokenCount":12,"cachedContentTokenCount":60}}"#,
            &mut acc,
            &mut n,
            &mut evs,
        );
        assert!(matches!(evs[0], StreamEvent::Thinking(ref t) if t == "plan"));
        assert!(matches!(evs[1], StreamEvent::Text(ref t) if t == "Hi"));
        assert_eq!(acc.text(), "Hi");
        let calls = acc.tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "fc-0", "deterministic synthetic ids");
        assert_eq!(calls[0].name, "Bash");
        assert_eq!(calls[0].arguments_json, r#"{"cmd":"ls"}"#);
        assert_eq!(acc.stop, Some(Stop::ToolUse), "tool-call presence wins");
        assert_eq!(acc.usage.input_tokens, 100);
        assert_eq!(
            acc.usage.output_tokens, 42,
            "thought tokens fold into output"
        );
        assert_eq!(acc.usage.cache_read_tokens, 60);
    }

    #[test]
    fn finish_reason_maps() {
        assert_eq!(finish_to_stop("STOP"), Stop::EndTurn);
        assert_eq!(finish_to_stop("MAX_TOKENS"), Stop::MaxTokens);
        assert_eq!(finish_to_stop("STOP_SEQUENCE"), Stop::StopSequence);
        assert_eq!(finish_to_stop("SAFETY"), Stop::EndTurn);
    }

    #[test]
    fn second_call_gets_second_id() {
        let mut acc = Step::default();
        let mut n = 0usize;
        let e: GEvent = serde_json::from_str(
            r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"A","args":{}}},{"functionCall":{"name":"B","args":{}}}]}}]}"#,
        )
        .unwrap();
        apply_event(&e, &mut acc, &mut n);
        let calls = acc.tool_calls();
        assert_eq!(calls[0].id, "fc-0");
        assert_eq!(calls[1].id, "fc-1");
    }
}
