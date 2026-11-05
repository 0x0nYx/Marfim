//! The brain's Faculty (P4): the learning hook wired into the agent
//! loop's post_step / pre_compaction / idle_propose points. Every piece
//! of learning state is LOCAL, bounded, inspectable (/why publishes the
//! raw state on the bus) and resettable — nothing leaves the machine and
//! nothing influences the loop invisibly (propose-approve for idle).

use marfim_core::faculty::{Faculty, HookCtx, Proposal, StepOutcome};
use serde_json::{json, Value};
use std::sync::Mutex;

use crate::brain::regret;
use crate::intel::adwin::ADWIN;
use crate::intel::conformal::ConformalCalibrator;

/// Bounded learning state, all of it deterministic.
pub struct LearningFaculty {
    /// ADWIN drift detector over the step outcome stream (accept = step
    /// without tool error). Bounded by max_buckets.
    drift: Mutex<ADWIN>,
    /// Conformal calibrator over (risk score, outcome) pairs, bounded.
    conformal: Mutex<ConformalCalibrator>,
    /// Per-model outcome stream for the regret audit, bounded.
    outcomes: Mutex<Vec<(String, f64)>>,
    /// Steps seen (inspection).
    steps: Mutex<u64>,
    /// Latched: a drift was observed (reset only by forget()/reset).
    drifted: Mutex<bool>,
    const_: Mutex<()>,
}

const MAX_OUTCOMES: usize = 1024;

impl Default for LearningFaculty {
    fn default() -> Self {
        Self::new()
    }
}

impl LearningFaculty {
    pub fn new() -> Self {
        Self {
            drift: Mutex::new(ADWIN::default()),
            conformal: Mutex::new(ConformalCalibrator::default_or_empty()),
            outcomes: Mutex::new(Vec::new()),
            steps: Mutex::new(0),
            drifted: Mutex::new(false),
            const_: Mutex::new(()),
        }
    }

    /// Inspectable /why state (the bus event carries exactly this).
    pub fn why(&self) -> Value {
        let _g = self.const_.lock().unwrap_or_else(|p| p.into_inner());
        json!({
            "steps": *self.steps.lock().unwrap_or_else(|p| p.into_inner()),
            "drift": {
                "latched": *self.drifted.lock().unwrap_or_else(|p| p.into_inner()),
                "state": self.drift.lock().unwrap_or_else(|p| p.into_inner()).to_json(),
            },
            "conformal": self.conformal.lock().unwrap_or_else(|p| p.into_inner()).to_json(),
            "outcomes_seen": self.outcomes.lock().unwrap_or_else(|p| p.into_inner()).len(),
        })
    }

    /// Full reset (the /forget surface): learning is REVERSIBLE.
    pub fn forget(&self) {
        let _g = self.const_.lock().unwrap_or_else(|p| p.into_inner());
        *self.drift.lock().unwrap_or_else(|p| p.into_inner()) = ADWIN::default();
        *self.conformal.lock().unwrap_or_else(|p| p.into_inner()) =
            ConformalCalibrator::default_or_empty();
        self.outcomes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        *self.steps.lock().unwrap_or_else(|p| p.into_inner()) = 0;
        *self.drifted.lock().unwrap_or_else(|p| p.into_inner()) = false;
    }
}

#[async_trait::async_trait]
impl Faculty for LearningFaculty {
    fn name(&self) -> &'static str {
        "brain.learning"
    }

    /// post_step: the three learners consume the step outcome.
    ///  - ADWIN drifts on the accept/reject stream (step w/o tool error)
    ///  - conformal observes (risk=1 if the step errored, outcome)
    ///  - regret audit accumulates per-model rewards in [0,1]
    async fn post_step(&self, _ctx: &HookCtx, o: &StepOutcome) {
        let _g = self.const_.lock().unwrap_or_else(|p| p.into_inner());
        *self.steps.lock().unwrap_or_else(|p| p.into_inner()) += 1;
        let accepted = !o.had_tool_error;
        {
            let mut d = self.drift.lock().unwrap_or_else(|p| p.into_inner());
            let report = d.update(accepted);
            if report["drift"].as_bool().unwrap_or(false) {
                *self.drifted.lock().unwrap_or_else(|p| p.into_inner()) = true;
            }
        }
        {
            // risk score: tool errors are the observable failure surface;
            // reward for the regret audit is 1 - risk
            let risk = if o.had_tool_error { 1.0 } else { 0.0 };
            let outcome = if accepted { "accept" } else { "reject" };
            self.conformal
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .observe(risk, outcome);
        }
        {
            let mut outs = self.outcomes.lock().unwrap_or_else(|p| p.into_inner());
            outs.push((o.model.clone(), if accepted { 1.0 } else { 0.0 }));
            if outs.len() > MAX_OUTCOMES {
                let over = outs.len() - MAX_OUTCOMES;
                outs.drain(..over); // bounded: oldest evicted first
            }
        }
    }

    /// pre_compaction: protect the most salient history items. Salience is
    /// deterministic: tool-error feedback > long assistant text > default,
    /// protecting at most PROTECT_MAX items (bounded influence).
    async fn pre_compaction(&self, _ctx: &HookCtx, history: &[String]) -> Vec<usize> {
        const PROTECT_MAX: usize = 2;
        let mut scored: Vec<(usize, u64)> = history
            .iter()
            .enumerate()
            .map(|(i, text)| {
                let err_boost = u64::from(
                    text.contains("Verification failed.") || text.contains("denied by faculty"),
                );
                (i, err_boost * 1_000_000 + text.chars().count() as u64)
            })
            .collect();
        scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        scored.truncate(PROTECT_MAX);
        scored.into_iter().map(|(i, _)| i).collect()
    }

    /// idle_propose: when drift latched, propose a review — never act.
    async fn idle_propose(&self, ctx: &HookCtx) -> Option<Proposal> {
        let drifted = *self.drifted.lock().unwrap_or_else(|p| p.into_inner());
        if !drifted {
            return None;
        }
        let outs = self
            .outcomes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let audit = regret::audit_from_draws(&outs);
        Some(Proposal {
            faculty: self.name().into(),
            kind: "drift-review".into(),
            summary: format!(
                "step-outcome drift detected at turn {} — review the session? (proposal only)",
                ctx.turn
            ),
            detail: audit.to_json(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> HookCtx {
        HookCtx {
            session_id: "t".into(),
            cwd: "/tmp".into(),
            turn: 1,
            step: 1,
        }
    }

    fn outcome(err: bool) -> StepOutcome {
        StepOutcome {
            model: "m".into(),
            input_tokens: 100,
            output_tokens: 10,
            had_tool_calls: false,
            had_tool_error: err,
            text_len: 5,
            latency_ms: 1.0,
        }
    }

    #[tokio::test]
    async fn post_step_feeds_all_three_learners() {
        let f = LearningFaculty::new();
        for _ in 0..40 {
            f.post_step(&ctx(), &outcome(false)).await;
        }
        let w = f.why();
        assert_eq!(w["steps"], 40);
        assert_eq!(w["outcomes_seen"], 40);
        assert_eq!(
            w["conformal"]["all_scores"].as_array().map(Vec::len),
            Some(40),
            "conformal history bounded+fed"
        );
    }

    #[tokio::test]
    async fn drift_latch_proposes_on_idle_and_forget_clears() {
        let f = LearningFaculty::new();
        // stable run: no proposal
        for _ in 0..40 {
            f.post_step(&ctx(), &outcome(false)).await;
        }
        assert!(
            f.idle_propose(&ctx()).await.is_none(),
            "no drift, no proposal"
        );
        // shift the stream to near-total failure → ADWIN must alarm
        for _ in 0..60 {
            f.post_step(&ctx(), &outcome(true)).await;
        }
        let p = f.idle_propose(&ctx()).await;
        assert!(p.is_some(), "drift latched → proposal");
        let p = p.unwrap();
        assert_eq!(p.kind, "drift-review");
        assert!(p.summary.contains("proposal only"));
        // /forget reverses everything
        f.forget();
        let w = f.why();
        assert_eq!(w["steps"], 0);
        assert!(f.idle_propose(&ctx()).await.is_none());
    }

    #[tokio::test]
    async fn pre_compaction_protects_errors_then_longest_bounded() {
        let f = LearningFaculty::new();
        let hist = vec![
            "short".to_string(),
            "Verification failed. Fix the issues.\n---".to_string(),
            "x".repeat(5000),
            "y".repeat(4000),
        ];
        let mut protected = f.pre_compaction(&ctx(), &hist).await;
        protected.sort_unstable();
        assert_eq!(
            protected,
            vec![1, 2],
            "error feedback + longest, bounded at 2"
        );
        assert!(protected.iter().all(|&i| i < hist.len()));
    }
}
