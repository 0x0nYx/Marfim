//! Router: role → ordered model chain with failover (INV-05). Every role
//! (primary/small/architect/review) is consumed through the router; the
//! real model id lands in the usage ledger; unknown pricing fails closed
//! for budgeted runs.

use crate::providers::{CostBook, Provider};
use crate::{CoreError, Result};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    Primary,
    Small,
    Architect,
    Review,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Primary => "primary",
            Role::Small => "small",
            Role::Architect => "architect",
            Role::Review => "review",
        }
    }
}

#[derive(Clone)]
pub struct ModelEntry {
    pub model: String,
    pub provider: Arc<dyn Provider>,
    /// Cost book carries pricing; `None` here means "pricing unknown".
    pub priced: bool,
}

impl std::fmt::Debug for ModelEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelEntry")
            .field("model", &self.model)
            .field("priced", &self.priced)
            .finish()
    }
}

pub struct Router {
    chains: std::collections::BTreeMap<Role, Vec<ModelEntry>>,
    pub costs: CostBook,
    /// When true, unpriced models fail closed (budget enforcement on).
    pub budgeted: bool,
    /// Failover state (D11): models marked unavailable until an instant.
    unavailable_until: std::collections::BTreeMap<(Role, String), std::time::Instant>,
    /// P3 bandit routing: UCB1 over observed outcomes per (role, model).
    /// Off by default — chain order stays the zero-config behavior.
    pub bandit: bool,
    stats: std::collections::BTreeMap<(Role, String), BanditStats>,
    rounds: std::collections::BTreeMap<Role, u64>,
    /// Consecutive failure count per (role, model) — feeds the cooldown
    /// ladder in `mark_failure` (distillation §F1: Hermes' main-chain
    /// cooldown `min(60s * 2^n, 14400s)`).
    fail_counts: std::collections::BTreeMap<(Role, String), u32>,
}

/// Cooldown ladder for repeated provider failures (distilled from
/// Hermes `fallback_cooldown.py`: base 60s doubling per consecutive
/// failure, capped at 4h; a provider-declared Retry-After is honored
/// when it is longer). Pure and deterministic.
pub const COOLDOWN_BASE_MS: u64 = 60_000;
pub const COOLDOWN_CAP_MS: u64 = 14_400_000;

pub fn cooldown_ms(consecutive_failures: u32) -> u64 {
    COOLDOWN_BASE_MS
        .saturating_mul(1u64 << consecutive_failures.min(20))
        .min(COOLDOWN_CAP_MS)
}

/// Observed outcomes for one (role, model) under bandit routing.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct BanditStats {
    pub pulls: u64,
    pub successes: u64,
    pub total_latency_ms: f64,
}

impl BanditStats {
    /// Reward in [0,1]: success indicator, latency as tiebreak signal
    /// (faster successes earn slightly more).
    fn reward(&self) -> f64 {
        if self.pulls == 0 {
            return 0.0;
        }
        let success_rate = self.successes as f64 / self.pulls as f64;
        let avg_latency = self.total_latency_ms / self.pulls as f64;
        let latency_bonus = 1.0 - (avg_latency / 30_000.0).min(0.25);
        success_rate * (0.75 + 0.25 * latency_bonus)
    }
}

impl Router {
    pub fn new(costs: CostBook, budgeted: bool) -> Self {
        Self {
            chains: Default::default(),
            costs,
            budgeted,
            unavailable_until: Default::default(),
            bandit: false,
            stats: Default::default(),
            rounds: Default::default(),
            fail_counts: Default::default(),
        }
    }

    /// P3: enable UCB1 selection over healthy chain candidates (default
    /// off: strict chain order).
    pub fn set_bandit(&mut self, on: bool) {
        self.bandit = on;
    }

    /// Record one observed outcome (called by the agent loop after each
    /// provider step).
    pub fn report_outcome(&mut self, role: Role, model: &str, success: bool, latency_ms: f64) {
        let s = self.stats.entry((role, model.to_string())).or_default();
        s.pulls += 1;
        if success {
            s.successes += 1;
        }
        s.total_latency_ms += latency_ms.max(0.0);
        *self.rounds.entry(role).or_default() += 1;
    }

    pub fn stats_for(&self, role: Role, model: &str) -> BanditStats {
        self.stats
            .get(&(role, model.to_string()))
            .cloned()
            .unwrap_or_default()
    }

    /// UCB1 pick among the USABLE candidates (probe-passing, available,
    /// priced under budget). Untried models go first, in chain order.
    /// Returns None when bandit selection does not apply.
    fn bandit_pick<F>(
        &self,
        role: Role,
        candidates: &[&ModelEntry],
        probe: &mut F,
    ) -> Option<ModelEntry>
    where
        F: FnMut(&ModelEntry) -> bool,
    {
        if !self.bandit || candidates.len() < 2 {
            return None;
        }
        let usable: Vec<&&ModelEntry> = candidates
            .iter()
            .filter(|e| self.is_available(role, &e.model) && probe(e))
            .collect();
        if usable.len() < 2 {
            return None;
        }
        let t = *self.rounds.get(&role).unwrap_or(&0);
        let mut best: Option<(&ModelEntry, f64)> = None;
        for e in usable {
            let s = self.stats_for(role, &e.model);
            let score = if s.pulls == 0 {
                // untried first: chain order breaks ties deterministically
                f64::INFINITY
            } else {
                s.reward() + std::f64::consts::SQRT_2 * ((t as f64).ln() / s.pulls as f64).sqrt()
            };
            if best.map(|(_, bs)| score > bs).unwrap_or(true) {
                best = Some((e, score));
            }
        }
        best.map(|(e, _)| ModelEntry {
            model: e.model.clone(),
            provider: e.provider.clone(),
            priced: e.priced,
        })
    }

    /// D11: mark a model unavailable after a transport error; the failover
    /// chain skips it until the backoff window passes.
    pub fn mark_unavailable(&mut self, role: Role, model: &str, backoff_ms: u64) {
        self.unavailable_until.insert(
            (role, model.to_string()),
            std::time::Instant::now() + std::time::Duration::from_millis(backoff_ms),
        );
    }

    /// Record one failed attempt through the cooldown ladder (distillation
    /// §F1): consecutive failures double the cooldown 60s → 120s → … capped
    /// at 4h. A provider-declared Retry-After longer than the ladder value
    /// wins. Observable via `cooldown_of`.
    pub fn mark_failure(&mut self, role: Role, model: &str, retry_after_ms: Option<u64>) {
        let key = (role, model.to_string());
        let n = self.fail_counts.entry(key.clone()).or_insert(0);
        *n = n.saturating_add(1);
        let ladder = cooldown_ms(*n);
        let wait = retry_after_ms.unwrap_or(0).max(ladder);
        self.unavailable_until.insert(
            key,
            std::time::Instant::now() + std::time::Duration::from_millis(wait),
        );
    }

    /// Record a successful attempt: the failure ladder resets AND any
    /// stale availability window is cleared — a success is direct proof
    /// the provider is back, so a recovered model must never stay blocked.
    pub fn mark_success(&mut self, role: Role, model: &str) {
        self.fail_counts.remove(&(role, model.to_string()));
        self.unavailable_until.remove(&(role, model.to_string()));
    }

    /// Current consecutive-failure count (observability).
    pub fn failures_of(&self, role: Role, model: &str) -> u32 {
        *self
            .fail_counts
            .get(&(role, model.to_string()))
            .unwrap_or(&0)
    }

    pub fn is_available(&self, role: Role, model: &str) -> bool {
        match self.unavailable_until.get(&(role, model.to_string())) {
            Some(until) => std::time::Instant::now() >= *until,
            None => true,
        }
    }

    pub fn set_chain(&mut self, role: Role, entries: Vec<ModelEntry>) {
        self.chains.insert(role, entries);
    }

    pub fn chain(&self, role: Role) -> Vec<ModelEntry> {
        self.chains.get(&role).cloned().unwrap_or_default()
    }

    pub fn roles(&self) -> Vec<Role> {
        self.chains.keys().copied().collect()
    }

    /// Resolve a role to its first usable model, walking the failover
    /// chain. `probe` is called per candidate to decide usability.
    pub fn resolve<F>(&self, role: Role, mut probe: F) -> Result<(ModelEntry, Vec<String>)>
    where
        F: FnMut(&ModelEntry) -> bool,
    {
        let chain = self.chain(role);
        if chain.is_empty() {
            return Err(CoreError::Provider(format!(
                "no models configured for role `{}`",
                role.as_str()
            )));
        }
        let mut tried = Vec::new();
        // P3 bandit: when enabled and more than one candidate is healthy,
        // UCB1 decides among them (failover still applies on the outcome).
        if let Some(picked) = self.bandit_pick(role, &chain.iter().collect::<Vec<_>>(), &mut probe)
        {
            return Ok((picked, tried));
        }
        for entry in &chain {
            if self.budgeted && !entry.priced {
                tried.push(format!("{}: unpriced (fail closed)", entry.model));
                continue;
            }
            // D11: models marked unavailable by a transport error are
            // skipped until their backoff window passes.
            if !self.is_available(role, &entry.model) {
                tried.push(format!("{}:unavailable", entry.model));
                continue;
            }
            if probe(entry) {
                return Ok((
                    ModelEntry {
                        model: entry.model.clone(),
                        provider: entry.provider.clone(),
                        priced: entry.priced,
                    },
                    tried,
                ));
            }
            tried.push(format!("{}:unavailable", entry.model));
        }
        Err(CoreError::Provider(format!(
            "failover exhausted for role `{}`: {}",
            role.as_str(),
            if tried.is_empty() {
                "all candidates fail closed (unpriced under budget)".into()
            } else {
                tried.join(", ")
            }
        )))
    }

    /// Why-this-model explanation surfaced to the user (ALIVE §display).
    pub fn explain(&self, role: Role, chosen: &str, tried: &[String]) -> String {
        if tried.is_empty() {
            format!("{}: `{}` selected (first in chain)", role.as_str(), chosen)
        } else {
            format!(
                "{}: `{}` selected after failover — tried: {}",
                role.as_str(),
                chosen,
                tried.join(", ")
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{MockProvider, Usage};

    fn entry(model: &str, priced: bool) -> ModelEntry {
        ModelEntry {
            model: model.into(),
            provider: Arc::new(MockProvider::new(vec![])),
            priced,
        }
    }

    #[test]
    fn failover_skips_unavailable() {
        let mut r = Router::new(CostBook::default(), true);
        r.set_chain(
            Role::Primary,
            vec![entry("down-model", true), entry("up-model", true)],
        );
        let (m, tried) = r
            .resolve(Role::Primary, |e| e.model != "down-model")
            .unwrap();
        assert_eq!(m.model, "up-model");
        assert_eq!(tried, vec!["down-model:unavailable".to_string()]);
        assert!(r
            .explain(Role::Primary, &m.model, &tried)
            .contains("failover"));
    }

    #[test]
    fn unpriced_fails_closed_when_budgeted() {
        let mut r = Router::new(CostBook::default(), true);
        r.set_chain(Role::Small, vec![entry("mystery-model", false)]);
        let err = r.resolve(Role::Small, |_| true).unwrap_err();
        assert!(format!("{err}").contains("fail closed"), "{err}");
    }

    #[test]
    fn unpriced_allowed_when_not_budgeted() {
        let mut r = Router::new(CostBook::default(), false);
        r.set_chain(Role::Small, vec![entry("mystery-model", false)]);
        let (m, _) = r.resolve(Role::Small, |_| true).unwrap();
        assert_eq!(m.model, "mystery-model");
    }

    #[test]
    fn empty_chain_is_error() {
        let r = Router::new(CostBook::default(), false);
        assert!(r.resolve(Role::Review, |_| true).is_err());
    }

    #[test]
    fn cost_book_integrates_with_usage() {
        let cb = CostBook::default();
        let u = Usage {
            input_tokens: 10_000,
            output_tokens: 2_000,
            ..Default::default()
        };
        let c = cb.cost_usd("gpt-5-mini", &u).unwrap();
        assert!((c - (10_000.0 * 0.25 + 2_000.0 * 2.0) / 1e6).abs() < 1e-12);
    }

    #[test]
    fn cooldown_ladder_doubles_and_caps() {
        assert_eq!(cooldown_ms(0), 60_000);
        assert_eq!(cooldown_ms(1), 120_000);
        assert_eq!(cooldown_ms(2), 240_000);
        assert_eq!(cooldown_ms(7), 7_680_000);
        assert_eq!(
            cooldown_ms(8),
            COOLDOWN_CAP_MS,
            "60s*2^8 exceeds the 4h cap"
        );
        assert_eq!(cooldown_ms(9), COOLDOWN_CAP_MS);
        assert_eq!(cooldown_ms(50), COOLDOWN_CAP_MS);
    }

    #[test]
    fn failure_ladder_escalates_and_resets_on_success() {
        let mut r = Router::new(CostBook::default(), false);
        r.set_chain(Role::Primary, vec![entry("m", true)]);
        assert_eq!(r.failures_of(Role::Primary, "m"), 0);
        r.mark_failure(Role::Primary, "m", None);
        assert_eq!(r.failures_of(Role::Primary, "m"), 1);
        r.mark_failure(Role::Primary, "m", None);
        assert_eq!(r.failures_of(Role::Primary, "m"), 2);
        assert!(!r.is_available(Role::Primary, "m"), "cooldown arm\u{ed}");
        r.mark_success(Role::Primary, "m");
        assert_eq!(
            r.failures_of(Role::Primary, "m"),
            0,
            "success resets ladder"
        );
        assert!(
            r.is_available(Role::Primary, "m"),
            "success also clears the window"
        );
    }

    #[test]
    fn provider_retry_after_wins_over_ladder_when_longer() {
        let mut r = Router::new(CostBook::default(), false);
        r.set_chain(Role::Primary, vec![entry("m", true)]);
        // Ladder for 1 failure is 120s; provider says wait 5s — ladder wins.
        r.mark_failure(Role::Primary, "m", Some(5_000));
        assert!(!r.is_available(Role::Primary, "m"));
        // Provider says wait 6h — that beats the 4h cap.
        r.mark_success(Role::Primary, "m");
        r.mark_failure(Role::Primary, "m", Some(21_600_000));
        let until = r
            .unavailable_until
            .get(&(Role::Primary, "m".into()))
            .copied()
            .unwrap_or_else(std::time::Instant::now);
        let wait = until.duration_since(std::time::Instant::now()).as_millis() as u64;
        assert!(wait > COOLDOWN_CAP_MS, "declared reset_at honored: {wait}");
    }

    fn bandit_entry(model: &str, priced: bool) -> ModelEntry {
        ModelEntry {
            model: model.into(),
            provider: Arc::new(crate::providers::MockProvider::new(vec![])),
            priced,
        }
    }

    fn two_chain() -> Router {
        let mut r = Router::new(CostBook::default(), false);
        r.set_chain(
            Role::Primary,
            vec![bandit_entry("a", true), bandit_entry("b", true)],
        );
        r
    }

    #[test]
    fn bandit_off_by_default_chain_order_unchanged() {
        let mut r = two_chain();
        r.report_outcome(Role::Primary, "a", false, 5.0); // a is failing
        r.report_outcome(Role::Primary, "b", true, 5.0);
        let (e, _) = r.resolve(Role::Primary, |_| true).unwrap();
        assert_eq!(e.model, "a", "no bandit → strict chain order");
    }

    #[test]
    fn bandit_prefers_healthy_model_after_observed_failures() {
        let mut r = two_chain();
        r.set_bandit(true);
        // a: 3 failures; b: 3 successes
        for _ in 0..3 {
            r.report_outcome(Role::Primary, "a", false, 5.0);
            r.report_outcome(Role::Primary, "b", true, 5.0);
        }
        let (e, _) = r.resolve(Role::Primary, |_| true).unwrap();
        assert_eq!(e.model, "b", "UCB1 routes away from the failing head");
    }

    #[test]
    fn bandit_plays_untried_first_in_chain_order() {
        let mut r = two_chain();
        r.set_bandit(true);
        let (e, _) = r.resolve(Role::Primary, |_| true).unwrap();
        assert_eq!(
            e.model, "a",
            "untried models first, chain order breaks ties"
        );
    }

    #[test]
    fn bandit_single_candidate_falls_back_to_chain_walk() {
        let mut r = Router::new(CostBook::default(), false);
        r.set_chain(Role::Primary, vec![bandit_entry("only", true)]);
        r.set_bandit(true);
        let (e, _) = r.resolve(Role::Primary, |_| true).unwrap();
        assert_eq!(e.model, "only");
    }

    #[test]
    fn bandit_stats_are_observable() {
        let mut r = two_chain();
        r.report_outcome(Role::Primary, "a", true, 100.0);
        r.report_outcome(Role::Primary, "a", false, 50.0);
        let s = r.stats_for(Role::Primary, "a");
        assert_eq!(s.pulls, 2);
        assert_eq!(s.successes, 1);
        assert!((s.total_latency_ms - 150.0).abs() < 1e-9);
    }
}
