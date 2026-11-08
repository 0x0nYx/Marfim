//! W3.2 Terminal tool: interactive PTY sessions for the model.
//!
//! One tool, six actions (`spawn`/`write`/`drain`/`screen`/`resize`/
//! `close`/`list`). A session is a REAL pty (the child's stdio is a tty,
//! so REPLs/curses behave) with a vt100 screen model — see marfim-term.
//!
//! Doctrine:
//! - **spawn is Execute-class** and flows through the permission engine
//!   like Bash; the child argv goes through `Sandbox::wrap_argv` (the
//!   same wrapper shape as the Bash sandbox — fail-closed when an
//!   enforceable sandbox is requested but unavailable).
//! - **No shell parsing ever happens here**: `write` sends raw bytes to
//!   the child's stdin; `spawn` execs argv directly.
//! - **Sessions outlive a turn** (that is the point), but they are
//!   BOUNDED: the server caps sessions (16), the pending ring caps bytes
//!   (64 KiB, drop-oldest with an explicit marker), and children are
//!   killed on close. Children of a session are NOT children of the turn
//!   — subagents cannot own terminals (whole-tool deny on the child
//!   engine; a long-lived process surface is a parent's responsibility).
//! - The gateway's `/v1/terminals` routes dispatch through THIS tool via
//!   the real tool host, so the HTTP surface cannot bypass permissions.

use crate::registry::{ToolCtx, ToolOutput};
use marfim_core::sandbox::Sandbox;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

use marfim_term::TermSpec;

/// Wait helper: poll a predicate with a deadline (sync tools run on the
/// blocking pool — a short sleep loop is fine and dependency-free).
fn wait_for<F: Fn() -> bool>(deadline_ms: u64, f: F) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_millis(deadline_ms) {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    f()
}

/// The registry entry point. Non-spawn actions run here directly
/// (spawn_blocking); `spawn` requires the host arm (sandbox argv wrap)
/// and says so honestly if reached directly.
pub fn terminal(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    match args["action"].as_str().unwrap_or("") {
        "write" => write(ctx, args),
        "drain" => drain(ctx, args),
        "screen" => screen(ctx, args),
        "resize" => resize(ctx, args),
        "close" => close(ctx, args),
        "list" => list(ctx),
        "spawn" => ToolOutput::err(
            "Terminal spawn must be dispatched through the tool host (sandbox argv wrap)",
        ),
        other => ToolOutput::err(format!(
            "unknown action `{other}` (expected spawn|write|drain|screen|resize|close|list)"
        )),
    }
}

/// The host arm: full dispatch INCLUDING spawn (sandbox-wrapped argv).
/// Sync on purpose: spawn/drain/screen are short bounded ops; the async
/// executor only needs the Bash-style arm to pick this up.
pub fn terminal_exec(ctx: &ToolCtx, sandbox: &Sandbox, args: &Value) -> ToolOutput {
    match args["action"].as_str().unwrap_or("") {
        "spawn" => spawn(ctx, sandbox, args),
        _ => terminal(ctx, args),
    }
}

fn spawn(ctx: &ToolCtx, sandbox: &Sandbox, args: &Value) -> ToolOutput {
    let Some(program) = args["command"].as_str() else {
        return ToolOutput::err("missing `command`");
    };
    let term_args: Vec<String> = args["args"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    // Sandbox wrapper FIRST (fail-closed), so a refusal never leaves a
    // half-built spec behind.
    let (prog, targs) = match sandbox.wrap_argv(program, &term_args) {
        Ok(w) => w,
        Err(e) => return ToolOutput::err(e),
    };
    let mut spec = TermSpec::new(prog.clone());
    spec.args = targs;
    if let Some(dir) = args["cwd"].as_str() {
        spec.cwd = Some(ctx.cwd.join(dir));
    }
    if let Some(rows) = args["rows"].as_u64() {
        spec.rows = rows.min(u16::MAX as u64) as u16;
    }
    if let Some(cols) = args["cols"].as_u64() {
        spec.cols = cols.min(u16::MAX as u64) as u16;
    }
    if let Some(Value::Object(env)) = args.get("env") {
        for (k, v) in env {
            if let Some(val) = v.as_str() {
                spec.env.insert(k.clone(), val.to_string());
            }
        }
    }
    match ctx.terminals.spawn(&spec) {
        Ok(id) => {
            // Give short-lived children a beat to produce first output so
            // `spawn` alone is useful (e.g. `sh -c 'echo hi'`).
            let s = ctx.terminals.get(id);
            if let Some(s) = s {
                let _ = wait_for(150, || s.total_bytes() > 0 || !s.alive());
            }
            let mut out = format!("terminal {id} spawned ({prog}");
            for a in &term_args {
                out.push(' ');
                out.push_str(a);
            }
            out.push(')');
            ToolOutput::ok(out)
        }
        Err(e) => ToolOutput::err(e),
    }
}

fn resolve(ctx: &ToolCtx, args: &Value) -> Result<Arc<marfim_term::TermSession>, String> {
    let id = args["id"].as_u64().ok_or("missing `id`")?;
    ctx.terminals
        .get(id)
        .ok_or_else(|| format!("no terminal {id}"))
}

fn write(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let text = match args["text"].as_str() {
        Some(t) => t,
        None => return ToolOutput::err("missing `text`"),
    };
    let s = match resolve(ctx, args) {
        Ok(s) => s,
        Err(e) => return ToolOutput::err(e),
    };
    match s.write(text.as_bytes()) {
        Ok(()) => ToolOutput::ok(format!(
            "written {} bytes to terminal {}",
            text.len(),
            s.id()
        )),
        Err(e) => ToolOutput::err(e),
    }
}

/// Incremental output since the last drain (the streaming read).
fn drain(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let s = match resolve(ctx, args) {
        Ok(s) => s,
        Err(e) => return ToolOutput::err(e),
    };
    let text = s.drain();
    if text.is_empty() {
        ToolOutput::ok(format!(
            "terminal {}: no new output (alive: {})",
            s.id(),
            s.alive()
        ))
    } else {
        let mut out = format!("terminal {} (alive: {}):\n", s.id(), s.alive());
        out.push_str(&text);
        ToolOutput::ok(out)
    }
}

/// The visible screen right now (what a human looking at the pane sees).
fn screen(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let s = match resolve(ctx, args) {
        Ok(s) => s,
        Err(e) => return ToolOutput::err(e),
    };
    let _ = wait_for(300, || s.pending_len() > 0 || !s.alive());
    let _ = s.drain(); // screen is authoritative; don't double-report
    let (row, col) = s.cursor();
    let mut out = format!(
        "terminal {} (alive: {}, exit: {:?}, cursor: {row}:{col}):\n",
        s.id(),
        s.alive(),
        s.exit_code()
    );
    out.push_str(&s.screen());
    ToolOutput::ok(out)
}

fn resize(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let (Some(rows), Some(cols)) = (
        args["rows"].as_u64().map(|v| v as u16),
        args["cols"].as_u64().map(|v| v as u16),
    ) else {
        return ToolOutput::err("missing `rows`/`cols`");
    };
    let s = match resolve(ctx, args) {
        Ok(s) => s,
        Err(e) => return ToolOutput::err(e),
    };
    match s.resize(rows, cols) {
        Ok(()) => ToolOutput::ok(format!("terminal {} resized to {rows}x{cols}", s.id())),
        Err(e) => ToolOutput::err(e),
    }
}

fn close(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let id = match args["id"].as_u64() {
        Some(id) => id,
        None => return ToolOutput::err("missing `id`"),
    };
    if ctx.terminals.close(id) {
        ToolOutput::ok(format!("terminal {id} closed (child killed)"))
    } else {
        ToolOutput::err(format!("no terminal {id}"))
    }
}

fn list(ctx: &ToolCtx) -> ToolOutput {
    let infos = ctx.terminals.list();
    if infos.is_empty() {
        return ToolOutput::ok("no terminal sessions");
    }
    let mut out = String::new();
    for i in infos {
        out.push_str(&format!(
            "terminal {} — {} | alive: {} | exit: {:?} | bytes: {} | pending: {} | age: {}s\n",
            i.id, i.command, i.alive, i.exit_code, i.total_bytes, i.pending_bytes, i.age_secs
        ));
    }
    ToolOutput::ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ToolCtx;

    fn ctx() -> ToolCtx {
        ToolCtx::default()
    }

    fn spawn_plain(ctx: &ToolCtx, script: &str) -> ToolOutput {
        let sandbox = Sandbox::default().with_level(marfim_core::sandbox::SandboxLevel::None);
        terminal_exec(
            ctx,
            &sandbox,
            &serde_json::json!({"action": "spawn", "command": "sh", "args": ["-c", script]}),
        )
    }

    #[test]
    fn spawn_drain_screen_close_roundtrip() {
        let ctx = ctx();
        let out = spawn_plain(&ctx, "echo terminal-hello; sleep 5");
        assert!(!out.is_error, "{}", out.output);
        let id: u64 = out
            .output
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .expect("spawn reports the id");
        let got = wait_for(2_000, || {
            let s = ctx.terminals.get(id).unwrap();
            s.screen().contains("terminal-hello")
        });
        assert!(got, "screen must hold the echoed line");
        let scr = screen(&ctx, &serde_json::json!({"action": "screen", "id": id}));
        assert!(scr.output.contains("terminal-hello"), "{}", scr.output);
        // close kills the sleeping child
        let closed = close(&ctx, &serde_json::json!({"action": "close", "id": id}));
        assert!(!closed.is_error, "{}", closed.output);
        let s = ctx.terminals.get(id);
        assert!(s.is_none(), "closed sessions leave the registry");
    }

    #[test]
    fn write_reaches_the_child_stdin() {
        let ctx = ctx();
        let out = spawn_plain(&ctx, "cat");
        let id: u64 = out
            .output
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap();
        let w = write(
            &ctx,
            &serde_json::json!({"action": "write", "id": id, "text": "ping-through-pty\n"}),
        );
        assert!(!w.is_error, "{}", w.output);
        let got = wait_for(2_000, || {
            ctx.terminals
                .get(id)
                .map(|s| s.screen().contains("ping-through-pty"))
                .unwrap_or(false)
        });
        assert!(got, "echoed input must reach the screen");
        ctx.terminals.close(id);
    }

    #[test]
    fn unknown_ids_and_actions_fail_honestly() {
        let ctx = ctx();
        assert!(screen(&ctx, &serde_json::json!({"action": "screen", "id": 42})).is_error);
        assert!(
            write(
                &ctx,
                &serde_json::json!({"action": "write", "id": 42, "text": "x"})
            )
            .is_error
        );
        assert!(close(&ctx, &serde_json::json!({"action": "close", "id": 42})).is_error);
        assert!(
            terminal(&ctx, &serde_json::json!({"action": "explode"})).is_error,
            "unknown action"
        );
        assert!(
            terminal(&ctx, &serde_json::json!({"action": "spawn"})).is_error,
            "direct spawn without the host arm refuses honestly"
        );
    }

    #[test]
    fn list_and_cap_are_honest() {
        let ctx = ToolCtx::default();
        let empty = list(&ctx);
        assert!(empty.output.contains("no terminal sessions"));
        let a = spawn_plain(&ctx, "sleep 5");
        let b = spawn_plain(&ctx, "sleep 5");
        assert!(!a.is_error && !b.is_error);
        let listed = list(&ctx);
        assert!(listed.output.contains("sleep 5"), "{}", listed.output);
        // fill to the cap
        let sandbox = Sandbox::default().with_level(marfim_core::sandbox::SandboxLevel::None);
        let mut ids = vec![];
        for _ in 0..32 {
            let o = terminal_exec(
                &ctx,
                &sandbox,
                &serde_json::json!({"action": "spawn", "command": "sh", "args": ["-c", "sleep 5"]}),
            );
            if o.is_error {
                assert!(
                    o.output.contains("cap reached"),
                    "cap error must name itself: {}",
                    o.output
                );
                break;
            }
            let id: u64 = o
                .output
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse().ok())
                .unwrap();
            ids.push(id);
        }
        for id in ids {
            ctx.terminals.close(id);
        }
    }

    #[test]
    fn sandbox_wraps_and_refuses_fail_closed() {
        // None level: plain argv.
        let plain = Sandbox::default().with_level(marfim_core::sandbox::SandboxLevel::None);
        let (p, a) = plain
            .wrap_argv("sh", &["-c".to_string(), "true".to_string()])
            .unwrap();
        assert_eq!((p.as_str(), a.len()), ("sh", 2));
        // Enforced + writable roots + no helper → REFUSAL (never plain).
        let mut rooted =
            Sandbox::default().with_level(marfim_core::sandbox::SandboxLevel::Enforced);
        rooted.writable_roots = vec!["/tmp/marfim-wrap-test".to_string()];
        let rooted = rooted.without_exec_helper();
        let err = rooted.wrap_argv("sh", &[]).unwrap_err();
        assert!(err.contains("refusing"), "{err}");
        // Enforced + helper → wrapper argv carries sandbox-exec. Only
        // assertable where the platform can actually enforce (the
        // availability probe runs first and refuses honestly otherwise —
        // that refusal is the next branch's test).
        let mut helper =
            Sandbox::default().with_level(marfim_core::sandbox::SandboxLevel::Enforced);
        helper.writable_roots = vec!["/tmp/marfim-wrap-test".to_string()];
        let helper = helper.with_exec_helper(std::path::PathBuf::from("/fake/marfim"));
        if helper.fs_sandbox_available() {
            let (p, a) = helper
                .wrap_argv("sh", &["-c".into(), "true".into()])
                .unwrap();
            assert_eq!(p, "/fake/marfim");
            assert_eq!(a[0], "sandbox-exec");
            assert!(a.windows(2).any(|w| w == ["--", "sh"]));
        } else {
            let err = helper.wrap_argv("sh", &[]).unwrap_err();
            assert!(err.contains("cannot enforce"), "{err}");
        }
    }

    #[test]
    fn cwd_env_and_geometry_reach_the_spec() {
        let ctx = ctx();
        let sandbox = Sandbox::default().with_level(marfim_core::sandbox::SandboxLevel::None);
        let dir = marfim_testkit::Temp::new("marfim-term-tool").unwrap();
        let o = terminal_exec(
            &ctx,
            &sandbox,
            &serde_json::json!({
                "action": "spawn",
                "command": "sh",
                "args": ["-c", "pwd; echo G=$MARFIM_TOOL_G"],
                "cwd": dir.path().to_str().unwrap(),
                "rows": 10,
                "cols": 40,
                "env": {"MARFIM_TOOL_G": "yes"},
            }),
        );
        assert!(!o.is_error, "{}", o.output);
        let id: u64 = o
            .output
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap();
        let s = ctx.terminals.get(id).unwrap();
        assert!(
            wait_for(2_000, || s.screen().contains("G=yes")),
            "cwd+env apply: {:?}",
            s.screen()
        );
        assert!(
            s.screen().contains(dir.path().to_str().unwrap()),
            "pwd is the joined cwd"
        );
        ctx.terminals.close(id);
    }

    #[test]
    fn env_map_ignores_non_string_values() {
        let ctx = ctx();
        let sandbox = Sandbox::default().with_level(marfim_core::sandbox::SandboxLevel::None);
        let o = terminal_exec(
            &ctx,
            &sandbox,
            &serde_json::json!({
                "action": "spawn",
                "command": "sh",
                "args": ["-c", "echo ok; exit 3"],
                "env": {"GOOD": "1", "BAD": 42},
            }),
        );
        assert!(!o.is_error, "{}", o.output);
        let id: u64 = o
            .output
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap();
        let s = ctx.terminals.get(id).unwrap();
        assert!(wait_for(2_000, || !s.alive()));
        assert_eq!(s.exit_code(), Some(3), "exit code is honest");
        ctx.terminals.close(id);
    }
}
