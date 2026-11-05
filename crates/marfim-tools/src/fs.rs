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
pub fn read(_ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let Some(path) = args["path"].as_str() else {
        return ToolOutput::err("missing `path`");
    };
    let full = resolve(&_ctx.cwd, path);
    match read_capped(&full, 512 * 1024) {
        Ok(text) => {
            let lines: Vec<&str> = text.lines().collect();
            let total = lines.len();
            let offset = args["offset"].as_u64().unwrap_or(0) as usize; // 0-based
            let limit = args["limit"].as_u64().map(|v| v as usize).unwrap_or(2000);
            if offset >= total && total > 0 {
                return ToolOutput::ok(format!(
                    "[truncated: offset {offset} is past end of file ({total} lines)]"
                ));
            }
            let start = offset.min(total);
            let end = (start.saturating_add(limit)).min(total);
            let mut out = String::new();
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
        Err(e) => ToolOutput::err(format!("Read failed for {path}: {e}")),
    }
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
    let (Some(path), Some(old), Some(new)) = (
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
    let full = resolve(&_ctx.cwd, path);
    let content = match read_capped(&full, 4 * 1024 * 1024) {
        Ok(c) => c,
        Err(e) => return ToolOutput::err(format!("Edit failed for {path}: {e}")),
    };
    match edit_ladder::apply_edit(&content, old, new, replace_all) {
        Some((next, rung)) => match std::fs::write(&full, &next) {
            Ok(_) => ToolOutput::ok(format!("edited {path} (ladder: {rung})")),
            Err(e) => ToolOutput::err(format!("write-back failed: {e}")),
        },
        None => ToolOutput::err(format!(
            "old_string not found in {path} (ladder exhausted: exact, line-anchored, whitespace-flexible)"
        )),
    }
}

pub fn multi_edit(_ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let Some(path) = args["path"].as_str() else {
        return ToolOutput::err("missing `path`");
    };
    let Some(edits) = args["edits"].as_array() else {
        return ToolOutput::err("missing `edits` array");
    };
    let full = resolve(&_ctx.cwd, path);
    let mut content = match read_capped(&full, 4 * 1024 * 1024) {
        Ok(c) => c,
        Err(e) => return ToolOutput::err(format!("MultiEdit failed for {path}: {e}")),
    };
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
            "applied {} edits to {path} (rungs: {})",
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
        let Ok(text) = std::fs::read_to_string(f) else {
            continue;
        };
        let rel = f
            .strip_prefix(&_ctx.cwd)
            .unwrap_or(f)
            .to_string_lossy()
            .to_string();
        for (i, line) in text.lines().enumerate() {
            if re.is_match(line) {
                out.push(format!("{rel}:{}:{line}", i + 1));
                if out.len() >= limit {
                    break 'files;
                }
            }
        }
    }
    if out.is_empty() {
        ToolOutput::ok(format!("no matches for /{pattern}/"))
    } else {
        ToolOutput::ok(out.join("\n"))
    }
}

pub fn ls(_ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let dir = args["path"].as_str().unwrap_or(".");
    let full = resolve(&_ctx.cwd, dir);
    let Ok(rd) = std::fs::read_dir(&full) else {
        return ToolOutput::err(format!("cannot read directory {dir}"));
    };
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if e.path().is_dir() {
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
            board: Arc::new(Default::default()),
            evidence: Arc::new(Default::default()),
            memory: Arc::new(Default::default()),
            todos: Arc::new(Default::default()),
            plan_mode: Arc::new(Default::default()),
            background: Arc::new(crate::bash::BackgroundShells::default()),
            monitors: Arc::new(crate::monitor::Monitors::default()),
            last_repomap: Arc::new(Default::default()),
            subagent: Arc::new(crate::subagent::UnwiredSubagents),
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

    mod tempdir {
        pub struct Temp(pub std::path::PathBuf);
        impl Temp {
            pub fn new(tag: &str) -> Self {
                let p = std::env::temp_dir().join(format!("{tag}-{}", std::process::id()));
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
}
