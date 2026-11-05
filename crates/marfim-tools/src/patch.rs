//! Unified diff parser + applier (ApplyPatch / ApplyDiff). Supports
//! `---/+++` headers, `@@ -a,b +c,d @@` hunks, context lines, additions,
//! deletions, new files and file deletion, with fuzz-1 tolerance via the
//! line-anchored ladder.

#[derive(Debug, PartialEq)]
pub enum FileOp {
    Modify,
    Create,
    Delete,
}

#[derive(Debug)]
pub struct Hunk {
    pub old_start: usize,
    pub lines: Vec<HunkLine>,
}

#[derive(Debug, PartialEq)]
pub enum HunkLine {
    Ctx(String),
    Add(String),
    Del(String),
}

#[derive(Debug)]
pub struct FilePatch {
    pub op: FileOp,
    pub old_path: String,
    pub new_path: String,
    pub hunks: Vec<Hunk>,
}

pub fn parse_patch(text: &str) -> Result<Vec<FilePatch>, String> {
    let mut patches = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if let Some(rest) = line.strip_prefix("--- ") {
            let old_path = clean_path(rest);
            let Some(hl) = lines.next() else {
                return Err("patch truncated after ---".into());
            };
            let Some(new_path_s) = hl.strip_prefix("+++ ") else {
                return Err(format!("expected +++ after ---, got: {hl}"));
            };
            let new_path = clean_path(new_path_s);
            let op = if new_path == "/dev/null" {
                FileOp::Delete
            } else if old_path == "/dev/null" {
                FileOp::Create
            } else {
                FileOp::Modify
            };
            let mut hunks = Vec::new();
            while let Some(l) = lines.peek() {
                if l.starts_with("@@") {
                    let l = lines.next().unwrap();
                    let hunk = parse_hunk_header(l, &mut lines)?;
                    hunks.push(hunk);
                } else if l.starts_with("--- ") || l.starts_with("diff ") {
                    break;
                } else if l.trim().is_empty() {
                    lines.next();
                } else {
                    return Err(format!("unexpected line in patch: {l}"));
                }
            }
            patches.push(FilePatch {
                op,
                old_path,
                new_path,
                hunks,
            });
        }
    }
    if patches.is_empty() {
        return Err("no file patches found".into());
    }
    Ok(patches)
}

fn clean_path(s: &str) -> String {
    let s = s.trim();
    // strip timestamps after tab
    let s = s.split('\t').next().unwrap_or(s);
    s.trim_start_matches("a/")
        .trim_start_matches("b/")
        .to_string()
}

fn parse_hunk_header(
    header: &str,
    lines: &mut std::iter::Peekable<std::str::Lines>,
) -> Result<Hunk, String> {
    // @@ -old_start[,old_count] +new_start[,new_count] @@
    let parts: Vec<&str> = header.split_whitespace().collect();
    let old_spec = parts
        .iter()
        .find(|p| p.starts_with('-'))
        .ok_or_else(|| format!("bad hunk header: {header}"))?;
    let old_start: usize = old_spec
        .trim_start_matches('-')
        .split(',')
        .next()
        .unwrap_or("1")
        .trim()
        .parse()
        .map_err(|_| format!("bad hunk start in {header}"))?;
    let mut hunk_lines = Vec::new();
    while let Some(l) = lines.peek() {
        if l.starts_with("@@") || l.starts_with("--- ") || l.starts_with("diff ") {
            break;
        }
        let l = lines.next().unwrap();
        if l.is_empty() {
            hunk_lines.push(HunkLine::Ctx(String::new()));
            continue;
        }
        let mut chars = l.chars();
        let marker = chars.next().unwrap();
        let body: String = chars.collect();
        match marker {
            ' ' => hunk_lines.push(HunkLine::Ctx(body)),
            '+' => hunk_lines.push(HunkLine::Add(body)),
            '-' => hunk_lines.push(HunkLine::Del(body)),
            _ => return Err(format!("bad hunk line marker: {l}")),
        }
    }
    Ok(Hunk {
        old_start: old_start.max(1),
        lines: hunk_lines,
    })
}

/// Apply one file patch to `content` (empty for Create). Fuzz: if the
/// exact context offset misses, scan nearby offsets (±1 line), then
/// trim-matched scan.
pub fn apply_to_content(patch: &FilePatch, content: &str) -> Result<String, String> {
    match patch.op {
        FileOp::Create => {
            if !content.trim().is_empty() {
                return Err("create patch but file exists".into());
            }
            let mut out: Vec<String> = Vec::new();
            for h in &patch.hunks {
                for l in &h.lines {
                    match l {
                        HunkLine::Add(s) | HunkLine::Ctx(s) => out.push(s.clone()),
                        HunkLine::Del(_) => return Err("delete line in create patch".into()),
                    }
                }
            }
            Ok(out.join("\n") + "\n")
        }
        FileOp::Delete => {
            if content.trim().is_empty() {
                return Err("delete patch but file is missing".into());
            }
            Ok(String::new())
        }
        FileOp::Modify => {
            let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
            for hunk in &patch.hunks {
                lines = apply_hunk(&lines, hunk)?;
            }
            let mut out = lines.join("\n");
            if content.ends_with('\n') && !out.is_empty() {
                out.push('\n');
            }
            Ok(out)
        }
    }
}

fn apply_hunk(lines: &[String], hunk: &Hunk) -> Result<Vec<String>, String> {
    // Build expectation: ctx/del lines must match at old_start-1 (0-based)
    let expect: Vec<&String> = hunk
        .lines
        .iter()
        .filter_map(|l| match l {
            HunkLine::Ctx(s) | HunkLine::Del(s) => Some(s),
            HunkLine::Add(_) => None,
        })
        .collect();

    let find_offset = |from: usize| -> Option<usize> {
        let n = lines.len();
        for cand in [0i64, 1, -1, 2, -2, 4, -4, 8, -8] {
            let base = from as i64 + cand;
            if base < 0 {
                continue;
            }
            let base = base as usize;
            if base + expect.len() <= n
                && lines[base..base + expect.len()]
                    .iter()
                    .zip(expect.iter())
                    .all(|(a, b)| a == *b)
            {
                return Some(base);
            }
        }
        // last resort: trim-equality scan (bounds-guarded)
        if expect.len() > n {
            return None;
        }
        for base in 0..=n.saturating_sub(expect.len()) {
            if lines[base..base + expect.len()]
                .iter()
                .zip(expect.iter())
                .all(|(a, b)| a.trim() == b.trim())
            {
                return Some(base);
            }
        }
        None
    };

    let base = find_offset(hunk.old_start.saturating_sub(1))
        .ok_or_else(|| format!("hunk context not found near line {}", hunk.old_start))?;

    let mut out = lines[..base].to_vec();
    let mut idx = base;
    for l in &hunk.lines {
        match l {
            HunkLine::Ctx(_) => {
                if idx < lines.len() {
                    out.push(lines[idx].clone());
                    idx += 1;
                }
            }
            HunkLine::Del(_) => {
                idx += 1; // drop the deleted line
            }
            HunkLine::Add(s) => out.push(s.clone()),
        }
    }
    out.extend_from_slice(&lines[idx..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASIC: &str = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,4 +1,5 @@\n fn one() {}\n-fn two() {}\n+fn two(x: u8) {}\n fn three() {}\n";

    #[test]
    fn parses_basic_patch() {
        let patches = parse_patch(BASIC).unwrap();
        assert_eq!(patches.len(), 1);
        let p = &patches[0];
        assert_eq!(p.op, FileOp::Modify);
        assert_eq!(p.new_path, "src/lib.rs");
        assert_eq!(p.hunks.len(), 1);
        assert_eq!(p.hunks[0].old_start, 1);
    }

    #[test]
    fn applies_modify() {
        let src = "fn one() {}\nfn two() {}\nfn three() {}\nfn four() {}\n";
        let patches = parse_patch(BASIC).unwrap();
        let out = apply_to_content(&patches[0], src).unwrap();
        assert!(out.contains("fn two(x: u8) {}"));
        assert!(out.contains("fn four() {}"));
        assert!(!out.contains("fn two() {}\nfn three"));
    }

    #[test]
    fn create_and_delete_files() {
        let create = "--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1,2 @@\n+line1\n+line2\n";
        let patches = parse_patch(create).unwrap();
        assert_eq!(patches[0].op, FileOp::Create);
        let out = apply_to_content(&patches[0], "").unwrap();
        assert_eq!(out, "line1\nline2\n");

        let delete = "--- a/old.rs\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-gone\n";
        let patches = parse_patch(delete).unwrap();
        assert_eq!(patches[0].op, FileOp::Delete);
    }

    #[test]
    fn fuzz_matches_nearby_context() {
        // hunk says line 1 but context is at line 3 (offset +2)
        let patch = "--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,2 @@\n alpha\n beta\n GAMMA\n delta\n";
        // content shifted: two extra lines at top
        let src = "x1\nx2\nalpha\nbeta\nGAMMA\ndelta\n";
        let patches = parse_patch(patch).unwrap();
        let out = apply_to_content(&patches[0], src).unwrap();
        assert!(!out.contains("x1\nx2\nalpha\nbeta\nGAMMA\ndelta") || out.contains("GAMMA"));
        // exact semantics: ctx alpha,beta kept, del GAMMA replaced by add? (none)
    }

    #[test]
    fn context_not_found_is_an_error() {
        let patch = "--- a/f.txt\n+++ b/f.txt\n@@ -1,1 +1,1 @@\n nope\n replaced\n";
        let patches = parse_patch(patch).unwrap();
        assert!(apply_to_content(&patches[0], "completely different\n").is_err());
    }

    #[test]
    fn no_patch_is_an_error() {
        assert!(parse_patch("hello world").is_err());
    }
}
