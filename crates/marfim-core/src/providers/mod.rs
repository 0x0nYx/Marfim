//! Provider layer: wire types (normalized), SSE streaming, single retry
//! layer, OpenAI-compat + Anthropic + Mock providers, cost book.
//!
//! Wire semantics ported from Node v1.2.0 (PARITY §1):
//! - retry on 429/408/409/5xx + network errors, never other 4xx,
//!   honor Retry-After, backoff 500ms * 2^attempt (base.js:32-48)
//! - tool-call args arrive as raw JSON fragments; unparseable args are
//!   re-requested by the harness (never executed) — see loop_
//! - usage normalized to the Anthropic 4-token-shape everywhere

use crate::{CoreError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod anthropic;
pub mod effort;
pub mod gemini;
pub mod openai;
pub mod responses;
pub mod validate;

pub use anthropic::Anthropic;
pub use effort::Effort;
pub use gemini::Gemini;
pub use openai::OpenAiCompat;
pub use responses::OpenAiResponses;
pub use validate::{validate_anthropic_body, validate_openai_body};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON string of arguments as streamed (before parsing).
    pub arguments_json: String,
}

impl ToolCall {
    pub fn parse_args(&self) -> std::result::Result<Value, String> {
        if self.arguments_json.trim().is_empty() {
            return Ok(Value::Object(Default::default()));
        }
        serde_json::from_str(&self.arguments_json)
            .map_err(|e| format!("tool call `{}` args are not valid JSON: {e}", self.name))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Block {
    Text(String),
    Thinking(String),
    ToolCall(ToolCall),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    Aborted,
}

impl Stop {
    pub fn as_str(&self) -> &'static str {
        match self {
            Stop::EndTurn => "end_turn",
            Stop::ToolUse => "tool_use",
            Stop::MaxTokens => "max_tokens",
            Stop::StopSequence => "stop_sequence",
            Stop::Aborted => "aborted",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
}

impl Usage {
    pub fn add(&mut self, o: &Usage) {
        self.input_tokens += o.input_tokens;
        self.output_tokens += o.output_tokens;
        self.cache_read_tokens += o.cache_read_tokens;
        self.cache_creation_tokens += o.cache_creation_tokens;
    }
}

/// One streamed assistant step (already normalized).
#[derive(Debug, Clone, Default)]
pub struct Step {
    pub blocks: Vec<Block>,
    pub stop: Option<Stop>,
    pub usage: Usage,
}

impl Step {
    pub fn text(&self) -> String {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn tool_calls(&self) -> Vec<&ToolCall> {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                Block::ToolCall(tc) => Some(tc),
                _ => None,
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    /// For assistant messages carrying tool calls that must round-trip.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tool_calls: Vec<ToolCall>,
    /// For tool results fed back: the call id this answers.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_call_id: Option<String>,
    /// Tool result errors must round-trip as errors, not successes.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub is_error: bool,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
            tool_calls: vec![],
            tool_call_id: None,
            is_error: false,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
            tool_calls: vec![],
            tool_call_id: None,
            is_error: false,
        }
    }

    pub fn tool_result(
        call_id: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            role: "tool".into(),
            content: content.into(),
            tool_calls: vec![],
            tool_call_id: Some(call_id.into()),
            is_error,
        }
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Anthropic `thinking` budget / OpenAI reasoning round-trip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<u64>,
    /// Anthropic cache breakpoints on the last tool/user block.
    pub cache_breakpoints: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Canonical reasoning-effort ladder (distilled: docs/DISTILLATION.md
    /// §R1). Each wire emits its own dialect from this one value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
}

/// Single retry layer (INV-06): the ONLY place in the codebase that
/// decides retryability. Ported from Node providers/base.js:32-48.
pub fn retry_policy(status: u16) -> bool {
    status == 429 || status == 408 || status == 409 || status >= 500
}

pub fn backoff_ms(attempt: u32) -> u64 {
    500u64.saturating_mul(1u64 << attempt.min(16))
}

pub fn retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let v = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    let secs: u64 = v.trim().parse().ok()?;
    Some(secs.saturating_mul(1000))
}

/// Wire trait: streaming one assistant step.
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    async fn chat_step(&self, req: &ChatRequest) -> Result<Step>;
    async fn chat_step_stream(
        &self,
        req: &ChatRequest,
        on_event: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<Step>;
}

pub enum StreamEvent {
    Text(String),
    Thinking(String),
}

/// ---------------------------------------------------------------- Mock
/// Scripted deterministic provider for tests + fixtures.
pub struct MockProvider {
    pub script: std::sync::Mutex<Vec<MockAction>>,
    pub calls: std::sync::Mutex<Vec<ChatRequest>>,
}

/// One scripted turn: either a step, a transport error (for failover
/// and retry tests), or a pause before the next entry (for signal /
/// timeout / cancel tests). Sleep is a test affordance only — never a
/// silent-mock fallback (D11).
#[derive(Debug, Clone)]
pub enum MockAction {
    Step(Step),
    Error(String),
    Sleep(u64),
}

impl MockProvider {
    pub fn new(steps: Vec<Step>) -> Self {
        Self {
            script: std::sync::Mutex::new(steps.into_iter().map(MockAction::Step).collect()),
            calls: std::sync::Mutex::new(vec![]),
        }
    }

    pub fn from_actions(actions: Vec<MockAction>) -> Self {
        Self {
            script: std::sync::Mutex::new(actions),
            calls: std::sync::Mutex::new(vec![]),
        }
    }

    pub fn error(msg: &str) -> MockAction {
        MockAction::Error(msg.to_string())
    }

    pub fn text(t: &str) -> Step {
        Step {
            blocks: vec![Block::Text(t.into())],
            stop: Some(Stop::EndTurn),
            usage: Usage::default(),
        }
    }

    pub fn tool_call(id: &str, name: &str, args: Value) -> Step {
        Step {
            blocks: vec![Block::ToolCall(ToolCall {
                id: id.into(),
                name: name.into(),
                arguments_json: args.to_string(),
            })],
            stop: Some(Stop::ToolUse),
            usage: Usage::default(),
        }
    }

    pub fn request_count(&self) -> usize {
        self.calls.lock().unwrap_or_else(|p| p.into_inner()).len()
    }
}

#[async_trait::async_trait]
impl Provider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }

    async fn chat_step(&self, req: &ChatRequest) -> Result<Step> {
        self.calls
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(req.clone());
        loop {
            // guard is scoped INSIDE the iteration: never held across await
            let action = {
                let mut script = self.script.lock().unwrap_or_else(|p| p.into_inner());
                if script.is_empty() {
                    return Ok(MockProvider::text(
                        "mock provider: no scripted turns configured",
                    ));
                }
                script.remove(0)
            };
            match action {
                MockAction::Step(s) => return Ok(s),
                MockAction::Error(e) => return Err(CoreError::Provider(e)),
                MockAction::Sleep(ms) => {
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                }
            }
        }
    }

    async fn chat_step_stream(
        &self,
        req: &ChatRequest,
        on_event: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<Step> {
        let step = self.chat_step(req).await?;
        for b in &step.blocks {
            match b {
                Block::Text(t) => on_event(StreamEvent::Text(t.clone())),
                Block::Thinking(t) => on_event(StreamEvent::Thinking(t.clone())),
                Block::ToolCall(_) => {}
            }
        }
        Ok(step)
    }
}

/// ------------------------------------------------------- Cost book
/// Real per-model pricing; unknown pricing fails closed for budgeted runs.
pub struct CostBook {
    /// usd per 1M tokens: (input, output)
    pub pricing: std::collections::BTreeMap<String, (f64, f64)>,
}

impl CostBook {
    /// P3: merge per-Mtok pricing for the given models.dev provider ids
    /// into the book (only entries that exist and carry both prices).
    /// Committing the snapshot keeps this fully offline (D11).
    pub fn extend_from_catalog(&mut self, provider_ids: &[&str]) {
        for p in provider_ids {
            for (id, m) in crate::catalog::models_of(p) {
                if let (Some(i), Some(o)) = (m.input_usd_per_mtok, m.output_usd_per_mtok) {
                    self.pricing.entry(id.clone()).or_insert((i, o));
                }
            }
        }
    }
}

impl Default for CostBook {
    fn default() -> Self {
        let mut pricing = std::collections::BTreeMap::new();
        // Public list prices (per 1M tokens, in/out) — extendable via
        // config `providers.<name>.pricing`; unknown stays fail-closed.
        pricing.insert("claude-haiku-4-5".into(), (1.0, 5.0));
        pricing.insert("claude-sonnet-4-5".into(), (3.0, 15.0));
        pricing.insert("gpt-5-mini".into(), (0.25, 2.0));
        pricing.insert("gpt-5".into(), (1.25, 10.0));
        Self { pricing }
    }
}

impl CostBook {
    pub fn cost_usd(&self, model: &str, usage: &Usage) -> Option<f64> {
        let (pin, pout) = self.pricing.get(model)?;
        Some(usage.input_tokens as f64 * pin / 1e6 + usage.output_tokens as f64 * pout / 1e6)
    }

    pub fn known(&self, model: &str) -> bool {
        self.pricing.contains_key(model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_policy_never_retries_client_errors_except_listed() {
        assert!(retry_policy(429));
        assert!(retry_policy(408));
        assert!(retry_policy(409));
        assert!(retry_policy(500));
        assert!(retry_policy(503));
        assert!(!retry_policy(400));
        assert!(!retry_policy(401));
        assert!(!retry_policy(403));
        assert!(!retry_policy(404));
        assert!(!retry_policy(422));
    }

    #[test]
    fn backoff_is_exponential_from_500ms() {
        assert_eq!(backoff_ms(0), 500);
        assert_eq!(backoff_ms(1), 1000);
        assert_eq!(backoff_ms(2), 2000);
    }

    #[test]
    fn mock_provider_replays_script() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mock = MockProvider::new(vec![
                MockProvider::tool_call("t1", "Read", serde_json::json!({"path": "x"})),
                MockProvider::text("done"),
            ]);
            let req = ChatRequest {
                messages: vec![ChatMessage::user("hi")],
                ..Default::default()
            };
            let s1 = mock.chat_step(&req).await.unwrap();
            assert_eq!(s1.stop, Some(Stop::ToolUse));
            let s2 = mock.chat_step(&req).await.unwrap();
            assert_eq!(s2.text(), "done");
            assert_eq!(mock.request_count(), 2);
        });
    }

    #[test]
    fn tool_call_arg_parse_edge_cases() {
        let ok = ToolCall {
            id: "a".into(),
            name: "T".into(),
            arguments_json: r#"{"x":1}"#.into(),
        };
        assert!(ok.parse_args().is_ok());
        let empty = ToolCall {
            id: "b".into(),
            name: "T".into(),
            arguments_json: "  ".into(),
        };
        assert_eq!(empty.parse_args().unwrap(), serde_json::json!({}));
        let bad = ToolCall {
            id: "c".into(),
            name: "T".into(),
            arguments_json: r#"{"x": "unclosed"#.into(),
        };
        assert!(bad.parse_args().is_err());
    }

    #[test]
    fn cost_book_fails_closed_on_unknown_model() {
        let cb = CostBook::default();
        assert_eq!(cb.cost_usd("unknown-model", &Usage::default()), None);
        assert!(!cb.known("unknown-model"));
        let u = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            ..Default::default()
        };
        let c = cb.cost_usd("claude-haiku-4-5", &u).unwrap();
        assert!((c - 6.0).abs() < 1e-9);
    }
}
