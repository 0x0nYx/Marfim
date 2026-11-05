//! Event bus: typed, JSON-serializable events broadcast to every surface
//! (REPL, headless stream-json, SSE gateway, journal).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    SessionStarted {
        session_id: String,
        cwd: String,
    },
    TurnStarted {
        turn: u64,
    },
    ModelSelected {
        role: String,
        model: String,
        why: String,
    },
    AssistantMessage {
        text: String,
    },
    Thinking {
        text: String,
    },
    /// Incremental streamed output (D9): `kind` is "text" or "thinking".
    StreamDelta {
        kind: String,
        text: String,
    },
    ToolCallStarted {
        id: String,
        tool: String,
        args: serde_json::Value,
    },
    ToolCallResult {
        id: String,
        tool: String,
        output: String,
        is_error: bool,
        duration_ms: u64,
    },
    ApprovalRequested {
        approval_id: String,
        tool: String,
        summary: String,
    },
    ApprovalResolved {
        approval_id: String,
        decision: String,
    },
    Usage {
        role: String,
        model: String,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_creation_tokens: u64,
        cost_usd: Option<f64>,
    },
    Compaction {
        before_tokens: u64,
        after_tokens: u64,
    },
    VerifyStarted {
        commands: Vec<String>,
    },
    VerifyFinished {
        passed: bool,
        output: String,
    },
    TurnEnded {
        turn: u64,
        stop_reason: String,
    },
    Error {
        message: String,
        fatal: bool,
    },
    /// P4 faculty hook observability: every faculty hook call lands here.
    FacultyHook {
        faculty: String,
        hook: String,
        detail: Value,
    },
}

impl Event {
    pub fn kind(&self) -> &'static str {
        match self {
            Event::SessionStarted { .. } => "session_started",
            Event::TurnStarted { .. } => "turn_started",
            Event::ModelSelected { .. } => "model_selected",
            Event::AssistantMessage { .. } => "assistant_message",
            Event::Thinking { .. } => "thinking",
            Event::StreamDelta { .. } => "stream_delta",
            Event::ToolCallStarted { .. } => "tool_call_started",
            Event::ToolCallResult { .. } => "tool_call_result",
            Event::ApprovalRequested { .. } => "approval_requested",
            Event::ApprovalResolved { .. } => "approval_resolved",
            Event::Usage { .. } => "usage",
            Event::Compaction { .. } => "compaction",
            Event::VerifyStarted { .. } => "verify_started",
            Event::VerifyFinished { .. } => "verify_finished",
            Event::FacultyHook { .. } => "faculty_hook",
            Event::TurnEnded { .. } => "turn_end",
            Event::Error { .. } => "error",
        }
    }

    pub fn to_json_line(&self) -> String {
        // Serialization of a plain data enum is infallible by construction;
        // the fallback keeps the stream well-formed (fail-visible) instead
        // of panicking a telemetry path (D6).
        let mut v = serde_json::to_value(self).unwrap_or_else(
            |_| serde_json::json!({"event": "serialization_error", "kind": self.kind()}),
        );
        if let Some(o) = v.as_object_mut() {
            o.insert(
                "event".into(),
                serde_json::Value::String(self.kind().into()),
            );
        }
        serde_json::to_string(&v).unwrap_or_else(|_| {
            format!(
                "{{\"event\":\"serialization_error\",\"kind\":\"{}\"}}",
                self.kind()
            )
        })
    }
}

/// Broadcast bus. Slow subscribers (SSE) must not block the loop —
/// `send` on broadcast is non-blocking; lagging receivers see `Err(Lagged)`.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(1024)
    }
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity.max(16));
        Self { tx }
    }

    pub fn publish(&self, ev: Event) {
        let _ = self.tx.send(ev); // no receivers = fine
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_roundtrip_json() {
        let ev = Event::ToolCallStarted {
            id: "t1".into(),
            tool: "Read".into(),
            args: serde_json::json!({"path": "a.rs"}),
        };
        let line = ev.to_json_line();
        assert!(line.contains("\"tool_call_started\""));
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["tool"], "Read");
    }

    #[tokio::test]
    async fn bus_broadcasts_to_two_subscribers() {
        let bus = EventBus::new(8);
        let mut s1 = bus.subscribe();
        let mut s2 = bus.subscribe();
        bus.publish(Event::TurnStarted { turn: 1 });
        assert!(matches!(
            s1.recv().await,
            Ok(Event::TurnStarted { turn: 1 })
        ));
        assert!(matches!(
            s2.recv().await,
            Ok(Event::TurnStarted { turn: 1 })
        ));
    }
}
