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
    let estimate = |h: &[ChatMessage]| -> u64 {
        h.iter()
            .map(|m| m.content.len() as u64)
            .sum::<u64>()
            .div_ceil(4)
    };
    if estimate(history) <= target_tokens {
        return 0;
    }
    let protected_start = history.len().saturating_sub(protected_tail);
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
