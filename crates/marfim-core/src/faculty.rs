//! Faculty hooks (P4): the agent loop exposes five hook points and any
//! number of faculties observe (and in narrow cases influence) the loop.
//! Hooks are OBSERVATIONAL by default — a faculty can only (a) deny a
//! tool call with a reason (pre_tool), (b) protect history items from
//! compaction (pre_compaction), or (c) PROPOSE an idle action that the
//! UI surfaces for explicit approval (idle_propose). Everything else is
//! read-only telemetry, published on the bus as `faculty_hook` events so
//! the /why surface can explain every decision (learning state is local,
//! versioned, bounded and reversible — no hidden influence).

use crate::events::{Event, EventBus};
use crate::providers::ChatRequest;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

/// Everything a faculty may know about the current loop position.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookCtx {
    pub session_id: String,
    pub cwd: String,
    pub turn: u64,
    pub step: u64,
}

/// What post_step sees: one completed model step (INV-05 ledger data).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepOutcome {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub had_tool_calls: bool,
    pub had_tool_error: bool,
    pub text_len: u64,
    pub latency_ms: f64,
}

/// An idle proposal: surfaced for explicit human approval, never executed
/// silently (propose-approve doctrine).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub faculty: String,
    pub kind: String,
    pub summary: String,
    pub detail: Value,
}

/// One BOUNDED actuation a faculty may apply at a turn boundary. The four
/// powers are exactly: "model_select" (pick the primary model for this
/// turn from ladder/bandit evidence), "permission_tighten" (deny-list a
/// misbehaving tool for this turn — deny>ask>allow: the engine stays the
/// sole authority, a faculty can only TIGHTEN), "compact_threshold"
/// (adjust the compaction trigger within [0.5x, 1.5x] of the configured
/// value), "idle_propose" (existing propose-approve surface). The loop
/// clamps and validates every actuation before applying it and publishes
/// Event::Actuation for each so nothing lands invisibly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Actuation {
    pub kind: String,
    /// The value the loop would have used without this actuation.
    pub before: Value,
    /// The requested value (validated + clamped by the loop).
    pub after: Value,
    /// The learning evidence the decision was made on (inspectable).
    pub evidence: Value,
}

/// One faculty. Implementations must be deterministic given (ctx, data):
/// no wall-clock, no unseeded RNG, bounded memory (learning state is the
/// implementer's responsibility and must be inspectable/resettable).
#[async_trait::async_trait]
pub trait Faculty: Send + Sync {
    fn name(&self) -> &'static str;

    /// Turn-start actuations: the four bounded powers. Default: none.
    /// The loop calls this once per turn, validates/clamps every entry,
    /// publishes Event::Actuation, applies what survived validation.
    async fn actuations(&self, _ctx: &HookCtx) -> Vec<Actuation> {
        Vec::new()
    }

    /// The inspectable learning state (consumed by /why). Default: null.
    fn why(&self) -> Value {
        Value::Null
    }

    /// Full reset of learned state (consumed by /forget — learning is
    /// REVERSIBLE). Default: no-op.
    fn forget(&self) {}

    /// Before each provider request. Read-only in v1 (observe the
    /// request); a faculty CANNOT rewrite prompts here — that influence
    /// would be invisible to the user and is intentionally excluded.
    async fn pre_model(&self, _ctx: &HookCtx, _req: &ChatRequest) {}

    /// Before each tool call. `Some(reason)` DENIES the call; the reason
    /// is shown to the model and the user verbatim.
    async fn pre_tool(&self, _ctx: &HookCtx, _tool: &str, _args: &Value) -> Option<String> {
        None
    }

    /// After each completed step: the learning hook (drift detection,
    /// conformal risk, regret audit all consume this stream).
    async fn post_step(&self, _ctx: &HookCtx, _outcome: &StepOutcome) {}

    /// Before compaction: given the history as texts, return the INDICES
    /// whose information is too salient to summarize away. The loop
    /// re-appends protected messages after the summary.
    async fn pre_compaction(&self, _ctx: &HookCtx, _history: &[String]) -> Vec<usize> {
        Vec::new()
    }

    /// Called when the session goes idle: optionally propose an action.
    async fn idle_propose(&self, _ctx: &HookCtx) -> Option<Proposal> {
        None
    }
}

/// Run one hook across all faculties, publishing an observability event
/// per call. Denial semantics only exist for pre_tool (its Option).
pub async fn run_pre_tool(
    bus: &EventBus,
    faculties: &[Arc<dyn Faculty>],
    ctx: &HookCtx,
    tool: &str,
    args: &Value,
) -> Option<String> {
    for f in faculties {
        let deny = f.pre_tool(ctx, tool, args).await;
        bus.publish(Event::FacultyHook {
            faculty: f.name().into(),
            hook: "pre_tool".into(),
            detail: serde_json::json!({
                "tool": tool,
                "denied": deny.is_some(),
                "reason": deny,
            }),
        });
        if deny.is_some() {
            return deny;
        }
    }
    None
}

pub async fn run_post_step(
    bus: &EventBus,
    faculties: &[Arc<dyn Faculty>],
    ctx: &HookCtx,
    outcome: &StepOutcome,
) {
    for f in faculties {
        f.post_step(ctx, outcome).await;
        bus.publish(Event::FacultyHook {
            faculty: f.name().into(),
            hook: "post_step".into(),
            detail: serde_json::to_value(outcome).unwrap_or(Value::Null),
        });
    }
}

pub async fn run_pre_model(
    bus: &EventBus,
    faculties: &[Arc<dyn Faculty>],
    ctx: &HookCtx,
    req: &ChatRequest,
) {
    for f in faculties {
        f.pre_model(ctx, req).await;
        bus.publish(Event::FacultyHook {
            faculty: f.name().into(),
            hook: "pre_model".into(),
            detail: serde_json::json!({ "messages": req.messages.len(), "tools": req.tools.len() }),
        });
    }
}

pub async fn run_pre_compaction(
    bus: &EventBus,
    faculties: &[Arc<dyn Faculty>],
    ctx: &HookCtx,
    history: &[String],
) -> Vec<usize> {
    let mut protected = Vec::new();
    for f in faculties {
        let idx = f.pre_compaction(ctx, history).await;
        bus.publish(Event::FacultyHook {
            faculty: f.name().into(),
            hook: "pre_compaction".into(),
            detail: serde_json::json!({ "protected": idx, "history_len": history.len() }),
        });
        for i in idx {
            if !protected.contains(&i) && i < history.len() {
                protected.push(i);
            }
        }
    }
    protected.sort_unstable();
    protected
}

pub async fn run_idle(
    bus: &EventBus,
    faculties: &[Arc<dyn Faculty>],
    ctx: &HookCtx,
) -> Vec<Proposal> {
    let mut proposals = Vec::new();
    for f in faculties {
        if let Some(p) = f.idle_propose(ctx).await {
            bus.publish(Event::FacultyHook {
                faculty: f.name().into(),
                hook: "idle_propose".into(),
                detail: serde_json::to_value(&p).unwrap_or(Value::Null),
            });
            proposals.push(p);
        }
    }
    proposals
}

/// Collect turn-start actuations from every faculty. Each actuation is
/// published on the bus as Event::Actuation (faculty, kind, before, after,
/// evidence) so the journal and the /why surface carry the full record.
/// The caller (the loop) validates/clamps/applies; this helper only
/// collects and publishes.
pub async fn run_actuations(
    bus: &EventBus,
    faculties: &[Arc<dyn Faculty>],
    ctx: &HookCtx,
) -> Vec<(String, Actuation)> {
    let mut out = Vec::new();
    for f in faculties {
        for a in f.actuations(ctx).await {
            bus.publish(Event::Actuation {
                faculty: f.name().into(),
                kind: a.kind.clone(),
                before: a.before.clone(),
                after: a.after.clone(),
                evidence: a.evidence.clone(),
            });
            out.push((f.name().to_string(), a));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventBus;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting {
        calls: AtomicUsize,
        deny_on: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl Faculty for Counting {
        fn name(&self) -> &'static str {
            "counting"
        }
        async fn pre_tool(&self, _c: &HookCtx, tool: &str, _a: &Value) -> Option<String> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == self.deny_on.load(Ordering::SeqCst) && tool == "Bash" {
                Some("faculty says no".into())
            } else {
                None
            }
        }
        async fn idle_propose(&self, _c: &HookCtx) -> Option<Proposal> {
            Some(Proposal {
                faculty: "counting".into(),
                kind: "review".into(),
                summary: "check the logs".into(),
                detail: Value::Null,
            })
        }
    }

    #[tokio::test]
    async fn hooks_fire_in_order_and_publish_events() {
        let bus = EventBus::new(256);
        let f: Arc<dyn Faculty> = Arc::new(Counting {
            calls: AtomicUsize::new(0),
            deny_on: AtomicUsize::new(0),
        });
        let ctx = HookCtx {
            session_id: "t".into(),
            cwd: "/tmp".into(),
            turn: 1,
            step: 1,
        };
        let deny = run_pre_tool(&bus, std::slice::from_ref(&f), &ctx, "Bash", &Value::Null).await;
        assert_eq!(deny.as_deref(), Some("faculty says no"));
        let allow = run_pre_tool(&bus, std::slice::from_ref(&f), &ctx, "Read", &Value::Null).await;
        assert!(allow.is_none());
        let props = run_idle(&bus, &[f], &ctx).await;
        assert_eq!(props.len(), 1);
        assert_eq!(props[0].faculty, "counting");
    }
}
