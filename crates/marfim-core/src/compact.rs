//! Context management (INV-07 + D4).
//!
//! Two stages, cheapest first:
//! 1. **micro-compaction** — elide the bodies of stale tool results,
//!    keeping a protected tail; pure, deterministic, no provider call.
//! 2. **summarization** — the Small role summarizes; failure is an
//!    explicit stop — never a silent continuation with truncated context.
//!
//! The old `summarize_via` free function was removed in the baseline
//! wave: the loop's `compact_via_small` is the only summarizer (it
//! journals usage and routes through the Small role properly).

use crate::providers::ChatMessage;

/// A tool result whose body was elided keeps its id and error flag; only
/// the payload shrinks. The marker names the original size so the model
/// (and the user) can see what was dropped.
pub const ELIDE_MARKER_PREFIX: &str = "[elided ";

/// Elide stale tool results, oldest first, until the estimated token
/// count drops to `target_tokens` (len/4 heuristic, same as the loop).
///
/// - The last `protected_tail` messages are NEVER touched (the in-flight
///   exchange the model is actively reasoning about).
/// - The FIRST tool result of each turn is also left intact when it is
///   the direct answer to the immediately preceding call — only results
///   older than the protected tail are eligible.
/// - Messages that are not tool results (user/assistant text, tool-call
///   carries) are never modified.
///
/// Returns the number of bytes removed. Pure and deterministic: same
/// input → same output, no wall-clock, no RNG.
pub fn micro_compact(
    history: &mut [ChatMessage],
    target_tokens: u64,
    protected_tail: usize,
) -> u64 {
    micro_compact_budget(history, target_tokens, protected_tail, None)
}

// ---------------------------------------------------------------------
// Distillation §C2: token-budget protected tail (Hermes
// `context_compressor_tail.py`). A message-count tail is blind to size:
// six protected messages can be six 40k-blob tool reads. The budget form
// grows the protected tail until it holds at least `tail_budget_tokens`
// (bounded by the message-count tail as a hard ceiling):
//   budget = clamp(2.5% of the context window, 10k floor, 25k cap)
//   (Hermes constants: TAIL_BUDGET_RATIO / TAIL_FLOOR / TAIL_CAP).

/// Window share for the protected-tail token budget.
pub const TAIL_BUDGET_RATIO: f64 = 0.025;
/// Absolute floor of the protected-tail budget in tokens.
pub const TAIL_FLOOR_TOKENS: u64 = 10_000;
/// Absolute cap of the protected-tail budget in tokens.
pub const TAIL_CAP_TOKENS: u64 = 25_000;

/// Pure helper: the protected-tail budget for a context window.
/// `None` (unknown window) keeps the floor — bounded either way.
pub fn tail_budget_tokens(context_window: Option<u64>) -> u64 {
    let from_window = context_window
        .map(|w| (w as f64 * TAIL_BUDGET_RATIO) as u64)
        .unwrap_or(TAIL_FLOOR_TOKENS);
    from_window.clamp(TAIL_FLOOR_TOKENS, TAIL_CAP_TOKENS).max(1)
}

/// micro-compaction with a token-budget protected tail: the protected
/// tail is `protected_tail` messages EXPANDED (never shrunk) so the
/// newest messages cover at least `tail_budget_tokens`. Estimation is
/// the same len/4 heuristic; the function stays pure and deterministic.
pub fn micro_compact_budget(
    history: &mut [ChatMessage],
    target_tokens: u64,
    protected_tail: usize,
    tail_budget_tokens: Option<u64>,
) -> u64 {
    let estimate = |h: &[ChatMessage]| -> u64 {
        h.iter()
            .map(|m| m.content.len() as u64)
            .sum::<u64>()
            .div_ceil(4)
    };
    if estimate(history) <= target_tokens {
        return 0;
    }
    // Expand the message-count tail until it covers the token budget.
    let mut protected_start = history.len().saturating_sub(protected_tail);
    if let Some(budget) = tail_budget_tokens {
        let mut covered = estimate(&history[protected_start.min(history.len())..]);
        let mut start = protected_start;
        while start > 0 && covered < budget {
            start -= 1;
            covered += history[start].content.len().div_ceil(4) as u64;
        }
        protected_start = start;
    }
    let mut removed_bytes = 0u64;
    // Oldest-first, but never inside the protected tail.
    for i in 0..protected_start.min(history.len()) {
        if estimate(history) <= target_tokens {
            break;
        }
        let m = &mut history[i];
        if m.tool_call_id.is_some() && !m.is_error && m.content.len() > 256 {
            removed_bytes += m.content.len() as u64;
            m.content = format!("{}{}B]", ELIDE_MARKER_PREFIX, removed_bytes);
        }
    }
    removed_bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_result(id: &str, body: &str) -> ChatMessage {
        ChatMessage {
            role: "tool".into(),
            content: body.into(),
            tool_calls: vec![],
            tool_call_id: Some(id.into()),
            is_error: false,
        }
    }

    #[test]
    fn under_target_is_a_noop() {
        let mut h = vec![ChatMessage::user("hello"), tool_result("t1", "x")];
        assert_eq!(micro_compact(&mut h, 1_000, 2), 0);
        assert_eq!(h[1].content, "x");
    }

    #[test]
    fn elides_stale_results_oldest_first_keeps_tail() {
        let big = "y".repeat(4_000);
        let mut h = vec![
            tool_result("t1", &big),
            tool_result("t2", &big),
            ChatMessage::user("now act"),
            tool_result("t3", &big), // protected tail
        ];
        let removed = micro_compact(&mut h, 300, 2);
        assert!(removed >= 4_000, "at least the first body is gone");
        assert!(h[0].content.starts_with(ELIDE_MARKER_PREFIX));
        // errors are never elided; tail is never elided
        let mut e = vec![tool_result("t1", &big), tool_result("t2", &big)];
        e[0].is_error = true;
        micro_compact(&mut e, 16, 0);
        assert!(e[0].content.starts_with("yyyy"), "error bodies survive");
    }

    #[test]
    fn deterministic_and_idempotent_at_target() {
        let big = "z".repeat(10_000);
        let mk = || {
            vec![
                tool_result("a", &big),
                tool_result("b", &big),
                ChatMessage::user("q"),
            ]
        };
        let mut a = mk();
        let mut b = mk();
        let ra = micro_compact(&mut a, 500, 1);
        let rb = micro_compact(&mut b, 500, 1);
        assert_eq!(ra, rb);
        let dump = |h: &[ChatMessage]| -> Vec<(String, String)> {
            h.iter()
                .map(|m| (m.role.clone(), m.content.clone()))
                .collect()
        };
        assert_eq!(dump(&a), dump(&b), "same input, same output");
    }

    // ---- §C2 token-budget tail ----

    #[test]
    fn tail_budget_clamped_between_floor_and_cap() {
        // Unknown window → floor.
        assert_eq!(tail_budget_tokens(None), TAIL_FLOOR_TOKENS);
        // 200k window → 2.5% = 5k → floored at 10k.
        assert_eq!(tail_budget_tokens(Some(200_000)), 10_000);
        // 2M window → 2.5% = 50k → capped at 25k.
        assert_eq!(tail_budget_tokens(Some(2_000_000)), TAIL_CAP_TOKENS);
        // 1M window → 2.5% = 25k = cap exactly.
        assert_eq!(tail_budget_tokens(Some(1_000_000)), 25_000);
    }

    #[test]
    fn budget_tail_protects_more_than_message_count_when_newest_are_huge() {
        let big = "x".repeat(40_000); // 10k tokens each
        let small = "s".repeat(200);
        let mut h = vec![
            tool_result("t0", &big),   // old, budget elides this one
            tool_result("t1", &small), // old, message-tail would elide; budget protects
            ChatMessage::user("q"),
            tool_result("t2", &big), // message-tail protected (last 3)
        ];
        let removed = micro_compact_budget(&mut h, 9_000, 3, Some(12_000));
        assert_eq!(removed, 0, "budget grew the tail past the message count");
        assert!(h[0].content.starts_with("xxxx"), "old result intact");
        assert!(h[1].content.starts_with("ssss"), "small result intact");
    }

    #[test]
    fn budget_tail_still_elides_when_target_forces_it() {
        let big = "x".repeat(40_000); // ~10k tokens each
        let small = "s".repeat(200); // not elidable (<256 bytes)
        let mut h = vec![
            tool_result("t0", &small),
            tool_result("t1", &big),
            tool_result("t2", &big),
            ChatMessage::user("q"),
            tool_result("t3", &big),
        ];
        // Tail budget 12k already covered by t2+user+t3 (~20k): no
        // expansion; the OLDEST elidable result (t1) goes first.
        let removed = micro_compact_budget(&mut h, 9_000, 3, Some(12_000));
        assert!(removed >= 40_000);
        assert!(h[1].content.starts_with(ELIDE_MARKER_PREFIX));
        assert!(h[0].content.starts_with("ssss"), "not elidable, intact");
        assert!(h[4].content.starts_with("xxxx"), "tail intact");
    }

    #[test]
    fn budget_covering_all_history_elides_nothing_stage2_takes_over() {
        let big = "x".repeat(40_000);
        let mut h = vec![
            tool_result("t0", &big),
            tool_result("t1", &big),
            ChatMessage::user("q"),
            tool_result("t2", &big),
        ];
        // Tail budget larger than the whole history: the protected tail
        // expands to cover everything — micro-compaction honestly reports
        // 0 (nothing is outside the tail) and the SUMMARIZER (stage 2)
        // owns the reduction. Eliding inside the live exchange would
        // destroy context the model is actively reasoning about.
        let removed = micro_compact_budget(&mut h, 6_000, 3, Some(50_000));
        assert_eq!(removed, 0);
        assert!(h[0].content.starts_with("xxxx"));
    }

    #[test]
    fn budget_none_behaves_exactly_like_message_count() {
        let big = "y".repeat(4_000);
        let mk = || {
            vec![
                tool_result("t1", &big),
                tool_result("t2", &big),
                ChatMessage::user("now act"),
                tool_result("t3", &big),
            ]
        };
        let mut a = mk();
        let mut b = mk();
        let ra = micro_compact(&mut a, 300, 2);
        let rb = micro_compact_budget(&mut b, 300, 2, None);
        assert_eq!(ra, rb);
    }
}

// ---------------------------------------------------------------------
// Distillation §C1: compaction trigger math (Hermes `context_compressor
// .py::_compute_threshold_tokens`). The trigger is derived from the REAL
// context budget, not an arbitrary fixed count:
//   effective = context_window − max_output_tokens
//   trigger   = effective × pct, floored at 64k tokens — but the floor
//               itself is capped at 85% of the effective window when it
//               binds, and small (<512k) windows get a raise-only 75%
//               floor. The result never exceeds the effective window.
// Pure and deterministic; unit-tested below.

/// Default trigger percentage (Hermes `compression.threshold=0.50`).
pub const DEFAULT_TRIGGER_PCT: f64 = 0.50;
/// Absolute floor for the trigger in tokens (Hermes
/// `MINIMUM_CONTEXT_LENGTH = 64_000`).
pub const MIN_TRIGGER_TOKENS: u64 = 64_000;
/// Windows below this size get the raise-only 75% floor (Hermes
/// `_SMALL_CTX_WINDOW_LIMIT = 512_000`).
/// W8 (§C, Hermes micro_compaction): a defrag sweep must reclaim at
/// least this many tokens to count as a success.
pub const DEFRAG_MIN_TOKENS: u64 = 2_000;
/// W8 (§C): after this many consecutive insufficient sweeps the micro
/// stage is skipped until the next full summarization resets the ledger.
pub const DEFRAG_MAX_FAILS: u32 = 3;

pub const SMALL_WINDOW_LIMIT: u64 = 512_000;
/// Cap ratio that binds when the 64k floor would overreach (Hermes
/// `_MIN_CTX_TRIGGER_RATIO = 0.85`).
pub const FLOOR_CAP_RATIO: f64 = 0.85;
/// Raise-only floor ratio on small windows (Hermes rule).
pub const SMALL_WINDOW_FLOOR_RATIO: f64 = 0.75;

pub fn trigger_tokens(context_window: u64, max_output_tokens: u64, pct: f64) -> u64 {
    let effective = context_window.saturating_sub(max_output_tokens).max(1);
    let pct = pct.clamp(0.05, 1.0);
    let base = (effective as f64 * pct) as u64;
    let floor = ((effective as f64 * FLOOR_CAP_RATIO) as u64).min(MIN_TRIGGER_TOKENS);
    let mut t = base.max(floor.min(effective));
    if effective < SMALL_WINDOW_LIMIT {
        let small_floor = (effective as f64 * SMALL_WINDOW_FLOOR_RATIO) as u64;
        t = t.max(small_floor.min(effective));
    }
    t.max(1).min(effective)
}

#[cfg(test)]
mod trigger_tests {
    use super::*;

    // All expectations hand-derived from the formula:
    //   eff = (ctx − out).max(1); base = eff×pct;
    //   floor = min(0.85×eff, 64k) capped at eff; t = max(base, floor);
    //   if eff < 512k: t = max(t, 0.75×eff); return t.max(1).min(eff).

    #[test]
    fn moderate_window_small_window_floor_applies() {
        // 200k window, 8k out → eff 192k: 50% = 96k, 64k floor below it,
        // then the <512k raise-only 75% floor lifts the trigger to 144k
        // (compaction fires with plenty of tail budget on a 200k model).
        assert_eq!(trigger_tokens(200_000, 8_000, DEFAULT_TRIGGER_PCT), 144_000);
    }

    #[test]
    fn floor_is_capped_at_85pct_of_effective() {
        // 32k window, 8k out → eff 24k: 50% = 12k; 64k floor would
        // overreach → capped at 85% = 20,400; small-window 75% floor
        // (18k) does not raise it further.
        assert_eq!(trigger_tokens(32_000, 8_000, DEFAULT_TRIGGER_PCT), 20_400);
    }

    #[test]
    fn small_window_75pct_floor_can_raise_over_the_capped_floor() {
        // 100k window, 8k out → eff 92k: 50% = 46k; capped floor = 64k;
        // small-window 75% = 69k raises the trigger.
        assert_eq!(trigger_tokens(100_000, 8_000, DEFAULT_TRIGGER_PCT), 69_000);
    }

    #[test]
    fn huge_window_uses_percentage_then_floors_at_64k() {
        // 1M window, 8k out → eff 992k (≥512k, no small-window rule):
        // 50% = 496k wins.
        assert_eq!(
            trigger_tokens(1_000_000, 8_000, DEFAULT_TRIGGER_PCT),
            496_000
        );
        // Tiny pct: the (uncapped) 64k floor binds.
        assert_eq!(trigger_tokens(1_000_000, 8_000, 0.05), 64_000);
    }

    #[test]
    fn trigger_never_exceeds_effective_window() {
        // eff = 1k: base 500, floor min(850, 64k) = 850, small floor 750
        // → 850.
        assert_eq!(trigger_tokens(9_000, 8_000, DEFAULT_TRIGGER_PCT), 850);
        // eff = 2k: base 1000, floor 1700 → 1700.
        assert_eq!(trigger_tokens(10_000, 8_000, DEFAULT_TRIGGER_PCT), 1_700);
    }

    #[test]
    fn zero_output_and_degenerate_inputs_are_safe() {
        assert_eq!(trigger_tokens(0, 0, DEFAULT_TRIGGER_PCT), 1);
        // 200k window, no output cap → eff 200k: 50% = 100k, small-window
        // floor 150k → 150k.
        assert_eq!(trigger_tokens(200_000, 0, DEFAULT_TRIGGER_PCT), 150_000);
    }
}
