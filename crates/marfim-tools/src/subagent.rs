//! D16: real subagent spawning. Task/TaskOutput run REAL nested agent
//! loops with role-restricted permissions and (when the project is a git
//! repo) worktree isolation. The spawner is a trait so tests can drive it
//! with mock providers; the binary wires the production host.

use serde_json::Value;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum TaskStatus {
    Running,
    Done { output: String },
    Failed { error: String },
}

/// Spawner contract consumed by the Task/TaskOutput tools.
pub trait SubagentSpawner: Send + Sync {
    fn spawn(&self, agent: &str, prompt: &str) -> Result<u64, String>;
    fn poll(&self, task_id: u64) -> Result<TaskStatus, String>;
}

/// Default wiring when no host is present: honest error, never success.
pub struct UnwiredSubagents;

impl SubagentSpawner for UnwiredSubagents {
    fn spawn(&self, _agent: &str, _prompt: &str) -> Result<u64, String> {
        Err("no subagent host wired (run `marfim` — the CLI installs the real spawner)".into())
    }
    fn poll(&self, _task_id: u64) -> Result<TaskStatus, String> {
        Err("no subagent host wired".into())
    }
}

/// Role-restricted permission profiles per agent persona (D16).
pub fn agent_permissions(agent: &str) -> (marfim_core::permissions::Mode, String) {
    use marfim_core::permissions::Mode;
    match agent {
        // read-only analysts: plan mode (read tools allow, rest denies)
        "explore" | "plan" | "reviewer" | "security" | "evaluator" => {
            (Mode::Plan, "read-only analyst".into())
        }
        // builders: default mode (edits allow, execution asks)
        "general" | "build-resolver" | "silent-failure" => {
            (Mode::Default, "builder".into())
        }
        _ => (Mode::Default, "builder (unknown persona falls back)".into()),
    }
}

/// Git-worktree isolation (D16): when the project is a git repo, subagents
/// run in a detached worktree so mutations cannot touch the main checkout.
pub struct WorktreeGuard {
    pub path: std::path::PathBuf,
    repo: Option<std::path::PathBuf>,
}

impl WorktreeGuard {
    /// Returns a guarded dir: a fresh worktree for git repos, or the same
    /// dir for non-git projects (isolation not applicable there).
    pub fn create(cwd: &std::path::Path) -> Result<Self, String> {
        let is_repo = std::process::Command::new("git")
            .args(["rev-parse", "--git-dir"])
            .current_dir(cwd)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !is_repo {
            return Ok(Self {
                path: cwd.to_path_buf(),
                repo: None,
            });
        }
        let base = std::env::temp_dir().join(format!(
            "marfim-worktree-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let out = std::process::Command::new("git")
            .args(["worktree", "add", "--detach"])
            .arg(&base)
            .current_dir(cwd)
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!(
                "worktree add failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(Self {
            path: base,
            repo: Some(cwd.to_path_buf()),
        })
    }
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        if let Some(repo) = &self.repo {
            let _ = std::process::Command::new("git")
                .args(["worktree", "remove", "--force"])
                .arg(&self.path)
                .current_dir(repo)
                .output();
        }
    }
}

/// Shared table of spawned tasks (TaskOutput polls this).
#[derive(Default)]
pub struct TaskTable {
    pub inner: std::sync::Mutex<std::collections::BTreeMap<u64, Arc<std::sync::Mutex<TaskSlot>>>>,
    next_id: std::sync::atomic::AtomicU64,
}

#[derive(Default)]
pub struct TaskSlot {
    pub status: Option<TaskStatus>,
    pub agent: String,
    pub worktree: Option<std::path::PathBuf>,
}

impl TaskTable {
    pub fn alloc(&self, agent: &str) -> (u64, Arc<std::sync::Mutex<TaskSlot>>) {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        let slot = Arc::new(std::sync::Mutex::new(TaskSlot {
            status: None,
            agent: agent.to_string(),
            worktree: None,
        }));
        self.inner.lock().unwrap().insert(id, slot.clone());
        (id, slot)
    }
}

/// Task tool (sync surface): delegates to the wired spawner.
pub fn task(ctx: &crate::registry::ToolCtx, args: &Value) -> crate::registry::ToolOutput {
    let agent = args["agent"].as_str().unwrap_or("general");
    let prompt = args["prompt"].as_str().unwrap_or("");
    if prompt.is_empty() {
        return crate::registry::ToolOutput::err("missing `prompt`");
    }
    match ctx.subagent.spawn(agent, prompt) {
        Ok(id) => crate::registry::ToolOutput::ok(format!(
            "task {id} started (agent {agent}); poll with TaskOutput"
        )),
        Err(e) => crate::registry::ToolOutput::err(e),
    }
}

/// TaskOutput tool: poll a spawned task.
pub fn task_output(ctx: &crate::registry::ToolCtx, args: &Value) -> crate::registry::ToolOutput {
    let Some(id) = args["task_id"]
        .as_u64()
        .or_else(|| args["task_id"].as_str().and_then(|s| s.parse().ok()))
    else {
        return crate::registry::ToolOutput::err("missing `task_id`");
    };
    match ctx.subagent.poll(id) {
        Ok(TaskStatus::Running) => crate::registry::ToolOutput::ok(format!("task {id}: running")),
        Ok(TaskStatus::Done { output }) => crate::registry::ToolOutput::ok(output),
        Ok(TaskStatus::Failed { error }) => crate::registry::ToolOutput::err(error),
        Err(e) => crate::registry::ToolOutput::err(format!("task {id}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_permissions_are_role_restricted() {
        use marfim_core::permissions::Mode;
        let (m, _) = agent_permissions("reviewer");
        assert_eq!(m, Mode::Plan, "reviewer is read-only (D16)");
        let (m2, _) = agent_permissions("general");
        assert_eq!(m2, Mode::Default);
        let (m3, _) = agent_permissions("security");
        assert_eq!(m3, Mode::Plan);
    }

    #[test]
    fn worktree_isolation_creates_and_removes() {
        let repo = std::env::temp_dir().join(format!("marfim-sub-repo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap()
        };
        run(&["init", "-q"]);
        std::fs::write(repo.join("README.md"), "# test repo\n").unwrap();
        run(&["add", "."]);
        let commit = run(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "init"]);
        assert!(
            commit.status.success(),
            "test repo setup failed: {}",
            String::from_utf8_lossy(&commit.stderr)
        );
        {
            let guard = WorktreeGuard::create(&repo).unwrap();
            assert!(guard.path.is_dir(), "worktree created");
            assert_ne!(guard.path, repo, "subagent does NOT run in the main checkout");
        }
        // after drop the worktree is gone
        let list = run(&["worktree", "list"]);
        assert!(
            !String::from_utf8_lossy(&list.stdout).contains("marfim-worktree"),
            "worktree cleaned up"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn task_table_allocates_unique_ids() {
        let t = TaskTable::default();
        let (a, _) = t.alloc("general");
        let (b, _) = t.alloc("explore");
        assert_ne!(a, b);
    }
}
