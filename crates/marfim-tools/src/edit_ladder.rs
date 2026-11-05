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
    None
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
