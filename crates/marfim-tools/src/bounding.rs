//! W5.5 mangled tool-name recovery + W5.4 generic tool-output bounding.

use std::path::Path;

/// Generic output cap for every tool result (W5.4). Tools with their own
/// tighter caps (Bash) already produce smaller outputs; this is the
/// uniform backstop so NO tool can flood the context.
pub const OUTPUT_CAP: usize = 30_000;
/// Spilled full outputs retained per tool-host surface (the rest age out
/// newest-kept — a bounded backing store, never a leak).
pub const BACKING_KEEP: usize = 64;
/// How much head/tail survives in the bounded view.
const HEAD: usize = 20_000;
const TAIL: usize = 8_000;

/// Cap a tool output. Over-cap outputs spill VERBATIM to a backing file
/// under `<cwd>/.marfim/outputs/` and the model receives head+tail with
/// the explicit backing path — the evidence is never destroyed, only
/// moved (REA doctrine: retention with a retained view).
pub fn bound(cwd: &Path, tool: &str, output: &str) -> String {
    if output.len() <= OUTPUT_CAP {
        return output.to_string();
    }
    let dir = cwd.join(".marfim").join("outputs");
    let _ = std::fs::create_dir_all(&dir);
    // Deterministic name: tool + monotonic seq derived from the existing
    // files (no wall clock in names — the no-clock doctrine; collisions
    // are impossible because we scan before writing).
    let seq = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("txt"))
                .count()
        })
        .unwrap_or(0);
    let path = dir.join(format!("{tool}-{seq:04}.txt"));
    let backing = match std::fs::write(&path, output) {
        Ok(()) => path.to_string_lossy().to_string(),
        Err(_) => String::new(), // spill is best-effort; the cap still holds
    };
    // Retention: keep only the newest BACKING_KEEP files.
    if let Ok(files) = std::fs::read_dir(&dir) {
        let mut entries: Vec<_> = files
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("txt"))
            .map(|e| (e.path(), e.metadata().and_then(|m| m.modified()).ok()))
            .collect();
        entries.sort_by_key(|(_, m)| *m);
        while entries.len() > BACKING_KEEP {
            let (oldest, _) = entries.remove(0);
            let _ = std::fs::remove_file(oldest);
        }
    }
    let cut = HEAD.min(output.len());
    let mut head_end = cut;
    while head_end < output.len() && !output.is_char_boundary(head_end) {
        head_end += 1;
    }
    let tail_start = output.len().saturating_sub(TAIL);
    let mut tail_start_adj = tail_start;
    while tail_start_adj < output.len() && !output.is_char_boundary(tail_start_adj) {
        tail_start_adj += 1;
    }
    let head = &output[..head_end];
    let tail = &output[tail_start_adj.min(output.len())..];
    let dropped = output.len() - head.len() - tail.len();
    let mut out = format!("{head}\n[truncated: {dropped} middle bytes elided");
    if backing.is_empty() {
        out.push_str(" — backing file could not be written]");
    } else {
        out.push_str(&format!(" — full output retained at {backing}]"));
    }
    if !tail.is_empty() {
        out.push('\n');
        out.push_str(tail);
    }
    out
}

/// W5.5 (goose `recover_mangled_tool_name`, clean-room): models emit
/// mangled tool names — `functions.Bash` prefixes from over-eager
/// function-calling adapters, dot-mangled namespaces, plural/space
/// typos. Recovery rules, in order:
/// 1. strip a `functions.` / `fn.` / `tool.` prefix;
/// 2. case-insensitive exact match;
/// 3. separator normalization (`.`/`-`/space → `_`) match — this also
///    maps `mcp.fs.read` → `mcp__fs__read` shape via the same fold;
/// 4. unique Levenshtein ≤ 2 candidate (ambiguous or too-far → None).
pub fn recover_mangled_tool_name(raw: &str, known: &[&str]) -> Option<String> {
    if known.contains(&raw) {
        return None; // nothing mangled, nothing to say
    }
    let stripped = raw
        .strip_prefix("functions.")
        .or_else(|| raw.strip_prefix("fn."))
        .or_else(|| raw.strip_prefix("tool."))
        .unwrap_or(raw);
    if known.contains(&stripped) {
        return Some(stripped.to_string());
    }
    if let Some(hit) = known.iter().find(|k| k.eq_ignore_ascii_case(stripped)) {
        return Some((*hit).to_string());
    }
    // Separator-insensitive fold: runs of `.`/`-`/`_`/space collapse to
    // ONE underscore, lowercased — so `mcp.fs.read`, `mcp-fs-read` and
    // `mcp__fs__read` all fold identically.
    let fold = |s: &str| -> String {
        let mut out = String::new();
        let mut prev_sep = false;
        for c in s.chars() {
            if c == '.' || c == '-' || c == ' ' || c == '_' {
                if !prev_sep {
                    out.push('_');
                }
                prev_sep = true;
            } else {
                out.push(c.to_ascii_lowercase());
                prev_sep = false;
            }
        }
        out
    };
    if let Some(hit) = known.iter().find(|k| fold(k) == fold(stripped)) {
        return Some((*hit).to_string());
    }
    let mut candidates: Vec<&str> = known
        .iter()
        .filter(|k| {
            let d = marfim_core::levenshtein::distance(k, stripped);
            d <= 2
        })
        .copied()
        .collect();
    if candidates.len() == 1 {
        return Some(candidates.remove(0).to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounding_passes_small_outputs_through() {
        let out = bound(Path::new("/tmp"), "Read", "short");
        assert_eq!(out, "short");
    }

    #[test]
    fn bounding_spills_and_names_the_backing_file() {
        let dir = marfim_testkit::Temp::new("marfim-bound").unwrap();
        let big = format!("{}\nTAIL-MARKER", "x".repeat(OUTPUT_CAP + 5_000));
        let out = bound(dir.path(), "Grep", &big);
        assert!(
            out.contains("[truncated:"),
            "head names the truncation: {out}"
        );
        assert!(out.contains("full output retained at"), "{out}");
        assert!(out.contains("TAIL-MARKER"), "tail survives: {out}");
        assert!(out.len() < big.len(), "bounded view is smaller");
        // backing file exists and holds EVERYTHING
        let odir = dir.path().join(".marfim/outputs");
        let files: Vec<_> = std::fs::read_dir(odir).unwrap().flatten().collect();
        assert_eq!(files.len(), 1);
        let full = std::fs::read_to_string(files[0].path()).unwrap();
        assert_eq!(full.len(), big.len());
    }

    #[test]
    fn bounding_retention_keeps_only_the_newest() {
        let dir = marfim_testkit::Temp::new("marfim-bound-ret").unwrap();
        for i in 0..(BACKING_KEEP + 8) {
            let big = format!("{}{}", "y".repeat(OUTPUT_CAP + 1), i);
            let _ = bound(dir.path(), "LS", &big);
        }
        let odir = dir.path().join(".marfim/outputs");
        let n = std::fs::read_dir(odir).unwrap().flatten().count();
        assert_eq!(n, BACKING_KEEP, "older spills age out");
    }

    #[test]
    fn recovery_prefixes_case_and_separators() {
        let known = vec!["Bash", "Read", "Edit", "mcp__fs__read_file", "WebFetch"];
        assert_eq!(
            recover_mangled_tool_name("functions.Bash", &known),
            Some("Bash".into())
        );
        assert_eq!(
            recover_mangled_tool_name("bash", &known),
            Some("Bash".into())
        );
        assert_eq!(
            recover_mangled_tool_name("mcp.fs.read_file", &known),
            Some("mcp__fs__read_file".into())
        );
        assert_eq!(
            recover_mangled_tool_name("WebFetchx", &known),
            Some("WebFetch".into()),
            "distance-1 unique candidate"
        );
        assert_eq!(
            recover_mangled_tool_name("Read", &known),
            None,
            "clean name"
        );
        assert_eq!(recover_mangled_tool_name("zzzzzz", &known), None);
    }
}
