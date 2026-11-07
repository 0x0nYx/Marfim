//! Edit ladder (INV-11): exact match → whitespace-flexible →
//! line-anchored. Identifier-fuzzy matching rejected (bug-compat #3).

/// Ladder rung 1: exact substring replace.
pub fn exact(content: &str, old: &str, new: &str, replace_all: bool) -> Option<String> {
    if !content.contains(old) {
        return None;
    }
    Some(if replace_all {
        content.replace(old, new)
    } else {
        content.replacen(old, new, 1)
    })
}

/// Ladder rung 3: whitespace-flexible — collapse whitespace runs in both
/// content and needle for matching, then splice the replacement over the
/// original byte span.
pub fn whitespace_flexible(content: &str, old: &str, new: &str) -> Option<String> {
    let norm = |s: &str| -> String { s.split_whitespace().collect::<Vec<_>>().join(" ") };
    // normalized content + map from normalized char index → original byte offset
    let mut norm_content = String::new();
    let mut offsets: Vec<usize> = Vec::new(); // byte offset of each normalized char start
    let mut prev_ws = false;
    for (idx, ch) in content.char_indices() {
        if ch.is_whitespace() {
            if !prev_ws && !norm_content.is_empty() {
                norm_content.push(' ');
                offsets.push(idx);
                prev_ws = true;
            }
            continue;
        }
        prev_ws = false;
        norm_content.push(ch);
        offsets.push(idx);
    }
    // end offset for each normalized char = next char's byte offset (or len)
    let mut end_offsets: Vec<usize> = offsets.iter().skip(1).copied().collect();
    end_offsets.push(content.len());

    let needle = norm(old);
    if needle.is_empty() {
        return None;
    }
    let start_idx = norm_content.find(&needle)?;
    let start = offsets[start_idx];
    let end = end_offsets[start_idx + needle.chars().count() - 1];
    // the original span must normalize to the same needle (sanity)
    if norm(&content[start..end]) != needle {
        return None;
    }
    let mut out = String::with_capacity(content.len());
    out.push_str(&content[..start]);
    out.push_str(new);
    out.push_str(&content[end..]);
    Some(out)
}

/// Ladder rung 3: line-anchored — `old` matches a run of lines whose
/// trimmed contents equal the trimmed needle lines (indentation may
/// differ); replacement preserves the ORIGINAL indentation of the first
/// matched line.
pub fn line_anchored(content: &str, old: &str, new: &str) -> Option<String> {
    let mut needle_lines: Vec<&str> = old.lines().collect();
    // leading/trailing empty lines (from delimiter newlines) are spurious
    while needle_lines
        .first()
        .map(|l| l.trim().is_empty())
        .unwrap_or(false)
    {
        needle_lines.remove(0);
    }
    while needle_lines
        .last()
        .map(|l| l.trim().is_empty())
        .unwrap_or(false)
    {
        needle_lines.pop();
    }
    if needle_lines.is_empty() {
        return None;
    }
    let hay: Vec<&str> = content.lines().collect();
    let trim_eq = |a: &str, b: &str| a.trim() == b.trim();
    for start in 0..=hay.len().saturating_sub(needle_lines.len()) {
        let window = &hay[start..start + needle_lines.len()];
        if window
            .iter()
            .zip(needle_lines.iter())
            .all(|(h, n)| trim_eq(h, n))
        {
            // preserve original indentation of first matched line,
            // then emit the NEW lines re-indented to that level
            let indent: String = hay[start]
                .chars()
                .take_while(|c| c.is_whitespace())
                .collect();
            let mut rl: Vec<&str> = new.lines().collect();
            while rl.first().map(|l| l.trim().is_empty()).unwrap_or(false) {
                rl.remove(0);
            }
            while rl.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
                rl.pop();
            }
            let replacement: String = rl
                .iter()
                .map(|l| {
                    if l.trim().is_empty() {
                        String::new()
                    } else {
                        format!("{indent}{}", l.trim_start())
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            let mut out: Vec<String> = Vec::with_capacity(hay.len());
            out.extend(hay[..start].iter().map(|s| s.to_string()));
            out.extend(replacement.lines().map(str::to_string));
            out.extend(
                hay[start + needle_lines.len()..]
                    .iter()
                    .map(|s| s.to_string()),
            );
            return Some(out.join("\n") + if content.ends_with('\n') { "\n" } else { "" });
        }
    }
    None
}

/// The ladder: try rungs in order; return (result, rung_name).
pub fn apply_edit(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Option<(String, &'static str)> {
    if let Some(r) = exact(content, old, new, replace_all) {
        return Some((r, "exact"));
    }
    if let Some(r) = line_anchored(content, old, new) {
        return Some((r, "line-anchored"));
    }
    if let Some(r) = whitespace_flexible(content, old, new) {
        return Some((r, "whitespace-flexible"));
    }
    if let Some(r) = fuzzy_dominant(content, old, new) {
        return Some((r, "fuzzy-dominant"));
    }
    None
}

// ---------------------------------------------------------------------
// §S fuzzy rung (distilled: OMP `pi-edit/fuzzy.rs` policy — constants
// captured verbatim). After the three structural rungs fail, a
// line-window fuzzy match applies ONLY when it is unambiguous:
//   - similarity = 1 − banded_levenshtein / max(len) on TRIMMED lines,
//     window = the needle's line count;
//   - AUTO-APPLY requires best ≥ 0.97 AND dominance +0.08 over the
//     runner-up (OMP constants 0.97 / 0.08);
//   - everything weaker (0.95 / 0.92 / 0.8 bands) NEVER auto-applies —
//     Marfim has no mid-edit confirm surface, so fail-closed is the
//     only honest policy here.
// Replacement preserves the matched window's original first-line
// indentation (same contract as the line-anchored rung).

/// Auto-apply similarity threshold.
pub const FUZZY_DOMINANT_SIMILARITY: f64 = 0.97;
/// Required margin over the runner-up (unambiguity).
pub const FUZZY_DOMINANCE_MARGIN: f64 = 0.08;
/// Levenshtein band cap (edit distance beyond this can never reach the
/// similarity threshold on realistic lines — bounded work).
fn band_for(len: usize) -> usize {
    (len / 8 + 4).min(64)
}

/// Banded Levenshtein: `None` when the distance provably exceeds the
/// band (no full-matrix blowup). Same metric as the full algorithm.
fn banded_distance(a: &[char], b: &[char], band: usize) -> Option<usize> {
    if a.len().abs_diff(b.len()) > band {
        return None;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        // cells outside the band stay at "infinite" (band + 1 sentinel)
        let lo = i.saturating_sub(band).max(1);
        let hi = (i + band).min(b.len());
        for j in 1..=b.len() {
            if j < lo || j > hi {
                cur[j] = band + 1;
                continue;
            }
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1)
                .min(cur[j - 1] + 1)
                .min(prev[j - 1].min(band + 1) + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    let d = prev[b.len()];
    if d > band {
        None
    } else {
        Some(d)
    }
}

fn line_similarity(a: &str, b: &str) -> f64 {
    let av: Vec<char> = a.trim().chars().collect();
    let bv: Vec<char> = b.trim().chars().collect();
    if av.is_empty() && bv.is_empty() {
        return 1.0;
    }
    let max = av.len().max(bv.len()).max(1);
    match banded_distance(&av, &bv, band_for(max)) {
        Some(d) => 1.0 - (d as f64 / max as f64),
        None => 0.0,
    }
}

/// The fuzzy-dominant rung. `None` when no window clears BOTH bars.
pub fn fuzzy_dominant(content: &str, old: &str, new: &str) -> Option<String> {
    let needle_lines: Vec<&str> = old.lines().filter(|l| !l.trim().is_empty()).collect();
    if needle_lines.is_empty() {
        return None;
    }
    let hay: Vec<&str> = content.lines().collect();
    let n = needle_lines.len();
    if hay.len() < n {
        return None;
    }
    // score every window: mean per-line similarity, index-ordered scan
    let mut windows: Vec<(usize, f64)> = Vec::new();
    for start in 0..=(hay.len() - n) {
        let s: f64 = (0..n)
            .map(|k| line_similarity(hay[start + k], needle_lines[k]))
            .sum::<f64>()
            / n as f64;
        windows.push((start, s));
    }
    windows.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    let best = windows[0];
    if best.1 < FUZZY_DOMINANT_SIMILARITY {
        return None;
    }
    // dominance: the runner-up must be a DIFFERENT window and worse by
    // the margin (windows may tie at the same span only when n lines
    // overlap — a different start means a different target).
    let second = windows
        .iter()
        .skip(1)
        .find(|(i, _)| *i != best.0)
        .map(|(_, s)| *s)
        .unwrap_or(0.0);
    if best.1 - second < FUZZY_DOMINANCE_MARGIN {
        return None;
    }
    // splice: preserve the matched window's first-line indentation
    let start = best.0;
    let indent: String = hay[start]
        .chars()
        .take_while(|c| c.is_whitespace())
        .collect();
    let mut rl: Vec<&str> = new.lines().collect();
    while rl.first().map(|l| l.trim().is_empty()).unwrap_or(false) {
        rl.remove(0);
    }
    while rl.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        rl.pop();
    }
    let mut out: Vec<String> = Vec::with_capacity(hay.len());
    out.extend(hay[..start].iter().map(|s| s.to_string()));
    out.extend(rl.iter().map(|l| {
        if l.trim().is_empty() {
            String::new()
        } else {
            format!("{indent}{}", l.trim_start())
        }
    }));
    out.extend(hay[start + n..].iter().map(|s| s.to_string()));
    Some(out.join("\n") + if content.ends_with('\n') { "\n" } else { "" })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "fn main() {\n    let x = 1;\n    println!(\"{x}\");\n}\n";

    #[test]
    fn rung1_exact() {
        let (r, rung) = apply_edit(SRC, "let x = 1;", "let x = 2;", false).unwrap();
        assert_eq!(rung, "exact");
        assert!(r.contains("let x = 2;"));
    }

    #[test]
    fn rung2_line_anchored_handles_indentation_drift() {
        // model emitted the line with wrong indentation; leading \n keeps
        // the old_string from suffix-matching the 4-space original exactly
        let old = "\n  let x = 1;";
        let (r, rung) = apply_edit(SRC, old, "\n    let x = 2;", false).unwrap();
        assert_eq!(rung, "line-anchored");
        assert!(r.contains("let x = 2;"));
        // indentation of the original line preserved on replacement lines
        assert!(r.contains("\n    let x = 2;"), "{r}");
    }

    #[test]
    fn rung3_whitespace_flexible() {
        let old = "let  x   = 1;"; // weird internal spacing
        let (r, rung) = apply_edit(SRC, old, "let x = 3;", false).unwrap();
        assert_eq!(rung, "whitespace-flexible");
        assert!(r.contains("let x = 3;"));
    }

    // ---- §S fuzzy-dominant rung ----

    #[test]
    fn fuzzy_applies_when_dominant_and_97pct_similar() {
        // one slipped character on a LONG line: structural rungs all
        // miss (exact/trimmed/normalized all differ), the fuzzy window
        // is unique and clears both bars:
        //   sim(line) = 1 − 1/49 = 0.9796 ≥ 0.97
        //   mean(0.9796, 1.0, 1.0) = 0.993; runner-up window is far below
        let src = "fn main() {\n    let configuration_value = compute_default(\"file\");\n    let done = 2;\n    let fin = 3;\n}\n";
        let old =
            "let configuratioon_value = compute_default(\"file\");\nlet done = 2;\nlet fin = 3;";
        let new =
            "let configuration_value = compute_default(\"fyle\");\nlet done = 2;\nlet fin = 3;";
        let (r, rung) = apply_edit(src, old, new, false).unwrap();
        assert_eq!(rung, "fuzzy-dominant");
        assert!(r.contains("let configuration_value"), "{r}");
        assert!(
            r.contains("    let configuration_value"),
            "indent preserved: {r}"
        );
    }

    #[test]
    fn fuzzy_refuses_ambiguous_matches() {
        // TWO windows at the SAME similarity → dominance margin = 0 →
        // refuse (no silent choice between identical candidates)
        let src = "let alpha_bet_a = compute_something_long(1);\nlet alpha_bet_a = compute_something_long(1);\n";
        let old = "let alpha_bet_a = compute_somethong_long(1);"; // 1-char typo
        assert!(fuzzy_dominant(src, old, "replaced").is_none());
        assert!(apply_edit(src, old, "replaced", false).is_none());
    }

    #[test]
    fn fuzzy_refuses_low_similarity() {
        let src = "alpha beta gamma\n";
        assert!(fuzzy_dominant(src, "totally different text", "x").is_none());
    }

    #[test]
    fn banded_distance_matches_full_levenshtein_within_band() {
        // cross-check against marfim-core's full algorithm on inputs
        // inside the band
        for (a, b) in [
            ("kitten", "sitting"),
            ("model", "modle"),
            ("abc", "abc"),
            ("conts", "count"),
        ] {
            let av: Vec<char> = a.chars().collect();
            let bv: Vec<char> = b.chars().collect();
            let full = marfim_core::levenshtein::distance(a, b);
            let banded = banded_distance(&av, &bv, 64).unwrap();
            assert_eq!(full, banded, "{a} vs {b}");
        }
    }

    #[test]
    fn banded_distance_none_beyond_band() {
        let a: Vec<char> = "aaaaaaaaaa".chars().collect();
        let b: Vec<char> = "bbbbbbbbbbbbbbbbbbbb".chars().collect();
        assert_eq!(banded_distance(&a, &b, 4), None, "length diff > band");
    }

    #[test]
    fn no_match_is_none() {
        assert!(apply_edit(SRC, "nothing here", "x", false).is_none());
    }

    #[test]
    fn replace_all_hits_every_instance() {
        let src = "a b a b a";
        let (r, _) = apply_edit(src, "a", "z", true).unwrap();
        assert_eq!(r, "z b z b z");
        let (r2, _) = apply_edit(src, "a", "z", false).unwrap();
        assert_eq!(r2, "z b a b a");
    }

    #[test]
    fn line_anchored_preserves_trailing_newline() {
        let (r, _) = apply_edit(SRC, "fn main() {", "fn main() { // hi", false).unwrap();
        assert!(r.ends_with('\n'));
    }
}
