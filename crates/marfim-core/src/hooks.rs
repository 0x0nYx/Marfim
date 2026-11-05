//! Hooks (INV-09): PreToolUse runs before every tool; a hook failure or
//! non-zero exit blocks the tool (fail closed). PostToolUse fires after.
//! Hooks are configured as command strings receiving JSON on stdin and
//! answering `{"decision": "allow"|"deny", "reason": "..."}`.

use serde::Deserialize;
use std::sync::Arc;
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct HookSpec {
    pub event: HookEvent,
    pub command: String,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
}

#[derive(Debug, Deserialize)]
struct HookAnswer {
    #[serde(default)]
    decision: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Hooks {
    specs: Vec<HookSpec>,
}

impl Hooks {
    pub fn new(specs: Vec<HookSpec>) -> Self {
        Self { specs }
    }

    pub fn specs_for(&self, event: HookEvent) -> Vec<&HookSpec> {
        self.specs.iter().filter(|s| s.event == event).collect()
    }

    /// Run all PreToolUse hooks for `tool`. Err = blocked (fail closed).
    pub async fn pre_tool_use(&self, tool: &str, args_json: &str) -> Result<(), String> {
        let payload = serde_json::json!({
            "event": "PreToolUse",
            "tool": tool,
            "args": serde_json::from_str::<serde_json::Value>(args_json)
                .unwrap_or(serde_json::Value::String(args_json.to_string()))
        });
        for spec in self.specs_for(HookEvent::PreToolUse) {
            match run_hook(spec, &payload).await {
                Ok(answer) => {
                    if answer
                        .decision
                        .as_deref()
                        .map(|d| d.eq_ignore_ascii_case("deny"))
                        .unwrap_or(false)
                    {
                        return Err(format!(
                            "PreToolUse hook denied `{tool}`: {}",
                            answer.reason.unwrap_or_else(|| "no reason".into())
                        ));
                    }
                }
                Err(e) => {
                    // fail closed: hook error blocks the tool
                    return Err(format!("PreToolUse hook failed for `{tool}`: {e}"));
                }
            }
        }
        Ok(())
    }

    /// PostToolUse: best-effort; errors are surfaced but do not undo.
    pub async fn post_tool_use(&self, tool: &str, output: &str) {
        let payload = serde_json::json!({
            "event": "PostToolUse",
            "tool": tool,
            "output": output
        });
        for spec in self.specs_for(HookEvent::PostToolUse) {
            let _ = run_hook(spec, &payload).await;
        }
    }
}

async fn run_hook(spec: &HookSpec, payload: &serde_json::Value) -> Result<HookAnswer, String> {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(&spec.command);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.env_clear();
    for (k, v) in crate::permissions::sanitize_env(std::env::vars().collect::<Vec<_>>()) {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().map_err(|e| format!("spawn hook: {e}"))?;
    use tokio::io::AsyncWriteExt;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.to_string().as_bytes()).await;
        drop(stdin);
    }
    let out = tokio::time::timeout(
        std::time::Duration::from_millis(spec.timeout_ms),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| format!("hook timed out after {}ms", spec.timeout_ms))?
    .map_err(|e| format!("hook wait: {e}"))?;

    if !out.status.success() {
        let code = out.status.code().unwrap_or(-1);
        return Err(format!("hook exited with code {code}"));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    if text.trim().is_empty() {
        return Ok(HookAnswer {
            decision: Some("allow".into()),
            reason: None,
        });
    }
    serde_json::from_str(&text)
        .map_err(|e| format!("hook output not JSON: {e} (raw: {:.80})", text))
}

pub type SharedHooks = Arc<Hooks>;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pre_tool_use_fails_closed_on_hook_error() {
        let hooks = Hooks::new(vec![HookSpec {
            event: HookEvent::PreToolUse,
            command: "exit 3".into(),
            timeout_ms: 1000,
        }]);
        let err = hooks.pre_tool_use("Bash", "{}").await.unwrap_err();
        assert!(err.contains("hook exited with code 3"), "{err}");
    }

    #[tokio::test]
    async fn deny_decision_blocks_tool() {
        let hooks = Hooks::new(vec![HookSpec {
            event: HookEvent::PreToolUse,
            command: r#"echo '{"decision": "deny", "reason": "no writes on friday"}'"#.into(),
            timeout_ms: 1000,
        }]);
        let err = hooks.pre_tool_use("Write", "{}").await.unwrap_err();
        assert!(err.contains("no writes on friday"), "{err}");
    }

    #[tokio::test]
    async fn timeout_blocks_tool() {
        let hooks = Hooks::new(vec![HookSpec {
            event: HookEvent::PreToolUse,
            command: "sleep 5".into(),
            timeout_ms: 150,
        }]);
        assert!(hooks.pre_tool_use("Bash", "{}").await.is_err());
    }

    #[tokio::test]
    async fn allow_passes() {
        let hooks = Hooks::new(vec![HookSpec {
            event: HookEvent::PreToolUse,
            command: r#"echo '{"decision": "allow"}'"#.into(),
            timeout_ms: 1000,
        }]);
        assert!(hooks.pre_tool_use("Read", "{}").await.is_ok());
    }
}
