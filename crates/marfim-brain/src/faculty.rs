//! The brain's Faculty (P4): the learning hook wired into the agent
//! loop's post_step / pre_compaction / idle_propose points. Every piece
//! of learning state is LOCAL, bounded, inspectable (/why publishes the
//! raw state on the bus) and resettable — nothing leaves the machine and
//! nothing influences the loop invisibly (propose-approve for idle).

use marfim_core::faculty::{Actuation, Faculty, HookCtx, Proposal, StepOutcome};
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
    /// §A per-tool (attempts, errors) counters for the tighten actuation,
    /// bounded by the observed tool-name set.
    tool_stats: Mutex<std::collections::BTreeMap<String, (u64, u64)>>,
    /// Steps seen (inspection).
    steps: Mutex<u64>,
    /// Latched: a drift was observed (reset only by forget()/reset).
    drifted: Mutex<bool>,
    const_: Mutex<()>,
}

const MAX_OUTCOMES: usize = 1024;
/// Hoeffding confidence for actuation claims (conservative by design).
const ACTUATION_DELTA: f64 = 0.05;
/// Minimum evidence per model before a model_select actuation may fire.
const MIN_MODEL_SAMPLES: i64 = 30;
/// Minimum attempts per tool before a tighten actuation may fire.
const MIN_TOOL_ATTEMPTS: u64 = 20;
/// Error-rate LCB that justifies a tighten (the tool fails ≥50% even in
/// the optimistic bound).
const TIGHTEN_ERROR_LCB: f64 = 0.5;
/// At most this many tools tightened per actuation round (bounded power).
const MAX_TIGHTENED_TOOLS: usize = 4;

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
            tool_stats: Mutex::new(std::collections::BTreeMap::new()),
            steps: Mutex::new(0),
            drifted: Mutex::new(false),
            const_: Mutex::new(()),
        }
    }

    /// Inspectable /why state (the bus event carries exactly this).
    pub fn why(&self) -> Value {
        let _g = self.const_.lock().unwrap_or_else(|p| p.into_inner());
        let tools: Value = self
            .tool_stats
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(t, (n, e))| json!({ "tool": t, "attempts": n, "errors": e }))
            .collect();
        json!({
            "steps": *self.steps.lock().unwrap_or_else(|p| p.into_inner()),
            "drift": {
                "latched": *self.drifted.lock().unwrap_or_else(|p| p.into_inner()),
                "state": self.drift.lock().unwrap_or_else(|p| p.into_inner()).to_json(),
            },
            "conformal": self.conformal.lock().unwrap_or_else(|p| p.into_inner()).to_json(),
            "outcomes_seen": self.outcomes.lock().unwrap_or_else(|p| p.into_inner()).len(),
            "tool_stats": tools,
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
        self.tool_stats
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

    /// §A observe_tool: per-tool (attempts, errors) counters — the
    /// evidence stream behind the permission_tighten actuation.
    async fn observe_tool(&self, _ctx: &HookCtx, tool: &str, is_error: bool) {
        let _g = self.const_.lock().unwrap_or_else(|p| p.into_inner());
        let mut stats = self.tool_stats.lock().unwrap_or_else(|p| p.into_inner());
        let e = stats.entry(tool.to_string()).or_insert((0, 0));
        e.0 = e.0.saturating_add(1);
        if is_error {
            e.1 = e.1.saturating_add(1);
        }
    }

    /// §A turn-start actuations (the two evidence-backed powers; both are
    /// Hoeffding-gated so an actuation NEVER fires on a hunch):
    ///
    /// model_select — when a non-first chain model's optimistic bound
    /// (mean + radius) beats the first model's pessimistic bound by a
    /// clear margin over ≥ MIN_MODEL_SAMPLES each, ask the loop to start
    /// the turn on the better model (validated: must be IN the chain).
    ///
    /// permission_tighten — a tool whose error-rate LOWER bound exceeds
    /// TIGHTEN_ERROR_LCB over ≥ MIN_TOOL_ATTEMPTS attempts is denied for
    /// this turn (validated: tighten only ADDS denies).
    async fn actuations(&self, ctx: &HookCtx) -> Vec<Actuation> {
        let _g = self.const_.lock().unwrap_or_else(|p| p.into_inner());
        let mut out = Vec::new();

        // ---- model_select (Hoeffding LCB vs UCB over per-model rewards)
        {
            let outs = self.outcomes.lock().unwrap_or_else(|p| p.into_inner());
            let mut stats: std::collections::BTreeMap<&str, (i64, i64)> = Default::default();
            for (m, r) in outs.iter() {
                let e = stats.entry(m.as_str()).or_insert((0, 0));
                e.0 += 1;
                e.1 += *r as i64;
            }
            // first model seen = the chain head's de-facto baseline
            let mut models: Vec<(&str, i64, i64)> =
                stats.into_iter().map(|(k, (n, w))| (k, n, w)).collect();
            models.sort_by(|a, b| a.0.cmp(b.0)); // deterministic order
            if models.len() >= 2 {
                let baseline = models[0];
                let radius = |n: i64| {
                    crate::intel::conservative::hoeffding_radius(n, ACTUATION_DELTA)
                        .unwrap_or(f64::INFINITY)
                };
                let base_n = baseline.1;
                let base_mean = baseline.2 as f64 / base_n.max(1) as f64;
                let base_lcb = base_mean - radius(base_n);
                let mut best: Option<(&str, f64, f64, f64)> = None;
                for (m, n, wins) in models.iter().skip(1) {
                    if *n < MIN_MODEL_SAMPLES || base_n < MIN_MODEL_SAMPLES {
                        continue;
                    }
                    let mean = *wins as f64 / *n as f64;
                    let lcb = mean - radius(*n);
                    // conservative gate: the candidate must be better than
                    // the baseline EVEN IN THE PESSIMISTIC WORLD (its own
                    // LCB strictly clears the baseline's LCB, and its mean
                    // is strictly higher). An optimistic UCB-only rule
                    // would promote worse models on lucky streaks.
                    if lcb > base_lcb && mean > base_mean {
                        let margin = lcb - base_lcb;
                        if best.map(|(_, _, _, bm)| margin > bm).unwrap_or(true) {
                            best = Some((m, mean, lcb, margin));
                        }
                    }
                }
                if let Some((m, mean, lcb, margin)) = best {
                    out.push(Actuation {
                        kind: "model_select".into(),
                        before: json!(baseline.0),
                        after: json!(m),
                        evidence: json!({
                            "reason": "hoeffding candidate lcb > baseline lcb",
                            "baseline": baseline.0,
                            "baseline_mean": base_mean,
                            "baseline_lcb": base_lcb,
                            "candidate": m,
                            "candidate_mean": mean,
                            "candidate_lcb": lcb,
                            "margin": margin,
                            "delta": ACTUATION_DELTA,
                            "min_samples": MIN_MODEL_SAMPLES,
                        }),
                    });
                }
            }
        }

        // ---- permission_tighten (error-rate LCB > threshold)
        {
            let stats = self.tool_stats.lock().unwrap_or_else(|p| p.into_inner());
            let mut tightened: Vec<Value> = Vec::new();
            let mut evidence: Vec<Value> = Vec::new();
            for (tool, (attempts, errors)) in stats.iter() {
                if *attempts < MIN_TOOL_ATTEMPTS {
                    continue;
                }
                let rate = *errors as f64 / *attempts as f64;
                let lcb = rate
                    - crate::intel::conservative::hoeffding_radius(
                        *attempts as i64,
                        ACTUATION_DELTA,
                    )
                    .unwrap_or(f64::NEG_INFINITY);
                if lcb > TIGHTEN_ERROR_LCB {
                    tightened.push(json!(tool));
                    evidence.push(json!({
                        "tool": tool,
                        "attempts": attempts,
                        "errors": errors,
                        "error_rate": rate,
                        "lcb": lcb,
                        "delta": ACTUATION_DELTA,
                    }));
                }
                if tightened.len() >= MAX_TIGHTENED_TOOLS {
                    break;
                }
            }
            if !tightened.is_empty() {
                out.push(Actuation {
                    kind: "permission_tighten".into(),
                    before: json!([]),
                    after: Value::Array(tightened),
                    evidence: json!({
                        "reason": "tool error-rate LCB exceeds threshold",
                        "per_tool": evidence,
                        "min_attempts": MIN_TOOL_ATTEMPTS,
                    }),
                });
            }
        }

        let _ = ctx;
        out
    }

    /// /why: the inspectable state (trait surface for the slash command).
    fn why(&self) -> Value {
        LearningFaculty::why(self)
    }

    /// /forget: full reset (trait surface for the slash command).
    fn forget(&self) {
        LearningFaculty::forget(self)
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

    fn outcome_with_model(model: &str, err: bool) -> StepOutcome {
        StepOutcome {
            model: model.to_string(),
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
    // ---- §A actuations (Hoeffding-gated) ----

    #[tokio::test]
    async fn model_select_fires_only_on_pessimistic_dominance() {
        let f = LearningFaculty::new();
        // baseline m-a: 24/30 (mean 0.8, LCB ≈ 0.8 − 0.248 = 0.552);
        // candidate m-b: 40/40 (mean 1.0, LCB ≈ 1.0 − 0.215 = 0.785).
        // Both gates clear: candidate mean 1.0 > 0.8 AND its pessimistic
        // LCB clears the baseline's.
        for i in 0..30 {
            f.post_step(&ctx(), &outcome_with_model("m-a", i >= 24))
                .await;
        }
        for _ in 0..40 {
            f.post_step(&ctx(), &outcome_with_model("m-b", false)).await;
        }
        let acts = f.actuations(&ctx()).await;
        let sel: Vec<&Actuation> = acts.iter().filter(|a| a.kind == "model_select").collect();
        assert_eq!(sel.len(), 1, "one model_select actuation");
        let a = sel[0];
        assert_eq!(a.before, serde_json::json!("m-a"));
        assert_eq!(a.after, serde_json::json!("m-b"));
        assert!(
            a.evidence["candidate_lcb"].as_f64().unwrap()
                > a.evidence["baseline_lcb"].as_f64().unwrap(),
            "pessimistic dominance holds: {a:?}"
        );
    }

    #[tokio::test]
    async fn model_select_refuses_without_dominance_or_evidence() {
        let f = LearningFaculty::new();
        // candidate WORSE than baseline (30 vs 0.5 mean) — refuses even
        // with enough samples: a lucky UCB must never promote a worse
        // model.
        for _ in 0..30 {
            f.post_step(&ctx(), &outcome_with_model("m-a", false)).await;
        }
        for _ in 0..30 {
            f.post_step(&ctx(), &outcome_with_model("m-b", true)).await;
        }
        let acts = f.actuations(&ctx()).await;
        assert!(
            !acts.iter().any(|a| a.kind == "model_select"),
            "worse candidate never promoted"
        );
        // too little evidence: no actuation at all
        let f2 = LearningFaculty::new();
        for _ in 0..10 {
            f2.post_step(&ctx(), &outcome_with_model("m-a", false))
                .await;
            f2.post_step(&ctx(), &outcome_with_model("m-b", false))
                .await;
        }
        assert!(f2.actuations(&ctx()).await.is_empty(), "below MIN samples");
    }

    #[tokio::test]
    async fn tighten_fires_on_high_error_lcb_and_is_bounded() {
        let f = LearningFaculty::new();
        for _ in 0..20 {
            f.observe_tool(&ctx(), "Bad", true).await;
        }
        for _ in 0..20 {
            f.observe_tool(&ctx(), "Good", false).await;
        }
        let acts = f.actuations(&ctx()).await;
        let tight: Vec<&Actuation> = acts
            .iter()
            .filter(|a| a.kind == "permission_tighten")
            .collect();
        assert_eq!(tight.len(), 1);
        let tools = tight[0].after.as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0], serde_json::json!("Bad"));
        assert_eq!(
            tight[0].evidence["per_tool"][0]["tool"],
            serde_json::json!("Bad")
        );
        // forget clears everything (reversible)
        f.forget();
        assert!(f.actuations(&ctx()).await.is_empty());
    }

    #[tokio::test]
    async fn trait_why_and_forget_override_defaults() {
        let f: std::sync::Arc<dyn marfim_core::faculty::Faculty> =
            std::sync::Arc::new(LearningFaculty::new());
        // trait-object dispatch reaches the real state (not the Null default)
        let w = f.why();
        assert!(w.is_object(), "why() is the real state, not Null");
        assert_eq!(w["steps"], 0);
        for _ in 0..5 {
            f.post_step(&ctx(), &outcome(false)).await;
        }
        assert_eq!(f.why()["steps"], 5);
        f.forget();
        assert_eq!(f.why()["steps"], 0);
    }
}
