//! W5.1 hashline edit anchors (distilled: OMP `pi-edit` hashline mode,
//! clean-room): a Read stamps its output with `[path#tag]`, where `tag`
//! is a 4-hex content snapshot of the file AT READ TIME. An Edit may
//! carry the anchor (`path#tag`); a mismatch means the file changed
//! under the model since its Read — the edit REFUSES with a re-read
//! instruction. The model literally cannot edit against stale reads.
//!
//! The tag is a COURTESY anchor, not a security control: 16 bits detect
//! "some edit happened between your read and your write" (the common
//! case — a concurrent tool run) with near-certainty, while keeping the
//! budget OMP measured (−61% tokens vs re-pasting content). Absent tag
//! = legacy behavior (byte-identical), so the surface is opt-in per
//! call and every existing flow keeps working.

/// FNV-1a 64-bit over the bytes, folded to 16 bits, printed 4-hex.
/// Deterministic across processes and platforms (no wall clock, no
/// randomness) — the doctrine for output-affecting paths.
pub fn content_tag(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    let folded = (h ^ (h >> 32) ^ (h >> 16)) as u16;
    format!("{folded:04x}")
}

/// Split an optional `#tag` anchor off a path argument: `path#1a2b` →
/// (`path`, Some("1a2b")). The suffix is an anchor ONLY when it is
/// exactly 4 hex chars — anything else stays part of the path
/// (deterministic fallback, mirroring the selector-grammar doctrine).
pub fn split_anchor(raw: &str) -> (&str, Option<&str>) {
    let Some((p, tag)) = raw.rsplit_once('#') else {
        return (raw, None);
    };
    let ok = !p.is_empty() && tag.len() == 4 && tag.chars().all(|c| c.is_ascii_hexdigit());
    if ok {
        (p, Some(tag))
    } else {
        (raw, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_is_deterministic_and_content_sensitive() {
        let a = content_tag("hello\nworld\n");
        assert_eq!(a, content_tag("hello\nworld\n"), "stable");
        assert_ne!(a, content_tag("hello\nworld!\n"), "content-sensitive");
        assert_eq!(a.len(), 4);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        // A one-byte change folds differently in the overwhelmingly common
        // case; assert the empty vs non-empty distinction at minimum.
        assert_ne!(content_tag(""), content_tag("x"));
    }

    #[test]
    fn anchor_split_is_strict() {
        let (p, tag) = split_anchor("src/main.rs#1a2b");
        assert_eq!((p, tag), ("src/main.rs", Some("1a2b")));
        // non-hex / wrong-length suffixes stay part of the path
        assert_eq!(split_anchor("src/main.rs#tag"), ("src/main.rs#tag", None));
        assert_eq!(
            split_anchor("src/main.rs#12345"),
            ("src/main.rs#12345", None)
        );
        assert_eq!(split_anchor("src/main.rs"), ("src/main.rs", None));
        // a bare trailing # is not an anchor
        assert_eq!(split_anchor("name#"), ("name#", None));
    }
}
