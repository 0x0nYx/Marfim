//! W6.4 trajectory compressor (distilled: Hermes `trajectory_compressor.py`,
//! clean-room): shrink a conversation trajectory to a token target while
//! preserving what models actually need:
//!
//! - the HEAD (the shaping first message) is protected;
//! - the FIRST message of every role is protected (the anchor of each
//!   register — a conversation whose first tool result vanished reads
//!   wrong);
//! - the LAST 4 messages are protected (the live work surface);
//! - removals snap to TOOL-PAIR boundaries: an assistant toolCall is
//!   never cut from its tool result (a dangling pair breaks every
//!   provider's replay);
//! - everything removed collapses into ONE bounded summary entry that
//!   lists what was dropped, honestly.

use crate::providers::ChatMessage;

/// Token targets from the Hermes spec constants.
pub const TARGET_TOKENS: usize = 15_250;
pub const SUMMARY_BUDGET_TOKENS: usize = 750;
/// The live work surface: never compress the tail below this.
pub const TAIL_PROTECT: usize = 4;

/// Rough token estimate (the loop's len/4 heuristic — this module stays
/// independent of provider state).
pub fn est_tokens(s: &str) -> usize {
    s.len().div_ceil(4)
}

/// Compress the trajectory to [`TARGET_TOKENS`]. Returns `(kept,
/// summary)` — the summary is a single user-role message carrying the
/// bounded digest of what was removed, or `None` when nothing needed
/// dropping.
pub fn trajectory_compress(messages: &[ChatMessage]) -> (Vec<ChatMessage>, Option<ChatMessage>) {
    trajectory_compress_to(messages, TARGET_TOKENS, SUMMARY_BUDGET_TOKENS)
}

pub fn trajectory_compress_to(
    messages: &[ChatMessage],
    target: usize,
    summary_budget: usize,
) -> (Vec<ChatMessage>, Option<ChatMessage>) {
    let total: usize = messages.iter().map(|m| est_tokens(&m.content)).sum();
    if total <= target || messages.is_empty() {
        return (messages.to_vec(), None);
    }

    let n = messages.len();
    let head_protect = 1.min(n); // the shaping first message
    let tail_protect = TAIL_PROTECT.min(n);
    let mut keep = vec![false; n];
    keep[..head_protect].fill(true);
    keep[n - tail_protect..].fill(true);
    // the FIRST message of each role is the role's anchor — mark it, and
    // only it (a seen-set, not a role-set).
    let mut seen_roles: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for (i, m) in messages.iter().enumerate() {
        if seen_roles.insert(m.role.as_str()) {
            keep[i] = true;
        }
    }

    // Drop oldest-unprotected-first until the budget fits, snapping to
    // tool-pair boundaries: when either half of a pair is protected,
    // BOTH stay.
    let mut kept_total: usize = messages
        .iter()
        .zip(&keep)
        .filter(|(_, k)| **k)
        .map(|(m, _)| est_tokens(&m.content))
        .sum();
    let mut dropped: Vec<String> = Vec::new();
    let mut i = 0;
    while i < n {
        if kept_total <= target {
            break;
        }
        if keep[i] {
            i += 1;
            continue;
        }
        // pair snap: assistant toolCall + its tool result move together.
        let is_call = !messages[i].tool_calls.is_empty();
        let is_result = messages[i].tool_call_id.is_some();
        let mut end = i;
        if is_call && i + 1 < n && messages[i + 1].tool_call_id.is_some() && !keep[i + 1] {
            end = i + 1;
        }
        if is_result && i > 0 && !messages[i - 1].tool_calls.is_empty() && !keep[i - 1] {
            // the call half is being handled when we reach it; skip the
            // result so we never drop it ahead of its call
            i += 1;
            continue;
        }
        // pair protection: if either half is protected, keep both
        let partner_protected =
            (is_call && i + 1 < n && messages[i + 1].tool_call_id.is_some() && keep[i + 1])
                || (is_result && i > 0 && !messages[i - 1].tool_calls.is_empty() && keep[i - 1]);
        if partner_protected {
            keep[i] = true;
            kept_total += est_tokens(&messages[i].content);
            i += 1;
            continue;
        }
        for j in i..=end {
            keep[j] = false;
            kept_total = kept_total.saturating_sub(est_tokens(&messages[j].content));
            dropped.push(format!(
                "{}: {}",
                messages[j].role,
                truncate_for_summary(&messages[j].content, 80)
            ));
        }
        i = end + 1;
    }

    // Pair repair (fixpoint): a protected half always drags its partner
    // back in — budget loses to integrity, every time.
    loop {
        let mut changed = false;
        for i in 0..n {
            if !keep[i] {
                continue;
            }
            let m = &messages[i];
            if !m.tool_calls.is_empty()
                && i + 1 < n
                && messages[i + 1].tool_call_id.is_some()
                && !keep[i + 1]
            {
                keep[i + 1] = true;
                changed = true;
            }
            if m.tool_call_id.is_some()
                && i > 0
                && !messages[i - 1].tool_calls.is_empty()
                && !keep[i - 1]
            {
                keep[i - 1] = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let kept: Vec<ChatMessage> = messages
        .iter()
        .zip(&keep)
        .filter(|(_, k)| **k)
        .map(|(m, _)| m.clone())
        .collect();
    if dropped.is_empty() {
        return (kept, None);
    }

    let mut summary = String::from("[trajectory summary: removed for context budget]\n");
    for d in &dropped {
        let line = format!("- {d}\n");
        if est_tokens(&summary) + est_tokens(&line) > summary_budget {
            summary.push_str("- [… older removals elided]\n");
            break;
        }
        summary.push_str(&line);
    }
    let smsg = ChatMessage::user(summary);
    (kept, Some(smsg))
}

fn truncate_for_summary(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.replace('\n', " ")
    } else {
        let mut end = max;
        while end < s.len() && !s.is_char_boundary(end) {
            end += 1;
        }
        format!("{}…", s[..end].replace('\n', " "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage::user(content) // role fixed below
            .with_role(role)
    }

    trait WithRole {
        fn with_role(self, role: &str) -> ChatMessage;
    }
    impl WithRole for ChatMessage {
        fn with_role(mut self, role: &str) -> ChatMessage {
            self.role = role.into();
            self
        }
    }

    fn big(role: &str, tag: &str, kb: usize) -> ChatMessage {
        msg(role, &format!("[{tag}] {}", "x".repeat(kb * 1024)))
    }

    fn call(id: &str, tag: &str, kb: usize) -> ChatMessage {
        let mut m = big("assistant", tag, kb);
        m.tool_calls = vec![crate::providers::ToolCall {
            id: id.into(),
            name: "Read".into(),
            arguments_json: "{}".into(),
        }];
        m
    }

    fn result(id: &str, tag: &str, kb: usize) -> ChatMessage {
        let mut m = big("tool", tag, kb);
        m.tool_call_id = Some(id.into());
        m
    }

    #[test]
    fn small_trajectories_pass_through_untouched() {
        let m = vec![msg("user", "hi"), msg("assistant", "hello")];
        let (kept, summary) = trajectory_compress(&m);
        assert_eq!(kept.len(), 2);
        assert!(summary.is_none());
    }

    #[test]
    fn compression_protects_head_tail_and_first_of_role() {
        let mut m = vec![msg("system", "shape the session")];
        m.push(big("user", "first-user", 20));
        for i in 0..10 {
            m.push(big("assistant", &format!("a{i}"), 20));
            m.push(big("tool", &format!("t{i}"), 20));
        }
        m.push(msg("user", "latest question"));
        m.push(msg("assistant", "working on it"));
        let (kept, summary) = trajectory_compress_to(&m, 8_000, 400);
        assert!(summary.is_some(), "big trajectory must compress");
        assert!(kept[0].content.contains("shape the session"));
        assert!(kept.iter().any(|x| x.content.contains("first-user")));
        assert!(
            kept.iter()
                .any(|x| x.role == "tool" && x.content.contains("t0")),
            "first tool result survives"
        );
        let tail_has_latest = kept
            .iter()
            .rev()
            .take(TAIL_PROTECT)
            .any(|x| x.content.contains("latest question"));
        assert!(tail_has_latest);
        assert!(
            est_tokens(&summary.as_ref().unwrap().content) <= 400 + 32,
            "summary within budget"
        );
    }

    #[test]
    fn tool_pairs_never_split() {
        let mut m = vec![msg("user", "go")];
        for i in 0..6 {
            m.push(call(&format!("c{i}"), &format!("call{i}"), 20));
            m.push(result(&format!("c{i}"), &format!("res{i}"), 20));
        }
        m.push(msg("assistant", "final"));
        let (kept, _s) = trajectory_compress_to(&m, 6_000, 300);
        for (i, k) in kept.iter().enumerate() {
            if !k.tool_calls.is_empty() {
                assert!(
                    i + 1 < kept.len()
                        && kept[i + 1].tool_call_id == k.tool_calls[0].id.clone().into(),
                    "pair integrity violated at {i}"
                );
            }
            if k.tool_call_id.is_some() {
                assert!(
                    i > 0 && !kept[i - 1].tool_calls.is_empty(),
                    "orphan result at {i}"
                );
            }
        }
    }

    #[test]
    fn summary_names_what_was_removed() {
        let mut m = vec![big("user", "first", 20)];
        for i in 0..8 {
            m.push(big("assistant", &format!("block-{i}"), 20));
        }
        m.push(msg("assistant", "tail"));
        let (_kept, summary) = trajectory_compress_to(&m, 10_000, 500);
        let s = summary.expect("summary exists");
        assert!(s.content.contains("trajectory summary"));
        assert!(
            s.content.contains("assistant:") || s.content.contains("user:"),
            "summary names roles: {}",
            s.content
        );
    }
}
