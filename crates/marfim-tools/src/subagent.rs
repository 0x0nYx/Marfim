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
        "general" | "build-resolver" | "silent-failure" => (Mode::Default, "builder".into()),
        _ => (Mode::Default, "builder (unknown persona falls back)".into()),
    }
}

/// §G subagent contract (distilled: OMP task tool + Hermes delegation):
/// tools a CHILD agent may NEVER call. Recursive delegation (Task /
/// TaskOutput) closes fork bombs; Memory / SessionSearch are the parent
/// session's channels (a child writing to the parent's user model or
/// reading its journal would leak state across the boundary); Monitor /
/// MonitorKill are long-lived surfaces a poll-based child cannot own.
/// The binary turns this into deny RULES on the child's engine —
/// deny>ask>allow keeps the engine the sole authority.
pub fn blocked_tools(agent: &str) -> Vec<&'static str> {
    let _ = agent;
    vec![
        "Task",
        "TaskOutput",
        "Memory",
        "SessionSearch",
        "Monitor",
        "MonitorKill",
        // W3.2: a terminal session outlives the child (long-lived process
        // surface) and recursion-closes like Task — parents own terminals.
        "Terminal",
    ]
}

/// The deny rules derived from [`blocked_tools`] (exact tool match, no
/// argv prefix: whole-tool denial).
pub fn blocked_rules(agent: &str) -> Vec<marfim_core::permissions::Rule> {
    blocked_tools(agent)
        .into_iter()
        .map(|tool| marfim_core::permissions::Rule {
            tool: tool.to_string(),
            decision: marfim_core::permissions::Decision::Deny,
            argv_prefix: None,
        })
        .collect()
}

/// Subagent isolation (D16 + W1.5): git repos get a detached worktree
/// (through the GitBackend); non-git projects degrade to a bounded COPY
/// of the tree — never the same dir, mutations can never touch the
/// original either way.
pub struct WorktreeGuard {
    pub path: std::path::PathBuf,
    repo: Option<std::path::PathBuf>,
    /// true = copy isolation (Drop removes the copy directory).
    copied: bool,
}

/// Bounded recursive copy for non-git isolation (W1.5). `.git` is
/// skipped; more than [`ISOLATION_COPY_FILE_CAP`] files is an honest
/// error (a runaway copy is worse than a refusal).
pub const ISOLATION_COPY_FILE_CAP: usize = 50_000;

pub fn copy_tree_bounded(src: &std::path::Path, dst: &std::path::Path) -> Result<usize, String> {
    fn walk(src: &std::path::Path, dst: &std::path::Path, count: &mut usize) -> Result<(), String> {
        for entry in
            std::fs::read_dir(src).map_err(|e| format!("copy read {}: {e}", src.display()))?
        {
            let entry = entry.map_err(|e| format!("copy entry: {e}"))?;
            let name = entry.file_name();
            if name == ".git" {
                continue; // history stays behind; the snapshot is the tree
            }
            let from = entry.path();
            let to = dst.join(&name);
            if from.is_dir() {
                std::fs::create_dir_all(&to).map_err(|e| format!("mkdir {to:?}: {e}"))?;
                walk(&from, &to, count)?;
            } else {
                *count += 1;
                if *count > ISOLATION_COPY_FILE_CAP {
                    return Err(format!(
                        "copy isolation refused: workspace exceeds {ISOLATION_COPY_FILE_CAP} files"
                    ));
                }
                std::fs::copy(&from, &to)
                    .map_err(|e| format!("copy {} → {}: {e}", from.display(), to.display()))?;
            }
        }
        Ok(())
    }
    let mut count = 0;
    std::fs::create_dir_all(dst).map_err(|e| format!("mkdir {dst:?}: {e}"))?;
    walk(src, dst, &mut count)?;
    Ok(count)
}

fn isolated_base(kind: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "marfim-{kind}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ))
}

impl WorktreeGuard {
    /// Returns a guarded dir: a fresh detached worktree for git repos, a
    /// bounded copy for non-git projects. NEVER the original directory.
    pub fn create(cwd: &std::path::Path) -> Result<Self, String> {
        let backend = marfim_core::git::default_backend();
        if backend.is_repo(cwd) {
            let base = isolated_base("worktree");
            backend.worktree_add_detached(cwd, &base)?;
            Ok(Self {
                path: base,
                repo: Some(cwd.to_path_buf()),
                copied: false,
            })
        } else {
            let base = isolated_base("copy");
            copy_tree_bounded(cwd, &base)?;
            Ok(Self {
                path: base,
                repo: None,
                copied: true,
            })
        }
    }
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        if let Some(repo) = &self.repo {
            let _ = marfim_core::git::default_backend().worktree_remove(repo, &self.path);
        }
        if self.copied {
            let _ = std::fs::remove_dir_all(&self.path);
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
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id, slot.clone());
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
        let repo = marfim_testkit::unique_dir("marfim-sub-repo");
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
        let commit = run(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "init",
        ]);
        assert!(
            commit.status.success(),
            "test repo setup failed: {}",
            String::from_utf8_lossy(&commit.stderr)
        );
        {
            let guard = WorktreeGuard::create(&repo).unwrap();
            assert!(guard.path.is_dir(), "worktree created");
            assert_ne!(
                guard.path, repo,
                "subagent does NOT run in the main checkout"
            );
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
    fn non_git_projects_degrade_to_bounded_copy_isolation() {
        let dir = marfim_testkit::unique_dir("marfim-sub-copy");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("hello.txt"), "hi\n").unwrap();
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(dir.join("nested/deep.txt"), "deep\n").unwrap();
        {
            let guard = WorktreeGuard::create(&dir).unwrap();
            assert_ne!(guard.path, dir, "never the original dir");
            assert!(
                guard.path.join("hello.txt").is_file(),
                "copy carries the files"
            );
            assert!(
                guard.path.join("nested/deep.txt").is_file(),
                "copy is recursive"
            );
            // mutations in the copy cannot touch the original
            std::fs::write(guard.path.join("hello.txt"), "mutated\n").unwrap();
        }
        let original = std::fs::read_to_string(dir.join("hello.txt")).unwrap_or_default();
        assert_eq!(original, "hi\n", "original untouched");
        // the copy is cleaned up with the guard
        let copies: Vec<_> = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("marfim-copy"))
            .collect();
        assert!(copies.is_empty(), "copy cleaned up on drop");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn blocked_tools_close_recursion_and_parent_channels() {
        use marfim_core::permissions::Decision;
        // §G: the blocked set is persona-independent (a child is a child)
        for persona in ["general", "explore", "reviewer", "unknown-x"] {
            let rules = blocked_rules(persona);
            let names: Vec<&str> = blocked_tools(persona);
            assert_eq!(rules.len(), names.len());
            assert!(names.contains(&"Task"), "recursion closed: {persona}");
            assert!(names.contains(&"Memory"), "parent memory closed");
            assert!(names.contains(&"SessionSearch"), "parent journal closed");
            for r in &rules {
                assert_eq!(r.decision, Decision::Deny);
                assert!(r.argv_prefix.is_none(), "whole-tool denial");
            }
        }
    }

    #[test]
    fn task_table_allocates_unique_ids() {
        let t = TaskTable::default();
        let (a, _) = t.alloc("general");
        let (b, _) = t.alloc("explore");
        assert_ne!(a, b);
    }
}
