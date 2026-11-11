//! W10.1 (§S, Hermes memory-stack caps): the rendered memory surfaces are
//! HARD-CAPPED. Project memory renders at most [`MEMORY_CAP_CHARS`], the
//! user model at most [`USER_CAP_CHARS`]. The caps are drop-oldest: lines
//! are kept from the END of the text (the newest entries, since remember
//! appends), whole lines only, and the cut is announced with an exact
//! marker so the model can tell the surface was trimmed. Byte-identical
//! passthrough under the cap (reversible, observable).

/// Cap for the project memory surface (MEMORY.md analog).
pub const MEMORY_CAP_CHARS: usize = 2200;

/// Cap for the user-model surface (USER.md analog).
pub const USER_CAP_CHARS: usize = 1375;

/// The exact marker prepended when a cap dropped lines.
pub const CAP_MARKER: &str = "[memory: older entries dropped to fit the cap]";

/// Enforce a character cap over a rendered memory surface. Line-oriented:
/// whole lines are kept from the end (newest last), never a partial line,
/// and the marker is prepended only when something was actually dropped.
/// The result is ALWAYS ≤ `cap` chars (the marker counts against the
/// budget — a cap smaller than the marker yields the marker's head, the
/// honest degenerate case).
pub fn cap_chars(text: &str, cap: usize) -> String {
    if text.chars().count() <= cap {
        return text.to_string();
    }
    let marker_chars = CAP_MARKER.chars().count();
    if marker_chars >= cap {
        // honest degenerate case: even the marker cannot fit — the
        // truncated marker IS the surface (never a fake passthrough).
        return CAP_MARKER.chars().take(cap).collect();
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut used = marker_chars;
    for line in text.lines().rev() {
        let line_len = line.chars().count() + 1; // + newline
        if used + line_len > cap {
            break;
        }
        used += line_len;
        kept.push(line);
    }
    kept.reverse();
    let mut out = String::with_capacity(cap);
    out.push_str(CAP_MARKER);
    for line in kept {
        out.push('\n');
        out.push_str(line);
    }
    out
}

/// Cap for the project memory surface.
pub fn cap_memory(text: &str) -> String {
    cap_chars(text, MEMORY_CAP_CHARS)
}

/// Cap for the user-model surface.
pub fn cap_user(text: &str) -> String {
    cap_chars(text, USER_CAP_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_cap_is_byte_identical() {
        let t = "alpha\nbeta\ngamma";
        assert_eq!(cap_chars(t, 2200), t);
    }

    #[test]
    fn over_cap_drops_oldest_whole_lines_and_marks() {
        let mut t = String::new();
        // 100 lines of 30 chars = 3100 bytes with newlines > 2200.
        for i in 0..100 {
            t.push_str(&format!("line-{i:026}"));
            t.push('\n');
        }
        let capped = cap_memory(t.trim_end());
        assert!(capped.starts_with(CAP_MARKER));
        assert!(capped.chars().count() <= MEMORY_CAP_CHARS);
        // newest line survives, oldest dropped
        assert!(capped.contains("line-00000000000000000000000099"));
        assert!(!capped.contains("line-0000000000000000000000000"));
        // whole lines only: every kept line is complete
        for line in capped.lines().skip(1) {
            assert!(line.starts_with("line-"));
        }
    }

    #[test]
    fn user_cap_is_smaller() {
        let t = "x".repeat(2000);
        let capped = cap_user(&t);
        assert!(capped.starts_with(CAP_MARKER));
        assert!(capped.chars().count() <= USER_CAP_CHARS);
    }

    #[test]
    fn degenerate_tiny_cap_still_fits() {
        let capped = cap_chars("hello\nworld", 10);
        assert!(capped.chars().count() <= 10);
        // below the marker's own size the surface is the truncated marker
        assert!(capped.starts_with("[memory:"));
    }

    #[test]
    fn unicode_counted_by_chars_not_bytes() {
        // é is 2 bytes; 1500 chars = 3000 bytes > 2200 bytes but chars cap
        // measures CHARS, so 1500 chars fits under 2200.
        let t = "é".repeat(1500);
        assert_eq!(cap_memory(&t), t);
    }
}
