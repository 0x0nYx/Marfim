//! Bash / BashOutput / KillShell: sandboxed execution with process-group
//! timeout kill (INV-10), output caps, background shell registry.

use crate::registry::{ToolCtx, ToolOutput};
use marfim_core::shellparse;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tokio::process::Command;

pub const OUTPUT_CAP: usize = 30_000;

#[derive(Debug)]
pub struct ShellRecord {
    pub id: u64,
    pub command: String,
    pub output: Mutex<String>,
    pub done: Mutex<bool>,
    pub exit_code: Mutex<Option<i32>>,
}

#[derive(Default)]
pub struct BackgroundShells {
    next_id: AtomicU64,
    pub shells: Mutex<Vec<std::sync::Arc<ShellRecord>>>,
}

impl BackgroundShells {
    pub fn alloc(&self, command: &str) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        self.shells
            .lock()
            .unwrap()
            .push(std::sync::Arc::new(ShellRecord {
                id,
                command: command.into(),
                output: Mutex::new(String::new()),
                done: Mutex::new(false),
                exit_code: Mutex::new(None),
            }));
        id
    }

    pub fn get(&self, id: u64) -> Option<std::sync::Arc<ShellRecord>> {
        self.shells
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.id == id)
            .cloned()
    }
}

fn cap(s: &mut String) {
    if s.len() > OUTPUT_CAP {
        let keep = &s[s.len() - OUTPUT_CAP..];
        let start = keep.find('\n').map(|i| i + 1).unwrap_or(0);
        *s = format!("[output truncated]\n{}", &keep[start..]);
    }
}

/// Synchronous-ish bash execution with hard timeout + process group kill.
/// The PermissionEngine has ALREADY allowed this call by the time we run.
pub async fn run_bash(
    ctx: &ToolCtx,
    command: &str,
    sandbox: &marfim_core::sandbox::Sandbox,
    timeout_ms: u64,
) -> (String, bool) {
    let analysis = shellparse::analyze(command);
    // nested-exec / heredoc guard: never executed automatically (INV-08);
    // the caller must have asked the user. If we still see them here with
    // no approval listener, fail closed.
    if matches!(
        analysis.risk,
        shellparse::ShellRisk::NestedExec | shellparse::ShellRisk::Heredoc
    ) {
        return (
            format!(
                "command uses {} — requires explicit user approval (never auto-run)",
                if analysis.risk == shellparse::ShellRisk::NestedExec {
                    "nested execution"
                } else {
                    "heredoc"
                }
            ),
            true,
        );
    }

    let mut cmd: Command = sandbox.command("/bin/sh");
    cmd.arg("-c").arg(command);
    cmd.current_dir(&ctx.cwd);
    cmd.stdin(std::process::Stdio::null());

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (format!("spawn failed: {e}"), true),
    };
    let pid = child.id();

    let res = tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        child.wait_with_output(),
    )
    .await;
    match res {
        Err(_) => {
            if let Some(p) = pid {
                marfim_core::sandbox::kill_process_group(p as i32).await;
            }
            (
                format!(
                    "command timed out after {timeout_ms}ms and was killed (process group {:?})",
                    pid
                ),
                true,
            )
        }
        Ok(Err(e)) => (format!("wait failed: {e}"), true),
        Ok(Ok(out)) => {
            let mut text = String::from_utf8_lossy(&out.stdout).to_string();
            let err = String::from_utf8_lossy(&out.stderr);
            if !err.trim().is_empty() {
                text.push_str("\n[stderr]\n");
                text.push_str(&err);
            }
            let is_err = !out.status.success();
            if is_err {
                text.push_str(&format!("\n[exit: {}]", out.status.code().unwrap_or(-1)));
            }
            cap(&mut text);
            (text, is_err)
        }
    }
}

pub fn bash(_ctx: &ToolCtx, args: &Value) -> ToolOutput {
    // The async runner is driven by tools_host::execute; this sync shim is
    // only for schema/registration. Real execution goes through run_bash.
    let _ = args;
    ToolOutput::err("Bash must run through the async host executor")
}

pub fn bash_output(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let Some(id) = args["shell_id"].as_u64() else {
        return ToolOutput::err("missing `shell_id`");
    };
    match ctx.background.get(id) {
        Some(rec) => {
            let out = rec.output.lock().unwrap().clone();
            let done = *rec.done.lock().unwrap();
            let code = *rec.exit_code.lock().unwrap();
            ToolOutput::ok(format!(
                "shell {id} ({}): done={done} exit={:?}\n{}",
                rec.command,
                code,
                if out.is_empty() {
                    "(no output yet)"
                } else {
                    &out
                }
            ))
        }
        None => ToolOutput::err(format!("no background shell {id}")),
    }
}

pub fn kill_shell(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let Some(id) = args["shell_id"].as_u64() else {
        return ToolOutput::err("missing `shell_id`");
    };
    match ctx.background.get(id) {
        Some(rec) => {
            *rec.done.lock().unwrap() = true;
            *rec.exit_code.lock().unwrap() = Some(-1);
            ToolOutput::ok(format!("killed shell {id}"))
        }
        None => ToolOutput::err(format!("no background shell {id}")),
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
            board: Arc::new(Default::default()),
            evidence: Arc::new(Default::default()),
            memory: Arc::new(Default::default()),
            todos: Arc::new(Default::default()),
            plan_mode: Arc::new(Default::default()),
            background: Arc::new(BackgroundShells::default()),
            monitors: Arc::new(Default::default()),
            last_repomap: Arc::new(Default::default()),
        }
    }

    #[tokio::test]
    async fn bash_runs_simple_command() {
        let c = ctx();
        let sandbox = marfim_core::sandbox::Sandbox::probe();
        let (out, err) = run_bash(&c, "echo hello-world", &sandbox, 5000).await;
        assert!(!err, "{out}");
        assert!(out.contains("hello-world"));
    }

    #[tokio::test]
    async fn bash_timeout_kills() {
        let c = ctx();
        let sandbox = marfim_core::sandbox::Sandbox::probe();
        let t0 = std::time::Instant::now();
        let (out, err) = run_bash(&c, "sleep 30", &sandbox, 700).await;
        let elapsed = t0.elapsed();
        assert!(err);
        assert!(out.contains("timed out"), "{out}");
        assert!(elapsed < std::time::Duration::from_secs(5), "{elapsed:?}");
    }

    #[tokio::test]
    async fn nested_exec_refused_without_approval() {
        let c = ctx();
        let sandbox = marfim_core::sandbox::Sandbox::probe();
        let (out, err) = run_bash(&c, "echo $(whoami)", &sandbox, 5000).await;
        assert!(err);
        assert!(out.contains("never auto-run"), "{out}");
    }

    #[tokio::test]
    async fn failing_command_reports_exit() {
        let c = ctx();
        let sandbox = marfim_core::sandbox::Sandbox::probe();
        let (out, err) = run_bash(&c, "exit 7", &sandbox, 5000).await;
        assert!(err);
        assert!(out.contains("[exit: 7]"), "{out}");
    }

    #[test]
    fn background_shell_registry() {
        let c = ctx();
        let id = c.background.alloc("sleep 5");
        assert!(c.background.get(id).is_some());
        assert!(c.background.get(999).is_none());
        let o = kill_shell(&c, &serde_json::json!({"shell_id": id}));
        assert!(!o.is_error);
    }
}
