//! Plan mode tools + Monitor + NotebookEdit + binary inspection (rea) +
//! Task/TaskOutput (subagent spawns are driven by the host).

use crate::registry::{ToolCtx, ToolOutput};
use serde_json::Value;

pub fn enter_plan_mode(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let _ = args;
    *ctx.plan_mode.lock().unwrap_or_else(|p| p.into_inner()) = true;
    ToolOutput::ok(
        "plan mode ON: read-only until ExitPlanMode. Research the codebase, then present the plan.",
    )
}

pub fn exit_plan_mode(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let plan = args["plan"].as_str().unwrap_or("");
    if plan.trim().is_empty() {
        return ToolOutput::err("missing `plan` (the implementation plan text)");
    }
    *ctx.plan_mode.lock().unwrap_or_else(|p| p.into_inner()) = false;
    ToolOutput::ok("plan mode OFF; plan recorded:\n".to_string() + plan)
}

// ------------------------------------------------------------- NotebookEdit

pub fn notebook_edit(_ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let (Some(path), Some(cell_id), Some(new_source)) = (
        args["path"].as_str(),
        args["cell_id"].as_str(),
        args["new_source"].as_str(),
    ) else {
        return ToolOutput::err("missing `path`, `cell_id` or `new_source`");
    };
    let full = crate::fs::resolve(&_ctx.cwd, path);
    let Ok(text) = std::fs::read_to_string(&full) else {
        return ToolOutput::err(format!("cannot read notebook {path}"));
    };
    let mut nb: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => return ToolOutput::err(format!("not a valid ipynb: {e}")),
    };
    let Some(cells) = nb["cells"].as_array_mut() else {
        return ToolOutput::err("no `cells` array in notebook");
    };
    let mut found = false;
    for c in cells.iter_mut() {
        if c["id"] == Value::String(cell_id.into()) {
            c["source"] = Value::Array(
                new_source
                    .lines()
                    .map(|l| Value::String(format!("{l}\n")))
                    .collect(),
            );
            found = true;
            break;
        }
    }
    if !found {
        return ToolOutput::err(format!("cell `{cell_id}` not found"));
    }
    let serialized = match serde_json::to_string_pretty(&nb) {
        Ok(s) => s,
        Err(e) => return ToolOutput::err(format!("serialize notebook failed: {e}")),
    };
    match std::fs::write(&full, &serialized) {
        Ok(_) => ToolOutput::ok(format!("replaced cell {cell_id} in {path}")),
        Err(e) => ToolOutput::err(format!("write failed: {e}")),
    }
}

// ------------------------------------------------------------------ Monitor

/// W3.2 migrated Monitor: `path` watches a file (mtime, original
/// semantics); `terminal` watches a terminal session's output (fires when
/// the session produced bytes). One monitor watches exactly one source.
pub fn monitor(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    if let Some(path) = args["path"].as_str() {
        let full = crate::fs::resolve(&ctx.cwd, path);
        let id = ctx.monitors.start(full.to_string_lossy().as_ref());
        return ToolOutput::ok(format!(
            "monitor {id} watching {path} (poll its baseline mtime)"
        ));
    }
    if let Some(term_id) = args["terminal"].as_u64() {
        if ctx.terminals.get(term_id).is_none() {
            return ToolOutput::err(format!("no terminal {term_id}"));
        }
        let id = ctx
            .monitors
            .start_source(crate::monitor::MonitorSource::Terminal(term_id));
        return ToolOutput::ok(format!(
            "monitor {id} watching terminal {term_id} (fires on new output)"
        ));
    }
    ToolOutput::err("missing `path` (file watch) or `terminal` (terminal output watch)")
}

pub fn monitor_kill(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let Some(id) = args["monitor_id"].as_u64() else {
        return ToolOutput::err("missing `monitor_id`");
    };
    match ctx.monitors.kill(id) {
        true => ToolOutput::ok(format!("monitor {id} stopped")),
        false => ToolOutput::err(format!("no monitor {id}")),
    }
}

// --------------------------------------------------------------------- rea

pub fn bin_info(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let Some(path) = args["path"].as_str() else {
        return ToolOutput::err("missing `path`");
    };
    let full = crate::fs::resolve(&ctx.cwd, path);
    let Ok(bytes) = std::fs::read(&full) else {
        return ToolOutput::err(format!("cannot read {path}"));
    };
    let mut out = format!("{}: {} bytes\n", path, bytes.len());
    // magic sniff
    if bytes.starts_with(&[0x7f, b'E', b'L', b'F']) {
        out.push_str("format: ELF\n");
    } else if bytes.starts_with(b"PK") {
        out.push_str("format: zip/jar/docx\n");
    } else if bytes.starts_with(b"SQLite format 3") {
        out.push_str("format: sqlite3\n");
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        out.push_str("format: png\n");
    } else if bytes.starts_with(&[0xca, 0xfe, 0xba, 0xbe]) {
        out.push_str("format: java class\n");
    } else {
        out.push_str("format: unknown\n");
    }
    // entropy sample (coarse)
    let mut freq = [0u64; 256];
    for b in &bytes {
        freq[*b as usize] += 1;
    }
    let n = bytes.len().max(1) as f64;
    let entropy: f64 = freq
        .iter()
        .filter(|c| **c > 0)
        .map(|c| {
            let p = *c as f64 / n;
            -p * p.log2()
        })
        .sum();
    out.push_str(&format!("entropy: {entropy:.2} bits/byte\n"));
    ToolOutput::ok(out)
}

pub fn bin_strings(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let Some(path) = args["path"].as_str() else {
        return ToolOutput::err("missing `path`");
    };
    let min_len = args["min_length"].as_u64().unwrap_or(6) as usize;
    let full = crate::fs::resolve(&ctx.cwd, path);
    let Ok(bytes) = std::fs::read(&full) else {
        return ToolOutput::err(format!("cannot read {path}"));
    };
    let mut out = Vec::new();
    let mut cur = String::new();
    for b in bytes {
        if (32..127).contains(&b) {
            cur.push(b as char);
        } else {
            if cur.len() >= min_len {
                out.push(cur.clone());
            }
            cur.clear();
        }
    }
    if cur.len() >= min_len {
        out.push(cur);
    }
    if out.is_empty() {
        ToolOutput::ok("(no strings found)")
    } else {
        ToolOutput::ok(out.into_iter().take(500).collect::<Vec<_>>().join("\n"))
    }
}

pub fn bin_hexdump(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let Some(path) = args["path"].as_str() else {
        return ToolOutput::err("missing `path`");
    };
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let len = args["length"].as_u64().unwrap_or(256).min(4096) as usize;
    let full = crate::fs::resolve(&ctx.cwd, path);
    let Ok(bytes) = std::fs::read(&full) else {
        return ToolOutput::err(format!("cannot read {path}"));
    };
    let mut out = String::new();
    for (i, chunk) in bytes
        .iter()
        .skip(offset)
        .take(len)
        .collect::<Vec<_>>()
        .chunks(16)
        .enumerate()
    {
        let addr = offset + i * 16;
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let ascii: String = chunk
            .iter()
            .map(|b| {
                if (32..127).contains(&**b) {
                    **b as char
                } else {
                    '.'
                }
            })
            .collect();
        out.push_str(&format!("{addr:08x}  {:<47}  |{ascii}|\n", hex.join(" ")));
    }
    ToolOutput::ok(out)
}

pub fn bin_sqlite(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let Some(path) = args["path"].as_str() else {
        return ToolOutput::err("missing `path`");
    };
    let full = crate::fs::resolve(&ctx.cwd, path);
    let Ok(bytes) = std::fs::read(&full) else {
        return ToolOutput::err(format!("cannot read {path}"));
    };
    if !bytes.starts_with(b"SQLite format 3") {
        return ToolOutput::err(format!("{path} is not a sqlite3 database"));
    }
    // header: page size at offset 16 (u16 BE)
    let page_size = u16::from_be_bytes([bytes[16], bytes[17]]) as usize;
    let page_size = if page_size == 1 { 65_536 } else { page_size };
    // scan page type 0x0d (table leaf) cells for sqlite_master rows — a
    // minimal reader that reports page structure; full row decoding is
    // delegated to the sqlite3 binary when present.
    let n_pages = bytes.len() / page_size;
    let mut leaf_pages = 0usize;
    let mut index_pages = 0usize;
    for p in 0..n_pages {
        let off = if p == 0 { 100 } else { 0 };
        if off + 1 > page_size {
            break;
        }
        let start = p * page_size + off;
        if start < bytes.len() {
            match bytes[start] {
                0x0d => leaf_pages += 1,
                0x0a => index_pages += 1,
                _ => {}
            }
        }
    }
    let sqlite3 = std::process::Command::new("sqlite3")
        .arg(&full)
        .arg(".tables")
        .output();
    match sqlite3 {
        Ok(o) if o.status.success() => {
            let tables = String::from_utf8_lossy(&o.stdout);
            ToolOutput::ok(format!(
                "sqlite3 db: page_size={page_size} pages={n_pages} (table_leaf={leaf_pages} index_leaf={index_pages})\ntables:\n{tables}"
            ))
        }
        _ => ToolOutput::ok(format!(
            "sqlite3 db: page_size={page_size} pages={n_pages} (table_leaf={leaf_pages} index_leaf={index_pages}); sqlite3 binary not available for .tables"
        )),
    }
}

// external-binary tools (AstQuery / Eval / JsModuleGraph): honest gating

pub fn ast_query(_ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let _ = args;
    ToolOutput::err(
        "AstQuery requires tree-sitter grammars (not bundled in this build); use Grep + RepoMap",
    )
}

pub fn eval_js(_ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let Some(code) = args["code"].as_str() else {
        return ToolOutput::err("missing `code`");
    };
    let node = std::process::Command::new("node")
        .arg("-e")
        .arg(code)
        .current_dir(&_ctx.cwd)
        .output();
    match node {
        Ok(o) if o.status.success() => {
            ToolOutput::ok(String::from_utf8_lossy(&o.stdout).to_string())
        }
        Ok(o) => ToolOutput::err(format!(
            "node exited {}: {}",
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stderr)
        )),
        Err(_) => ToolOutput::err("node binary not available (Eval disabled)"),
    }
}

pub fn js_module_graph(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let entry = args["entry"].as_str().unwrap_or(".");
    let node = std::process::Command::new("node")
        .arg("-e")
        .arg(format!(
            r#"const path=require('path'),fs=require('fs');
const seen=new Set();const edges=[];
function walk(f){{if(seen.has(f))return;seen.add(f);
let src;try{{src=fs.readFileSync(f,'utf8')}}catch{{return}};
const re=/(?:from\s+|require\()\s*['"](\.[^'"]+)['"]/g;let m;
while((m=re.exec(src))){{const r=path.resolve(path.dirname(f),m[1]);
for(const c of [r,r+'.js',r+'.ts',r+'.mjs',r+'/index.js']){{if(fs.existsSync(c)&&fs.statSync(c).isFile()){{edges.push(`${{f}} -> ${{c}}`);walk(c);break}}}}}}
walk(path.resolve('{}'));console.log(edges.join('\n'))"#,
            crate::fs::resolve(&ctx.cwd, entry).display()
        ))
        .current_dir(&ctx.cwd)
        .output();
    match node {
        Ok(o) if o.status.success() => {
            ToolOutput::ok(String::from_utf8_lossy(&o.stdout).to_string())
        }
        Ok(o) => ToolOutput::err(String::from_utf8_lossy(&o.stderr).to_string()),
        Err(_) => ToolOutput::err("node binary not available (JsModuleGraph disabled)"),
    }
}

// ------------------------------------------------------------- Task family

pub fn task(_ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let agent = args["agent"].as_str().unwrap_or("general");
    let prompt = args["prompt"].as_str().unwrap_or("");
    let _ = (agent, prompt);
    ToolOutput::err("Task must run through the async host executor (spawns a subagent loop)")
}

pub fn task_output(_ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let _ = args;
    ToolOutput::err("TaskOutput must run through the async host executor")
}

// --------------------------------------------------------- Memory (tool)

pub fn memory_tool(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    match args["action"].as_str() {
        Some("remember") => {
            let Some(text) = args["text"].as_str() else {
                return ToolOutput::err("missing `text`");
            };
            let mut m = ctx.memory.lock().unwrap_or_else(|p| p.into_inner());
            let id = m.remember(text, args["safety"].as_bool().unwrap_or(false));
            ToolOutput::ok(format!("remembered as {id}"))
        }
        Some("recall") => ToolOutput::ok(
            ctx.memory
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .render(),
        ),
        Some("forget") => {
            let Some(id) = args["id"].as_str() else {
                return ToolOutput::err("missing `id`");
            };
            let confirm = args["confirm"].as_bool().unwrap_or(false);
            match ctx
                .memory
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .forget(id, confirm)
            {
                Ok(_) => ToolOutput::ok(format!("forgot {id}")),
                Err(e) => ToolOutput::err(format!("{e}")),
            }
        }
        other => ToolOutput::err(format!("bad action {other:?} (remember|recall|forget)")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ToolCtx;
    use std::sync::Arc;

    fn ctx() -> ToolCtx {
        ToolCtx {
            cwd: std::env::temp_dir(),
            session_id: "t".into(),
            journal_path: Arc::new(std::sync::Mutex::new(None)),
            board: Arc::new(Default::default()),
            evidence: Arc::new(Default::default()),
            memory: Arc::new(Default::default()),
            todos: Arc::new(Default::default()),
            plan_mode: Arc::new(Default::default()),
            background: Arc::new(Default::default()),
            monitors: Arc::new(Default::default()),
            terminals: Arc::new(marfim_term::TermServer::default()),
            last_repomap: Arc::new(Default::default()),
            subagent: Arc::new(crate::subagent::UnwiredSubagents),
            bash_default_timeout_ms: 120_000,
        }
    }

    #[test]
    fn plan_mode_toggles() {
        let c = ctx();
        assert!(!*c.plan_mode.lock().unwrap_or_else(|p| p.into_inner()));
        enter_plan_mode(&c, &serde_json::json!({}));
        assert!(*c.plan_mode.lock().unwrap_or_else(|p| p.into_inner()));
        let o = exit_plan_mode(&c, &serde_json::json!({"plan": "do x then y"}));
        assert!(!o.is_error);
        assert!(!*c.plan_mode.lock().unwrap_or_else(|p| p.into_inner()));
        let o2 = exit_plan_mode(&c, &serde_json::json!({}));
        assert!(o2.is_error);
    }

    #[test]
    fn notebook_edit_replaces_cell() {
        let dir = marfim_testkit::unique_dir("marfim-nb");
        let _ = std::fs::create_dir_all(&dir);
        let nb_path = dir.join("n.ipynb");
        std::fs::write(
            &nb_path,
            r#"{"cells":[{"id":"c1","cell_type":"code","source":["old = 1\n"]}],"metadata":{},"nbformat":4,"nbformat_minor":5}"#,
        )
        .unwrap();
        let mut c = ctx();
        c.cwd = dir.clone();
        let o = notebook_edit(
            &c,
            &serde_json::json!({
                "path": "n.ipynb", "cell_id": "c1", "new_source": "new = 2"
            }),
        );
        assert!(!o.is_error, "{}", o.output);
        let text = std::fs::read_to_string(&nb_path).unwrap();
        assert!(text.contains("new = 2"));
        let o2 = notebook_edit(
            &c,
            &serde_json::json!({
                "path": "n.ipynb", "cell_id": "zz", "new_source": "x"
            }),
        );
        assert!(o2.is_error);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bin_tools_work_on_real_bytes() {
        let dir = marfim_testkit::unique_dir("marfim-bin");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("blob.bin");
        let mut data = vec![0x7f, b'E', b'L', b'F'];
        data.extend(b"hello-marfim-string\0\0\0padding-padding-padding");
        std::fs::write(&p, &data).unwrap();
        let mut c = ctx();
        c.cwd = dir.clone();
        let info = bin_info(&c, &serde_json::json!({"path": "blob.bin"}));
        assert!(info.output.contains("ELF"), "{}", info.output);
        assert!(info.output.contains("entropy"));
        let strs = bin_strings(&c, &serde_json::json!({"path": "blob.bin"}));
        assert!(
            strs.output.contains("hello-marfim-string"),
            "{}",
            strs.output
        );
        let hex = bin_hexdump(&c, &serde_json::json!({"path": "blob.bin", "length": 16}));
        assert!(hex.output.contains("7f 45 4c 46"), "{}", hex.output);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sqlite_detects_non_db() {
        let c = ctx();
        let o = bin_sqlite(&c, &serde_json::json!({"path": "/bin/sh"}));
        // /bin/sh is not sqlite → error
        assert!(o.is_error || o.output.contains("not a sqlite3"));
    }

    #[test]
    fn memory_tool_actions() {
        let c = ctx();
        let o = memory_tool(
            &c,
            &serde_json::json!({"action": "remember", "text": "likes tabs", "safety": false}),
        );
        assert!(!o.is_error);
        let r = memory_tool(&c, &serde_json::json!({"action": "recall"}));
        assert!(r.output.contains("likes tabs"));
        let f = memory_tool(&c, &serde_json::json!({"action": "forget", "id": "m1"}));
        assert!(!f.is_error);
        let bad = memory_tool(&c, &serde_json::json!({"action": "nope"}));
        assert!(bad.is_error);
    }

    #[test]
    fn external_gated_tools_error_honestly() {
        let c = ctx();
        assert!(ast_query(&c, &serde_json::json!({})).is_error);
        assert!(task(&c, &serde_json::json!({})).is_error);
    }
}

// ------------------------------------------------- registry-backed tools

pub fn apply_patch_tool(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let Some(patch_text) = args["patch"].as_str() else {
        return ToolOutput::err("missing `patch`");
    };
    let patches = match crate::patch::parse_patch(patch_text) {
        Ok(p) => p,
        Err(e) => return ToolOutput::err(format!("patch parse error: {e}")),
    };
    let mut applied = Vec::new();
    for p in &patches {
        let target = match p.op {
            crate::patch::FileOp::Create => ctx.cwd.join(&p.new_path),
            _ => ctx.cwd.join(&p.old_path),
        };
        let content = std::fs::read_to_string(&target).unwrap_or_default();
        match crate::patch::apply_to_content(p, &content) {
            Ok(next) => {
                if p.op == crate::patch::FileOp::Delete {
                    if let Err(e) = std::fs::remove_file(&target) {
                        return ToolOutput::err(format!("delete failed: {e}"));
                    }
                } else {
                    if let Some(par) = target.parent() {
                        let _ = std::fs::create_dir_all(par);
                    }
                    if let Err(e) = std::fs::write(&target, &next) {
                        return ToolOutput::err(format!("write failed: {e}"));
                    }
                }
                applied.push(format!("{} ({:?})", target.display(), p.op));
            }
            Err(e) => return ToolOutput::err(format!("{}: {e}", target.display())),
        }
    }
    ToolOutput::ok(format!("applied patch to:\n{}", applied.join("\n")))
}

pub fn apply_diff_tool(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let (Some(path), Some(patch_text)) = (args["path"].as_str(), args["patch"].as_str()) else {
        return ToolOutput::err("missing `path` or `patch`");
    };
    let patches =
        match crate::patch::parse_patch(&format!("--- a/{path}\n+++ b/{path}\n{patch_text}")) {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(format!("patch parse error: {e}")),
        };
    let full = crate::fs::resolve(&ctx.cwd, path);
    let content = std::fs::read_to_string(&full).unwrap_or_default();
    match crate::patch::apply_to_content(&patches[0], &content) {
        Ok(next) => match std::fs::write(&full, &next) {
            Ok(_) => ToolOutput::ok(format!("applied diff to {path}")),
            Err(e) => ToolOutput::err(format!("write failed: {e}")),
        },
        Err(e) => ToolOutput::err(format!("{path}: {e}")),
    }
}

pub fn todo_write(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let Some(list) = args["todos"].as_array() else {
        return ToolOutput::err("missing `todos` array");
    };
    match crate::todo::validate(list) {
        Ok(todos) => {
            let rendered = crate::todo::render(&todos);
            *ctx.todos.lock().unwrap_or_else(|p| p.into_inner()) = todos;
            ToolOutput::ok(rendered)
        }
        Err(e) => ToolOutput::err(e),
    }
}

pub fn think(args: &Value) -> ToolOutput {
    let thought = args["thought"].as_str().unwrap_or("");
    if thought.trim().is_empty() {
        return ToolOutput::err("missing `thought`");
    }
    ToolOutput::ok("(noted; no side effects)".to_string())
}

pub fn repo_map(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let budget = args["budget"].as_u64().unwrap_or(200) as usize;
    let mut files: Vec<(std::path::PathBuf, String)> = Vec::new();
    let mut stack = vec![ctx.cwd.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "node_modules" || name == "target" {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else if crate::repomap::lang_of(&p).is_some() {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    files.push((p, text));
                }
            }
        }
    }
    if files.is_empty() {
        return ToolOutput::ok("(no source files found)");
    }
    let map = crate::repomap::build(&files, budget);
    *ctx.last_repomap.lock().unwrap_or_else(|p| p.into_inner()) = map.clone();
    ToolOutput::ok(map)
}

pub fn compute_tool(args: &Value) -> ToolOutput {
    let Some(expr) = args["expression"].as_str() else {
        return ToolOutput::err("missing `expression`");
    };
    match crate::compute::eval(expr) {
        Ok(v) => {
            if v == v.trunc() && v.abs() < 1e15 {
                ToolOutput::ok(format!("= {}", v as i128))
            } else {
                ToolOutput::ok(format!("= {v}"))
            }
        }
        Err(e) => ToolOutput::err(e),
    }
}

/// §S session_search (distilled: Hermes `hermes_state_fts.py`/`_search.py`
/// — lineage-aware session search over the journal corpus, adapted to
/// Marfim's JSONL journal). Bounded: scans the NEWEST [`marfim_core::
/// sessionsearch::SCAN_CAP`] text-bearing records; hits are ≤ 2k-char
/// snippets. Deterministic ranking (BM25-lite + phrase bonus).
pub fn session_search(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let Some(query) = args["query"]
        .as_str()
        .map(str::trim)
        .filter(|q| !q.is_empty())
    else {
        return ToolOutput::err("missing `query`");
    };
    let journal = {
        let p = ctx.journal_path.lock().unwrap_or_else(|p| p.into_inner());
        p.clone()
    };
    let Some(path) = journal else {
        return ToolOutput::err(
            "no session journal is attached to this tool context (session_search unavailable)",
        );
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => return ToolOutput::err(format!("journal unreadable: {e}")),
    };
    // newest SCAN_CAP lines: the journal is append-only, so the tail is
    // the most recent history (bounded window contract).
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .len()
        .saturating_sub(marfim_core::sessionsearch::SCAN_CAP);
    let window = &lines[start..];
    // map each JSONL record to (kind, text) via its tagged fields
    let records: Vec<(String, String)> = window
        .iter()
        .enumerate()
        .filter_map(|(i, line)| {
            let v: serde_json::Value = serde_json::from_str(line).ok()?;
            let kind = v["kind"].as_str().unwrap_or("other").to_string();
            let text = text_of_record(&v)?;
            Some((format!("[{}]", i), format!("{kind}: {text}")))
        })
        .collect();
    if records.is_empty() {
        return ToolOutput::ok("(no text-bearing records in the journal window)");
    }
    let q = marfim_core::sessionsearch::parse_query(query);
    let hits = marfim_core::sessionsearch::search(&records, &q);
    if hits.is_empty() {
        return ToolOutput::ok(format!(
            "(no records match {query:?} in the last {} journal records — try shorter keywords)",
            records.len()
        ));
    }
    let mut out = format!(
        "session search: {} hit(s) for {query:?} over the newest {} record(s)\n",
        hits.len(),
        records.len()
    );
    for h in &hits {
        out.push_str(&format!(
            "\n[record {} | {} | score {:.3}]\n{}\n",
            h.index, h.kind, h.score, h.snippet
        ));
    }
    ToolOutput::ok(out)
}

/// The text-bearing fields of a journal record (serde `kind`-tagged).
/// Records without text (turn markers, usage) are skipped.
fn text_of_record(v: &serde_json::Value) -> Option<String> {
    let kind = v["kind"].as_str()?;
    let text = match kind {
        "user_message" => v["text"].as_str()?.to_string(),
        "assistant_message" => v["text"].as_str()?.to_string(),
        "tool_result" => v["output"].as_str()?.to_string(),
        "compaction" => v["summary"].as_str()?.to_string(),
        "steer" => v["text"].as_str()?.to_string(),
        "actuation" => v["detail"].to_string(),
        _ => return None,
    };
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

pub fn tool_search(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let _ = ctx;
    let Some(query) = args["query"].as_str() else {
        return ToolOutput::err("missing `query`");
    };
    let limit = args["limit"].as_u64().unwrap_or(5) as usize;
    // The registry is reachable via the host; here we search the static
    // builtin catalog by name for honest discovery UX.
    let catalog = [
        ("BinInfo", "inspect a binary file: format magic, entropy"),
        ("BinStrings", "extract printable strings from a binary"),
        ("BinHexdump", "hex dump a byte range of a binary"),
        ("BinSqlite", "inspect a sqlite3 database"),
        ("AstQuery", "AST queries (tree-sitter, not bundled)"),
        ("Eval", "evaluate JS via the node binary"),
        ("JsModuleGraph", "import graph via the node binary"),
        ("ApplyPatch", "apply a unified diff patch"),
        ("ApplyDiff", "apply a single-file unified diff"),
        ("Monitor", "watch a file for changes"),
        ("MonitorKill", "stop a monitor"),
        ("NotebookEdit", "replace a jupyter cell by id"),
    ];
    let q = query.to_lowercase();
    let mut out = Vec::new();
    for (name, desc) in catalog {
        if name.to_lowercase().contains(&q) || desc.contains(&q) {
            out.push(format!("{name}: {desc}"));
        }
        if out.len() >= limit {
            break;
        }
    }
    if out.is_empty() {
        ToolOutput::ok(format!("no tools match `{query}`"))
    } else {
        ToolOutput::ok(out.join("\n"))
    }
}

#[cfg(test)]
mod tests_extra {
    use super::*;
    use crate::registry::ToolCtx;
    use std::sync::Arc;

    fn ctx() -> ToolCtx {
        ToolCtx {
            cwd: marfim_testkit::unique_dir("marfim-extra"),
            session_id: "t".into(),
            journal_path: Arc::new(std::sync::Mutex::new(None)),
            board: Arc::new(Default::default()),
            evidence: Arc::new(Default::default()),
            memory: Arc::new(Default::default()),
            todos: Arc::new(Default::default()),
            plan_mode: Arc::new(Default::default()),
            background: Arc::new(Default::default()),
            monitors: Arc::new(Default::default()),
            terminals: Arc::new(marfim_term::TermServer::default()),
            last_repomap: Arc::new(Default::default()),
            subagent: Arc::new(crate::subagent::UnwiredSubagents),
            bash_default_timeout_ms: 120_000,
        }
    }

    #[test]
    fn patch_tool_applies_to_file() {
        let c = ctx();
        std::fs::create_dir_all(&c.cwd).unwrap();
        std::fs::write(c.cwd.join("lib.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        let o = apply_patch_tool(
            &c,
            &serde_json::json!({
                "patch": "--- a/lib.rs\n+++ b/lib.rs\n@@ -1,2 +1,2 @@\n fn a() {}\n-fn b() {}\n+fn b(x: u8) {}\n"
            }),
        );
        assert!(!o.is_error, "{}", o.output);
        let text = std::fs::read_to_string(c.cwd.join("lib.rs")).unwrap();
        assert!(text.contains("fn b(x: u8) {}"));
    }

    #[test]
    fn think_requires_content() {
        assert!(think(&serde_json::json!({"thought": ""})).is_error);
        assert!(!think(&serde_json::json!({"thought": "hi"})).is_error);
    }

    #[test]
    fn compute_tool_formats() {
        assert_eq!(
            compute_tool(&serde_json::json!({"expression": "6 * 7"})).output,
            "= 42"
        );
        assert_eq!(
            compute_tool(&serde_json::json!({"expression": "1/2"})).output,
            "= 0.5"
        );
        assert!(compute_tool(&serde_json::json!({"expression": "bad)"})).is_error);
    }
}
