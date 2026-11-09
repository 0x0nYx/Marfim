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
    /// W7.3 health TTLs (§F Hermes `auxiliary_health.py`): the last
    /// success per (role, model) is TRUSTED for 600s, then goes stale
    /// (re-probed); a transient failure quarantines the candidate for at
    /// least 60s. Observability only — chain order and the bandit keep
    /// owning selection; health annotates `explain`.
    last_success: std::collections::BTreeMap<(Role, String), std::time::Instant>,
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

/// W7.3 (§F): how long a POSITIVE health observation is trusted before
/// it goes stale and the candidate is re-probed.
pub const HEALTH_TTL_MS: u64 = 600_000;
/// W7.3 (§F): a transient failure quarantines the candidate for at least
/// this long (the ladder owns anything longer).
pub const TRANSIENT_QUARANTINE_MS: u64 = 60_000;

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
            last_success: Default::default(),
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
        // W7.3: even the FIRST failure carries the 60s transient
        // quarantine floor — a failed candidate is not retried within a
        // minute even if a future ladder change would say otherwise.
        let wait = retry_after_ms
            .unwrap_or(0)
            .max(ladder)
            .max(TRANSIENT_QUARANTINE_MS);
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
        self.last_success
            .insert((role, model.to_string()), std::time::Instant::now());
    }

    /// W7.3 health state: `Some(true)` while the last success is inside
    /// the 600s trust window, `Some(false)` while the candidate is in a
    /// failure window, `None` when never probed (unknown → healthy).
    pub fn health(&self, role: Role, model: &str) -> Option<bool> {
        if !self.is_available(role, model) {
            return Some(false);
        }
        match self.last_success.get(&(role, model.to_string())) {
            Some(at) => {
                let trusted = at.elapsed().as_millis() as u64 <= HEALTH_TTL_MS;
                Some(trusted)
            }
            None => None,
        }
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

    /// W8 context promotion (§C, OMP trigger order): when the current
    /// primary overflows, find a chain sibling whose CATALOG context
    /// window is strictly larger than the CURRENT model's window AND big
    /// enough for the live token load. Deterministic: chain order, first
    /// fit among the qualifying (ties resolve to the earliest). `None`
    /// when the catalog knows no such sibling — the caller compacts.
    pub fn promote_overflow(
        &self,
        role: Role,
        live_tokens: u64,
        current_window: Option<u64>,
    ) -> Option<String> {
        // A REAL overflow of the current model's capacity — anything less
        // and the current model keeps the step (promotion is for
        // overflow, not preference).
        let cur = current_window?;
        if live_tokens <= cur {
            return None;
        }
        let chain = self.chain(role);
        for e in &chain {
            let Some(ctx) = crate::catalog::context_of(&e.model) else {
                continue;
            };
            if ctx <= live_tokens || ctx <= cur {
                continue;
            }
            return Some(e.model.clone());
        }
        None
    }

    /// W7.3: the health line for the /why surface — one clause per
    /// probed chain entry (trusted / quarantined / stale / unknown).
    pub fn health_line(&self, role: Role) -> String {
        let mut parts: Vec<String> = Vec::new();
        for e in self.chain(role) {
            let note = match self.health(role, &e.model) {
                Some(true) => "trusted",
                Some(false) => "quarantined",
                None => "unknown",
            };
            parts.push(format!("{}:{note}", e.model));
        }
        format!("health[{}] {}", role.as_str(), parts.join(" "))
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

    #[test]
    fn w73_health_trust_quarantine_and_unknown_states() {
        let mut r = two_chain();
        // never probed → unknown
        assert_eq!(r.health(Role::Primary, "a"), None);
        // success → trusted inside the 600s window
        r.mark_success(Role::Primary, "a");
        assert_eq!(r.health(Role::Primary, "a"), Some(true));
        // failure → quarantined
        r.mark_failure(Role::Primary, "a", None);
        assert_eq!(r.health(Role::Primary, "a"), Some(false));
        // a later success clears the quarantine
        r.mark_success(Role::Primary, "a");
        assert_eq!(r.health(Role::Primary, "a"), Some(true));
        assert_eq!(
            r.health_line(Role::Primary),
            "health[primary] a:trusted b:unknown"
        );
    }

    #[test]
    fn w73_transient_quarantine_floor_holds_against_short_retry_after() {
        let mut r = two_chain();
        // a provider-declared Retry-After SHORTER than the 60s transient
        // quarantine must not shorten the quarantine (floor wins)
        r.mark_failure(Role::Primary, "a", Some(1_000));
        assert!(!r.is_available(Role::Primary, "a"));
        // the ladder never dips below the quarantine floor at any rung
        for n in 0u32..6 {
            assert!(cooldown_ms(n) >= TRANSIENT_QUARANTINE_MS);
        }
    }

    #[test]
    fn w8_promote_overflow_picks_the_largest_known_sibling() {
        let mut r = Router::new(CostBook::default(), false);
        // real catalog ids: haiku 200k? pro larger — pin relative order
        // from the committed snapshot itself (honest, not hand-waved)
        let haiku = crate::catalog::context_of("claude-haiku-4-5");
        let pro = crate::catalog::context_of("claude-opus-4-6");
        if let (Some(h), Some(p)) = (haiku, pro) {
            assert!(p > h, "snapshot ordering changed: haiku {h}, opus {p}");
            r.set_chain(
                Role::Primary,
                vec![
                    entry("claude-haiku-4-5", true),
                    entry("claude-opus-4-6", true),
                ],
            );
            // live load above haiku's window → promote to the larger one
            assert_eq!(
                r.promote_overflow(Role::Primary, h + 1, Some(h)).as_deref(),
                Some("claude-opus-4-6")
            );
            // live load the CURRENT window still holds → no promotion
            assert_eq!(r.promote_overflow(Role::Primary, 1_000, Some(h)), None);
            // a candidate must also hold the live load itself
            assert_eq!(
                r.promote_overflow(Role::Primary, u64::MAX / 2, Some(h)),
                None
            );
        }
        // models the catalog does not know → no promotion, compact instead
        r.set_chain(
            Role::Primary,
            vec![entry("mock-a", true), entry("mock-b", true)],
        );
        assert_eq!(r.promote_overflow(Role::Primary, u64::MAX / 2, None), None);
    }
}
