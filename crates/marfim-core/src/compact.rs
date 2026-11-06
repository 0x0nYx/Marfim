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
