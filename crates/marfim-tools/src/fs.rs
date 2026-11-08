//! Filesystem tools: Read / Write / Edit / MultiEdit / Glob / Grep / LS.

use crate::edit_ladder;
use crate::registry::{ToolCtx, ToolOutput};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn resolve(cwd: &Path, p: &str) -> PathBuf {
    if Path::new(p).is_absolute() {
        PathBuf::from(p)
    } else {
        cwd.join(p)
    }
}

fn read_capped(path: &Path, max_bytes: usize) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{e}"))?;
    if meta.len() > max_bytes as u64 {
        return Err(format!(
            "file too large: {} bytes (cap {max_bytes})",
            meta.len()
        ));
    }
    std::fs::read_to_string(path).map_err(|e| format!("{e}"))
}

/// D24: Read supports `offset`/`limit` (0-based line offset, max lines)
/// and marks truncation EXPLICITLY — the model must never silently lose
/// the tail of a file.
///
/// §S selector grammar (distilled: OMP read tool): the `path` argument
/// may carry a suffix — `path:A-B` (1-based inclusive line range),
/// `path:-N` (last N lines), `path:raw` (whole file within the byte
/// cap, still bounded), `path:sum` (W5.2 structure summary — definition
/// lines only, bodies elided). Explicit ranges add 1 leading / 3
/// trailing context lines marked `[ctx]`. A suffix that does not match
/// the grammar is part of the path (deterministic fallback).
///
/// W5.1 hashline anchors: every content-bearing read opens with
/// `[path#tag]`; pass `path#tag` to Edit/Write to refuse stale edits
/// (see marfim_core::hashline).
pub fn read(_ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let Some(raw_path) = args["path"].as_str() else {
        return ToolOutput::err("missing `path`");
    };
    let (path, selector) = split_selector(raw_path);
    let full = resolve(&_ctx.cwd, path);
    match read_capped(&full, 512 * 1024) {
        Ok(text) => {
            // W5.1: the content tag is stamped on EVERY read shape — the
            // anchor is only useful when the model can always see it.
            let header = format!(
                "[{}#{}]\n",
                display_path(_ctx, path),
                marfim_core::hashline::content_tag(&text)
            );
            let lines: Vec<&str> = text.lines().collect();
            let total = lines.len();
            match selector {
                Some(Selector::Range(a, b)) => {
                    let mut r = range_output(path, &lines, a, b);
                    r.output.insert_str(0, &header);
                    r
                }
                Some(Selector::LastN(n)) => {
                    let n = n.min(total);
                    let start = total - n;
                    let mut out = header;
                    out.push_str(&format!("[last {n} of {total} lines]\n"));
                    for (i, line) in lines[start..].iter().enumerate() {
                        out.push_str(&format!("{:>6}\t{line}\n", start + i + 1));
                    }
                    ToolOutput::ok(out)
                }
                Some(Selector::Raw) => {
                    let mut out = header;
                    for (i, line) in lines.iter().enumerate() {
                        out.push_str(&format!("{:>6}\t{line}\n", i + 1));
                    }
                    ToolOutput::ok(out)
                }
                Some(Selector::Sum) => {
                    let mut out = header;
                    out.push_str(&summarize(&lines));
                    ToolOutput::ok(out)
                }
                None => {
                    let offset = args["offset"].as_u64().unwrap_or(0) as usize; // 0-based
                    let limit = args["limit"].as_u64().map(|v| v as usize).unwrap_or(2000);
                    if offset >= total && total > 0 {
                        return ToolOutput::ok(format!(
                            "{header}[truncated: offset {offset} is past end of file ({total} lines)]"
                        ));
                    }
                    let start = offset.min(total);
                    let end = (start.saturating_add(limit)).min(total);
                    let mut out = header;
                    for (i, line) in lines[start..end].iter().enumerate() {
                        out.push_str(&format!("{:>6}\t{line}\n", start + i + 1));
                    }
                    if end < total {
                        out.push_str(&format!(
                            "[truncated: showing lines {}-{} of {} — re-read with offset={} for more]\n",
                            start + 1,
                            end,
                            total,
                            end
                        ));
                    }
                    ToolOutput::ok(out)
                }
            }
        }
        Err(e) => ToolOutput::err(format!("Read failed for {path}: {e}")),
    }
}

/// The path as the model addressed it (cwd-relative when possible).
fn display_path(ctx: &ToolCtx, path: &str) -> String {
    let full = resolve(&ctx.cwd, path);
    full.strip_prefix(&ctx.cwd)
        .unwrap_or(&full)
        .to_string_lossy()
        .to_string()
}

/// W5.2: structure summary — the definition-shaped lines with their
/// numbers, everything else elided. Language-agnostic: a line counts
/// when its first word is a common definition keyword, or it opens a
/// top-level block (`{`/`:` terminator with no leading whitespace).
/// Deterministic, bounded, and honest about elision counts.
fn summarize(lines: &[&str]) -> String {
    const DEF_KEYWORDS: [&str; 16] = [
        "fn ",
        "pub ",
        "def ",
        "class ",
        "struct ",
        "enum ",
        "impl ",
        "trait ",
        "interface ",
        "type ",
        "const ",
        "func ",
        "module ",
        "namespace ",
        "package ",
        "import ",
    ];
    let total = lines.len();
    let mut kept = 0usize;
    let mut out = String::from("[structure summary: definition lines only]\n");
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let is_def = DEF_KEYWORDS.iter().any(|k| trimmed.starts_with(k))
            || (trimmed.starts_with("export ")
                && DEF_KEYWORDS
                    .iter()
                    .any(|k| trimmed[7..].trim_start().starts_with(k)));
        if is_def {
            kept += 1;
            out.push_str(&format!("{:>6}\t{line}\n", i + 1));
        }
    }
    out.push_str(&format!(
        "[{} of {total} lines shown; use the plain read (optionally with an offset) for bodies]\n",
        kept
    ));
    out
}

/// Explicit range output with 1-lead / 3-trail `[ctx]` context lines.
fn range_output(path: &str, lines: &[&str], a: usize, b: usize) -> ToolOutput {
    let total = lines.len();
    if total == 0 || a == 0 || a > b || a > total {
        return ToolOutput::err(format!(
            "range {a}-{b} invalid for {path} ({total} lines; ranges are 1-based inclusive)"
        ));
    }
    let a = a.min(total);
    let b = b.min(total);
    let lead_start = a.saturating_sub(2); // 1 line before (1-based)
    let trail_end = (b + 3).min(total + 1); // 3 lines after
    let mut out = String::new();
    for (idx, line) in lines
        .iter()
        .enumerate()
        .skip(lead_start)
        .take(trail_end.saturating_sub(lead_start))
    {
        let n = idx + 1;
        let mark = if n >= a && n <= b { "" } else { "[ctx] " };
        out.push_str(&format!("{:>6}\t{mark}{line}\n", n));
    }
    ToolOutput::ok(out)
}

/// §S selector suffix grammar.
#[derive(Debug, PartialEq, Eq)]
enum Selector {
    /// 1-based inclusive line range A-B.
    Range(usize, usize),
    /// Last N lines.
    LastN(usize),
    /// Whole file (within the byte cap).
    Raw,
    /// W5.2 structure summary (definition lines only).
    Sum,
}

/// Split `path:SEL` — the suffix must STRICTLY match the grammar
/// (`A-B` digits, `-N`, `raw`); anything else is part of the path.
fn split_selector(raw: &str) -> (&str, Option<Selector>) {
    if let Some((p, sel)) = raw.rsplit_once(':') {
        if p.is_empty() {
            return (raw, None);
        }
        if sel == "raw" {
            return (p, Some(Selector::Raw));
        }
        if sel == "sum" {
            return (p, Some(Selector::Sum));
        }
        if let Some(n) = sel.strip_prefix('-') {
            if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) {
                if let Ok(k) = n.parse::<usize>() {
                    if k > 0 {
                        return (p, Some(Selector::LastN(k)));
                    }
                }
            }
            return (raw, None);
        }
        if let Some((a, b)) = sel.split_once('-') {
            let ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
            if ok(a) && ok(b) {
                if let (Ok(x), Ok(y)) = (a.parse::<usize>(), b.parse::<usize>()) {
                    if x > 0 && y > 0 {
                        return (p, Some(Selector::Range(x, y)));
                    }
                }
            }
            return (raw, None);
        }
    }
    (raw, None)
}

pub fn write(_ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let (Some(path), Some(content)) = (args["path"].as_str(), args["content"].as_str()) else {
        return ToolOutput::err("missing `path` or `content`");
    };
    let full = resolve(&_ctx.cwd, path);
    if let Some(p) = full.parent() {
        if let Err(e) = std::fs::create_dir_all(p) {
            return ToolOutput::err(format!("mkdir failed: {e}"));
        }
    }
    match std::fs::write(&full, content) {
        Ok(_) => ToolOutput::ok(format!("wrote {path} ({} bytes)", content.len())),
        Err(e) => ToolOutput::err(format!("Write failed for {path}: {e}")),
    }
}

pub fn edit(_ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let (Some(raw_path), Some(old), Some(new)) = (
        args["path"].as_str(),
        args["old_string"].as_str(),
        args["new_string"].as_str(),
    ) else {
        return ToolOutput::err("missing `path`, `old_string` or `new_string`");
    };
    if old == new {
        return ToolOutput::err("old_string == new_string (no-op edit)");
    }
    let replace_all = args["replace_all"].as_bool().unwrap_or(false);
    let (path, _) = marfim_core::hashline::split_anchor(raw_path);
    let full = resolve(&_ctx.cwd, path);
    let content = match read_capped(&full, 4 * 1024 * 1024) {
        Ok(c) => c,
        Err(e) => return ToolOutput::err(format!("Edit failed for {raw_path}: {e}")),
    };
    // W5.1 hashline anchor: a stale tag refuses BEFORE the ladder runs —
    // the model cannot edit against a read that no longer matches disk.
    if let Err(e) = check_anchor(raw_path, &content) {
        return ToolOutput::err(e);
    }
    match edit_ladder::apply_edit(&content, old, new, replace_all) {
        Some((next, rung)) => match std::fs::write(&full, &next) {
            Ok(_) => ToolOutput::ok(format!("edited {raw_path} (ladder: {rung})")),
            Err(e) => ToolOutput::err(format!("write-back failed: {e}")),
        },
        None => ToolOutput::err(format!(
            "old_string not found in {raw_path} (ladder exhausted: exact, line-anchored, whitespace-flexible)"
        )),
    }
}

/// W5.1: when the addressed path carries `#tag`, the on-disk content
/// must still hash to that tag — otherwise the file changed since the
/// model's Read and the mutation refuses (re-read first).
fn check_anchor(raw_path: &str, disk_content: &str) -> Result<(), String> {
    let (path, tag) = marfim_core::hashline::split_anchor(raw_path);
    let Some(tag) = tag else {
        return Ok(());
    };
    let current = marfim_core::hashline::content_tag(disk_content);
    if current != tag {
        return Err(format!(
            "stale read: {path} changed since your Read (read tag {tag}, disk tag {current}) — re-read the file and edit against the current content"
        ));
    }
    Ok(())
}

pub fn multi_edit(_ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let Some(raw_path) = args["path"].as_str() else {
        return ToolOutput::err("missing `path`");
    };
    let Some(edits) = args["edits"].as_array() else {
        return ToolOutput::err("missing `edits` array");
    };
    let (path, _) = marfim_core::hashline::split_anchor(raw_path);
    let full = resolve(&_ctx.cwd, path);
    let mut content = match read_capped(&full, 4 * 1024 * 1024) {
        Ok(c) => c,
        Err(e) => return ToolOutput::err(format!("MultiEdit failed for {raw_path}: {e}")),
    };
    if let Err(e) = check_anchor(raw_path, &content) {
        return ToolOutput::err(e);
    }
    // all-or-nothing: simulate each edit first
    let mut staged = content.clone();
    let mut rungs = Vec::new();
    for (i, e) in edits.iter().enumerate() {
        let (Some(old), Some(new)) = (e["old_string"].as_str(), e["new_string"].as_str()) else {
            return ToolOutput::err(format!("edit #{i}: missing old/new_string"));
        };
        match edit_ladder::apply_edit(
            &staged,
            old,
            new,
            e["replace_all"].as_bool().unwrap_or(false),
        ) {
            Some((next, rung)) => {
                staged = next;
                rungs.push(rung);
            }
            None => {
                return ToolOutput::err(format!(
                    "edit #{i} old_string not found; no changes applied (all-or-nothing)"
                ))
            }
        }
    }
    content = staged;
    match std::fs::write(&full, &content) {
        Ok(_) => ToolOutput::ok(format!(
            "applied {} edits to {raw_path} (rungs: {})",
            edits.len(),
            rungs.join(", ")
        )),
        Err(e) => ToolOutput::err(format!("write-back failed: {e}")),
    }
}

pub fn glob(_ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let Some(pattern) = args["pattern"].as_str() else {
        return ToolOutput::err("missing `pattern`");
    };
    let limit = args["limit"].as_u64().unwrap_or(100) as usize;
    let hits = crate::globber::walk_glob(&_ctx.cwd, pattern, limit);
    if hits.is_empty() {
        return ToolOutput::ok(format!("no files match {pattern}"));
    }
    let rel: Vec<String> = hits
        .iter()
        .map(|p| {
            p.strip_prefix(&_ctx.cwd)
                .unwrap_or(p)
                .to_string_lossy()
                .to_string()
        })
        .collect();
    ToolOutput::ok(rel.join("\n"))
}

pub fn grep(_ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let Some(pattern) = args["pattern"].as_str() else {
        return ToolOutput::err("missing `pattern`");
    };
    let re = match regex::Regex::new(pattern) {
        Ok(r) => r,
        Err(e) => return ToolOutput::err(format!("bad regex: {e}")),
    };
    let include = args["include"].as_str().unwrap_or("**/*");
    let limit = args["head_limit"].as_u64().unwrap_or(200) as usize;
    let files = crate::globber::walk_glob(&_ctx.cwd, include, 10_000);
    let mut out = Vec::new();
    'files: for f in &files {
        // W5.7: chunk-fed matching — memory is bounded by the chunk, not
        // the file (a huge log greps in constant space).
        let hits = match crate::walkcache::grep_file_chunked(f, &|l: &str| re.is_match(l), limit) {
            Ok(h) => h,
            Err(_) => continue,
        };
        let rel = f
            .strip_prefix(&_ctx.cwd)
            .unwrap_or(f)
            .to_string_lossy()
            .to_string();
        for (n, line) in hits {
            out.push(format!("{rel}:{n}:{line}"));
            if out.len() >= limit {
                break 'files;
            }
        }
    }
    if out.is_empty() {
        ToolOutput::ok(format!("no matches for /{pattern}/"))
    } else {
        ToolOutput::ok(out.join("\n"))
    }
}

pub fn ls(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let dir = args["path"].as_str().unwrap_or(".");
    let full = resolve(&ctx.cwd, dir);
    // W5.7 stamp cache: hot re-lists of an unchanged directory ride the
    // listing cache (mtime stamp + racy window); a moved stamp re-lists.
    let cached = ctx.dircache.listing(&full);
    let names: Vec<std::path::PathBuf> = match cached {
        Some(c) => c,
        None => {
            let rd = match std::fs::read_dir(&full) {
                Ok(rd) => rd,
                Err(_) => return ToolOutput::err(format!("cannot read directory {dir}")),
            };
            let v: Vec<std::path::PathBuf> = rd.flatten().map(|e| e.path()).collect();
            ctx.dircache.store(&full, v.clone());
            v
        }
    };
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for p in names {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if p.is_dir() {
            dirs.push(format!("{name}/"));
        } else {
            files.push(name);
        }
    }
    dirs.sort();
    files.sort();
    let mut out = String::new();
    for d in dirs {
        out.push_str(&d);
        out.push('\n');
    }
    for f in files {
        out.push_str(&f);
        out.push('\n');
    }
    if out.is_empty() {
        out = "(empty)\n".into();
    }
    ToolOutput::ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ToolCtx;
    use std::sync::Arc;

    fn ctx_in(root: &Path) -> ToolCtx {
        ToolCtx {
            cwd: root.to_path_buf(),
            session_id: "t".into(),
            journal_path: Arc::new(std::sync::Mutex::new(None)),
            board: Arc::new(Default::default()),
            evidence: Arc::new(Default::default()),
            memory: Arc::new(Default::default()),
            todos: Arc::new(Default::default()),
            plan_mode: Arc::new(Default::default()),
            background: Arc::new(crate::bash::BackgroundShells::default()),
            monitors: Arc::new(crate::monitor::Monitors::default()),
            terminals: Arc::new(marfim_term::TermServer::default()),
            dircache: Arc::new(crate::walkcache::DirCache::default()),
            rea: Arc::new(Default::default()),
            last_repomap: Arc::new(Default::default()),
            subagent: Arc::new(crate::subagent::UnwiredSubagents),
            bash_default_timeout_ms: 120_000,
        }
    }

    fn setup() -> (tempdir::Temp, ToolCtx) {
        let t = tempdir::Temp::new("marfim-fs");
        std::fs::write(t.path().join("main.rs"), "fn main() {}\n").unwrap();
        std::fs::create_dir_all(t.path().join("src")).unwrap();
        std::fs::write(t.path().join("src/lib.rs"), "pub fn f() -> u8 { 1 }\n").unwrap();
        let c = ctx_in(t.path());
        (t, c)
    }

    // -- W5.1 hashline anchors ------------------------------------------------

    #[test]
    fn read_stamps_the_content_tag_header() {
        let (t, c) = setup();
        let out = read(&c, &serde_json::json!({"path": "main.rs"}));
        assert!(!out.is_error);
        let tag = marfim_core::hashline::content_tag("fn main() {}\n");
        assert!(
            out.output.starts_with(&format!("[main.rs#{tag}]")),
            "header anchors the read: {}",
            out.output
        );
        // every selector shape carries the anchor
        for sel in ["main.rs:raw", "main.rs:1-1", "main.rs:sum", "main.rs:-1"] {
            let o = read(&c, &serde_json::json!({"path": sel}));
            assert!(o.output.starts_with('[') && o.output.contains('#'), "{sel}");
        }
        drop(t);
    }

    #[test]
    fn edit_with_fresh_anchor_proceeds_and_stale_refuses() {
        let (t, c) = setup();
        let first = read(&c, &serde_json::json!({"path": "main.rs"}));
        let tag = first
            .output
            .lines()
            .next()
            .unwrap()
            .trim_matches(['[', ']'])
            .rsplit('#')
            .next()
            .unwrap()
            .to_string();
        // fresh anchor: edit succeeds
        let ok = edit(
            &c,
            &serde_json::json!({
                "path": format!("main.rs#{tag}"),
                "old_string": "fn main() {}",
                "new_string": "fn main() { println!(\"hi\"); }",
            }),
        );
        assert!(!ok.is_error, "{}", ok.output);
        // the file changed under the model: the OLD tag must now refuse
        let stale = edit(
            &c,
            &serde_json::json!({
                "path": format!("main.rs#{tag}"),
                "old_string": "fn main() { println!(\"hi\"); }",
                "new_string": "fn main() {}",
            }),
        );
        assert!(stale.is_error, "stale anchor must refuse");
        assert!(stale.output.contains("stale read"), "{}", stale.output);
        assert!(stale.output.contains("re-read"), "{}", stale.output);
        // anchor-less edit keeps legacy behavior
        let legacy = edit(
            &c,
            &serde_json::json!({
                "path": "main.rs",
                "old_string": "fn main() { println!(\"hi\"); }",
                "new_string": "fn main() {}",
            }),
        );
        assert!(!legacy.is_error, "{}", legacy.output);
        drop(t);
    }

    #[test]
    fn multi_edit_honors_the_anchor_too() {
        let (t, c) = setup();
        let first = read(&c, &serde_json::json!({"path": "main.rs"}));
        let tag = first
            .output
            .lines()
            .next()
            .unwrap()
            .trim_matches(['[', ']'])
            .rsplit('#')
            .next()
            .unwrap()
            .to_string();
        std::fs::write(t.path().join("main.rs"), "changed under you\n").unwrap();
        let stale = multi_edit(
            &c,
            &serde_json::json!({
                "path": format!("main.rs#{tag}"),
                "edits": [{"old_string": "changed", "new_string": "x"}],
            }),
        );
        assert!(stale.is_error && stale.output.contains("stale read"));
        drop(t);
    }

    // -- W5.2 selector-driven summarization ----------------------------------

    #[test]
    fn sum_selector_elides_bodies_and_keeps_definitions() {
        let (t, c) = setup();
        std::fs::write(
            t.path().join("big.py"),
            "import os\n\n\nclass Engine:\n    def start(self):\n        return 1\n\n\ndef main():\n    pass\n",
        )
        .unwrap();
        let out = read(&c, &serde_json::json!({"path": "big.py:sum"}));
        assert!(!out.is_error, "{}", out.output);
        assert!(out.output.contains("structure summary"));
        assert!(out.output.contains("class Engine"), "{}", out.output);
        assert!(out.output.contains("def start"), "{}", out.output);
        assert!(out.output.contains("def main"), "{}", out.output);
        assert!(
            !out.output.contains("return 1"),
            "bodies are elided: {}",
            out.output
        );
        assert!(
            out.output.contains("lines shown"),
            "elision count is honest: {}",
            out.output
        );
        drop(t);
    }

    mod tempdir {
        pub struct Temp(pub std::path::PathBuf);
        impl Temp {
            pub fn new(tag: &str) -> Self {
                // pid + monotonic counter: parallel test threads in ONE process
                // must never share a directory (the old pid-only scheme let a
                // sibling test's remove_dir_all delete another test's fixtures
                // mid-run — the CI MSRV flake class).
                let p = marfim_testkit::unique_dir(tag);
                let _ = std::fs::remove_dir_all(&p);
                std::fs::create_dir_all(&p).unwrap();
                Self(p)
            }
            pub fn path(&self) -> &std::path::Path {
                &self.0
            }
        }
        impl Drop for Temp {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn read_is_line_numbered() {
        let (_t, c) = setup();
        let o = read(&c, &serde_json::json!({"path": "main.rs"}));
        assert!(!o.is_error);
        assert!(o.output.contains("     1\tfn main() {}"), "{}", o.output);
    }

    #[test]
    fn write_creates_parents() {
        let (_t, c) = setup();
        let o = write(
            &c,
            &serde_json::json!({"path": "deep/nested/x.txt", "content": "hi"}),
        );
        assert!(!o.is_error, "{}", o.output);
        assert_eq!(
            std::fs::read_to_string(c.cwd.join("deep/nested/x.txt")).unwrap(),
            "hi"
        );
    }

    #[test]
    fn edit_rungs_in_order() {
        let (_t, c) = setup();
        // rung: line-anchored (wrong indentation from model)
        let o = edit(
            &c,
            &serde_json::json!({
                "path": "main.rs",
                "old_string": "  fn main() {}",
                "new_string": "  fn main() { /* ok */ }"
            }),
        );
        assert!(!o.is_error, "{}", o.output);
        assert!(o.output.contains("line-anchored"), "{}", o.output);
        let text = std::fs::read_to_string(c.cwd.join("main.rs")).unwrap();
        assert!(text.contains("fn main() { /* ok */ }"));
    }

    #[test]
    fn edit_missing_is_error_with_ladder_note() {
        let (_t, c) = setup();
        let o = edit(
            &c,
            &serde_json::json!({
                "path": "main.rs",
                "old_string": "zzz-not-here",
                "new_string": "q"
            }),
        );
        assert!(o.is_error);
        assert!(o.output.contains("ladder exhausted"));
    }

    #[test]
    fn multi_edit_all_or_nothing() {
        let (_t, c) = setup();
        let o = multi_edit(
            &c,
            &serde_json::json!({
                "path": "main.rs",
                "edits": [
                    {"old_string": "fn main() {}", "new_string": "fn main() -> () {}"},
                    {"old_string": "NOT PRESENT", "new_string": "x"}
                ]
            }),
        );
        assert!(o.is_error);
        assert!(o.output.contains("no changes applied"));
        // file unchanged
        let text = std::fs::read_to_string(c.cwd.join("main.rs")).unwrap();
        assert!(!text.contains("-> ()"));
    }

    #[test]
    fn glob_and_grep_and_ls() {
        let (_t, c) = setup();
        let g = glob(&c, &serde_json::json!({"pattern": "**/*.rs"}));
        assert!(g.output.contains("src/lib.rs"));
        let gr = grep(
            &c,
            &serde_json::json!({"pattern": "fn f", "include": "**/*.rs"}),
        );
        assert!(gr.output.contains("src/lib.rs:1"));
        let l = ls(&c, &serde_json::json!({}));
        assert!(l.output.contains("main.rs"));
        assert!(l.output.contains("src/"));
    }

    // --------------------------------------------------------- D24: Read
    #[test]
    fn read_offset_limit_and_explicit_truncation_marker() {
        let (_t, c) = setup();
        let body: String = (1..=3000)
            .map(|i| format!("line {i}\n"))
            .collect::<String>();
        std::fs::write(c.cwd.join("big.txt"), &body).unwrap();

        // default: 2000 lines + explicit marker (never a silent cut)
        let o = read(&c, &serde_json::json!({"path": "big.txt"}));
        assert!(!o.is_error);
        assert!(o.output.contains("line 1\n"));
        assert!(o.output.contains("line 2000\n"));
        assert!(
            o.output
                .contains("[truncated: showing lines 1-2000 of 3000"),
            "explicit [truncated] marker missing (D24): {}",
            o.output.lines().last().unwrap_or("")
        );
        assert!(!o.output.contains("line 2001\n"), "no silent overflow");

        // offset+limit window
        let o2 = read(
            &c,
            &serde_json::json!({"path": "big.txt", "offset": 2499, "limit": 2}),
        );
        assert!(o2.output.contains("line 2500\n"));
        assert!(o2.output.contains("line 2501\n"));
        assert!(!o2.output.contains("line 2502\n"));
        assert!(o2
            .output
            .contains("[truncated: showing lines 2500-2501 of 3000"));

        // offset past end is an explicit marker, not an empty result
        let o3 = read(&c, &serde_json::json!({"path": "big.txt", "offset": 9999}));
        assert!(o3.output.contains("[truncated: offset 9999 is past end"));

        // small file: no marker
        std::fs::write(c.cwd.join("small.txt"), "only line\n").unwrap();
        let o4 = read(&c, &serde_json::json!({"path": "small.txt"}));
        assert!(!o4.output.contains("[truncated"));
    }

    // --------------------------------------------- §S: selector grammar
    #[test]
    fn selector_range_with_context_lines() {
        let (_t, c) = setup();
        let body: String = (1..=20).map(|i| format!("line {i}\n")).collect::<String>();
        std::fs::write(c.cwd.join("r.txt"), &body).unwrap();
        let o = read(&c, &serde_json::json!({"path": "r.txt:5-7"}));
        assert!(!o.is_error, "{o:?}");
        assert!(o.output.contains("\tline 5\n"), "{}", o.output);
        assert!(o.output.contains("\tline 7\n"));
        assert!(!o.output.contains("\tline 9\n"), "range respected");
        // 1 lead + 3 trail context, marked
        assert!(o.output.contains("[ctx] line 4\n"), "{}", o.output);
        assert!(o.output.contains("[ctx] line 8\n"));
        assert!(o.output.contains("[ctx] line 10\n"));
        assert!(!o.output.contains("line 11\n"), "3-trail cap");
    }

    #[test]
    fn selector_last_n_and_raw() {
        let (_t, c) = setup();
        let body: String = (1..=30).map(|i| format!("row {i}\n")).collect::<String>();
        std::fs::write(c.cwd.join("t.txt"), &body).unwrap();
        let o = read(&c, &serde_json::json!({"path": "t.txt:-5"}));
        assert!(o.output.contains("[last 5 of 30 lines]"));
        assert!(o.output.contains("row 30"));
        assert!(!o.output.contains("row 25\n"), "only the last 5");
        let o2 = read(&c, &serde_json::json!({"path": "t.txt:raw"}));
        assert!(o2.output.contains("row 1\n") && o2.output.contains("row 30\n"));
    }

    #[test]
    fn selector_invalid_suffix_falls_back_to_path() {
        let (_t, c) = setup();
        std::fs::write(c.cwd.join("plain.txt"), "content\n").unwrap();
        // `a:b` is not a valid selector — the whole string stays a path
        let o = read(&c, &serde_json::json!({"path": "plain.txt:x-y"}));
        assert!(
            o.is_error,
            "non-matching suffix stays part of the path → missing file"
        );
        assert!(o.output.contains("plain.txt:x-y"));
        // explicit out-of-bounds range is an honest error, not a guess
        let o2 = read(&c, &serde_json::json!({"path": "plain.txt:5-9"}));
        assert!(o2.is_error);
        assert!(o2.output.contains("range 5-9 invalid"));
    }
}
