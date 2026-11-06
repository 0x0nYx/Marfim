//! ToolHost bridge: registry → permissions → hooks → sandbox → execution.
//! Every tool call flows: PreToolUse hooks (fail closed) → permission
//! check (deny>ask>allow>mode, argv-level for Bash) → execute → PostToolUse.

use crate::registry::{ToolCtx, ToolOutput, ToolRegistry};
use marfim_core::loop_::{LoopCtx, ToolHost};
use marfim_core::permissions::{PermissionEngine, ToolClass};
use marfim_core::shellparse::ShellRisk;
use serde_json::Value;
use std::sync::Arc;

pub struct HostTools {
    pub registry: ToolRegistry,
    pub permissions: Arc<PermissionEngine>,
    pub hooks: marfim_core::hooks::Hooks,
    pub sandbox: marfim_core::sandbox::Sandbox,
    pub state: ToolCtx,
    /// Connected MCP servers by fully-qualified tool name
    /// (`mcp__<server>__<tool>`). Dispatch happens through the SAME
    /// permission path as builtin tools.
    pub mcp: std::sync::Arc<
        std::sync::Mutex<std::collections::BTreeMap<String, Arc<marfim_mcp::client::McpClient>>>,
    >,
}

impl HostTools {
    pub fn new(
        permissions: Arc<PermissionEngine>,
        state: ToolCtx,
        sandbox: marfim_core::sandbox::Sandbox,
        hooks: marfim_core::hooks::Hooks,
    ) -> Self {
        let mut registry = ToolRegistry::new();
        register_builtins(&mut registry);
        Self {
            registry,
            permissions,
            hooks,
            sandbox,
            state,
            mcp: Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new())),
        }
    }

    /// Register one connected MCP client's tools: registry entries
    /// (`mcp__<server>__<tool>`, schema-bearing) + dispatch table.
    /// `tools` is the client's listing, locked by the async caller.
    /// Returns the number of tools registered from this server.
    pub fn register_mcp_client(
        &mut self,
        client: &Arc<marfim_mcp::client::McpClient>,
        tools: &[(String, String, serde_json::Value)],
    ) -> usize {
        let mut n = 0;
        for (tool, _desc, schema) in tools {
            let full = format!("mcp__{}__{}", client.server_name, tool);
            self.registry
                .register_mcp(&client.server_name, tool, schema.clone());
            self.mcp
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(full, Arc::clone(client));
            n += 1;
        }
        n
    }
}

/// Convert an MCP tools/call result to (text, is_error): join the text
/// content blocks; honor isError.
fn mcp_result_to_output(result: &serde_json::Value) -> (String, bool) {
    let is_err = result["isError"].as_bool().unwrap_or(false);
    let mut text = String::new();
    if let Some(blocks) = result["content"].as_array() {
        for b in blocks {
            if b["type"].as_str() == Some("text") {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(b["text"].as_str().unwrap_or(""));
            }
        }
    }
    if text.is_empty() {
        text = result.to_string();
    }
    (text, is_err)
}

/// Register the full built-in surface (Node v1.2.0 names).
pub fn register_builtins(reg: &mut ToolRegistry) {
    use marfim_core::permissions::ToolClass as TC;
    macro_rules! tool {
        ($name:literal, $desc:literal, $class:expr, $conc:expr, $deferred:expr, $timeout:expr, $params:expr, $fn:expr) => {
            reg.register(crate::registry::ToolDef {
                name: $name,
                description: $desc,
                class: $class,
                concurrency_safe: $conc,
                deferred: $deferred,
                timeout_ms: $timeout,
                parameters: $params,
                run: Arc::new($fn),
            })
            .expect("tool registers");
        };
    }

    tool!(
        "Read",
        "Read a file (line-numbered; offset/limit supported, truncation marked).",
        TC::ReadOnly,
        true,
        false,
        10_000,
        serde_json::json!({"type":"object","required":["path"],"properties":{"path":{"type":"string"},"offset":{"type":"integer","description":"0-based line offset"},"limit":{"type":"integer","description":"max lines returned"}}}),
        crate::fs::read
    );
    tool!(
        "Write",
        "Write a file (creates parents).",
        TC::Edit,
        false,
        false,
        10_000,
        serde_json::json!({"type":"object","required":["path","content"],"properties":{"path":{"type":"string"},"content":{"type":"string"}}}),
        crate::fs::write
    );
    tool!(
        "Edit",
        "Edit a file via the exact → line-anchored → whitespace ladder.",
        TC::Edit,
        false,
        false,
        10_000,
        serde_json::json!({"type":"object","required":["path","old_string","new_string"],"properties":{"path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"},"replace_all":{"type":"boolean"}}}),
        crate::fs::edit
    );
    tool!(
        "MultiEdit",
        "Sequential edits, all-or-nothing.",
        TC::Edit,
        false,
        false,
        10_000,
        serde_json::json!({"type":"object","required":["path","edits"],"properties":{"path":{"type":"string"},"edits":{"type":"array"}}}),
        crate::fs::multi_edit
    );
    tool!(
        "Glob",
        "Find files by glob pattern (mtime-sorted).",
        TC::ReadOnly,
        true,
        false,
        10_000,
        serde_json::json!({"type":"object","required":["pattern"],"properties":{"pattern":{"type":"string"},"limit":{"type":"integer"}}}),
        crate::fs::glob
    );
    tool!(
        "Grep",
        "Regex search across files.",
        TC::ReadOnly,
        true,
        false,
        10_000,
        serde_json::json!({"type":"object","required":["pattern"],"properties":{"pattern":{"type":"string"},"include":{"type":"string"},"head_limit":{"type":"integer"}}}),
        crate::fs::grep
    );
    tool!(
        "LS",
        "List a directory (dirs first).",
        TC::ReadOnly,
        true,
        false,
        10_000,
        serde_json::json!({"type":"object","properties":{"path":{"type":"string"}}}),
        crate::fs::ls
    );
    tool!(
        "Bash",
        "Run a shell command (sandboxed, timeout, process-group kill).",
        TC::Execute,
        false,
        false,
        120_000,
        serde_json::json!({"type":"object","required":["command"],"properties":{"command":{"type":"string"},"timeout_ms":{"type":"integer"}}}),
        crate::bash::bash
    );
    tool!(
        "BashOutput",
        "Read background shell output.",
        TC::ReadOnly,
        true,
        false,
        5_000,
        serde_json::json!({"type":"object","required":["shell_id"],"properties":{"shell_id":{"type":"integer"}}}),
        crate::bash::bash_output
    );
    tool!(
        "KillShell",
        "Kill a background shell.",
        TC::Execute,
        false,
        false,
        5_000,
        serde_json::json!({"type":"object","required":["shell_id"],"properties":{"shell_id":{"type":"integer"}}}),
        crate::bash::kill_shell
    );
    tool!(
        "ApplyPatch",
        "Apply a unified diff patch.",
        TC::Edit,
        false,
        false,
        15_000,
        serde_json::json!({"type":"object","required":["patch"],"properties":{"patch":{"type":"string"}}}),
        crate::misc_tools::apply_patch_tool
    );
    tool!(
        "ApplyDiff",
        "Apply a single-file unified diff.",
        TC::Edit,
        false,
        false,
        15_000,
        serde_json::json!({"type":"object","required":["path","patch"],"properties":{"path":{"type":"string"},"patch":{"type":"string"}}}),
        crate::misc_tools::apply_diff_tool
    );
    tool!(
        "TodoWrite",
        "Replace the todo list.",
        TC::None_,
        true,
        false,
        5_000,
        serde_json::json!({"type":"object","required":["todos"],"properties":{"todos":{"type":"array"}}}),
        crate::misc_tools::todo_write
    );
    tool!(
        "Think",
        "Scratchpad: reason without side effects.",
        TC::None_,
        true,
        false,
        1_000,
        serde_json::json!({"type":"object","required":["thought"],"properties":{"thought":{"type":"string"}}}),
        |_c, a| crate::misc_tools::think(a)
    );
    tool!(
        "EnterPlanMode",
        "Enter read-only plan mode.",
        TC::None_,
        true,
        false,
        1_000,
        serde_json::json!({"type":"object"}),
        crate::misc_tools::enter_plan_mode
    );
    tool!(
        "ExitPlanMode",
        "Leave plan mode with the plan.",
        TC::None_,
        false,
        false,
        1_000,
        serde_json::json!({"type":"object","required":["plan"],"properties":{"plan":{"type":"string"}}}),
        crate::misc_tools::exit_plan_mode
    );
    tool!(
        "NotebookEdit",
        "Replace a Jupyter cell by id.",
        TC::Edit,
        false,
        false,
        10_000,
        serde_json::json!({"type":"object","required":["path","cell_id","new_source"],"properties":{"path":{"type":"string"},"cell_id":{"type":"string"},"new_source":{"type":"string"}}}),
        crate::misc_tools::notebook_edit
    );
    tool!(
        "RepoMap",
        "Ranked repository symbol map.",
        TC::ReadOnly,
        true,
        false,
        30_000,
        serde_json::json!({"type":"object","properties":{"budget":{"type":"integer"}}}),
        crate::misc_tools::repo_map
    );
    tool!(
        "Memory",
        "Persistent user model (remember/recall/forget).",
        TC::None_,
        true,
        false,
        5_000,
        serde_json::json!({"type":"object","required":["action"],"properties":{"action":{"type":"string"},"text":{"type":"string"},"id":{"type":"string"},"confirm":{"type":"boolean"},"safety":{"type":"boolean"}}}),
        crate::misc_tools::memory_tool
    );
    tool!(
        "Compute",
        "Safe arithmetic evaluation.",
        TC::None_,
        true,
        false,
        1_000,
        serde_json::json!({"type":"object","required":["expression"],"properties":{"expression":{"type":"string"}}}),
        |_c, a| crate::misc_tools::compute_tool(a)
    );
    tool!(
        "ToolSearch",
        "Search the deferred tool catalog.",
        TC::ReadOnly,
        true,
        false,
        1_000,
        serde_json::json!({"type":"object","required":["query"],"properties":{"query":{"type":"string"},"limit":{"type":"integer"}}}),
        crate::misc_tools::tool_search
    );
    tool!("Task", "Spawn a subagent (general|explore|plan|reviewer|security|silent-failure|build-resolver|evaluator).",
        TC::Execute, false, false, 600_000,
        serde_json::json!({"type":"object","required":["prompt"],"properties":{"agent":{"type":"string"},"prompt":{"type":"string"}}}),
        crate::subagent::task);
    tool!(
        "TaskOutput",
        "Poll a subagent task.",
        TC::ReadOnly,
        true,
        false,
        5_000,
        serde_json::json!({"type":"object","required":["task_id"],"properties":{"task_id":{"type":"string"}}}),
        crate::subagent::task_output
    );
    tool!(
        "WebFetch",
        "Fetch an https URL (SSRF-guarded).",
        TC::Network,
        true,
        false,
        30_000,
        serde_json::json!({"type":"object","required":["url"],"properties":{"url":{"type":"string"},"raw":{"type":"boolean"}}}),
        crate::web::web_fetch
    );
    tool!(
        "WebSearch",
        "Web search (requires API key).",
        TC::Network,
        true,
        false,
        30_000,
        serde_json::json!({"type":"object","required":["query"],"properties":{"query":{"type":"string"}}}),
        crate::web::web_search
    );
    tool!(
        "Monitor",
        "Watch a file for changes.",
        TC::ReadOnly,
        true,
        false,
        5_000,
        serde_json::json!({"type":"object","required":["path"],"properties":{"path":{"type":"string"}}}),
        crate::misc_tools::monitor
    );
    tool!(
        "MonitorKill",
        "Stop a monitor.",
        TC::ReadOnly,
        true,
        false,
        5_000,
        serde_json::json!({"type":"object","required":["monitor_id"],"properties":{"monitor_id":{"type":"integer"}}}),
        crate::misc_tools::monitor_kill
    );
    tool!(
        "BinInfo",
        "Inspect a binary file (magic, entropy).",
        TC::ReadOnly,
        true,
        true,
        10_000,
        serde_json::json!({"type":"object","required":["path"],"properties":{"path":{"type":"string"}}}),
        crate::misc_tools::bin_info
    );
    tool!(
        "BinStrings",
        "Extract printable strings from a binary.",
        TC::ReadOnly,
        true,
        true,
        10_000,
        serde_json::json!({"type":"object","required":["path"],"properties":{"path":{"type":"string"},"min_length":{"type":"integer"}}}),
        crate::misc_tools::bin_strings
    );
    tool!(
        "BinHexdump",
        "Hex dump a byte range.",
        TC::ReadOnly,
        true,
        true,
        10_000,
        serde_json::json!({"type":"object","required":["path"],"properties":{"path":{"type":"string"},"offset":{"type":"integer"},"length":{"type":"integer"}}}),
        crate::misc_tools::bin_hexdump
    );
    tool!(
        "BinSqlite",
        "Inspect a sqlite3 database.",
        TC::ReadOnly,
        true,
        true,
        10_000,
        serde_json::json!({"type":"object","required":["path"],"properties":{"path":{"type":"string"}}}),
        crate::misc_tools::bin_sqlite
    );
    tool!(
        "AstQuery",
        "AST queries (tree-sitter; not bundled).",
        TC::ReadOnly,
        true,
        true,
        10_000,
        serde_json::json!({"type":"object"}),
        crate::misc_tools::ast_query
    );
    tool!(
        "Eval",
        "Evaluate JS via the node binary.",
        TC::Execute,
        false,
        true,
        30_000,
        serde_json::json!({"type":"object","required":["code"],"properties":{"code":{"type":"string"}}}),
        crate::misc_tools::eval_js
    );
    tool!(
        "JsModuleGraph",
        "Import graph via the node binary.",
        TC::ReadOnly,
        true,
        true,
        30_000,
        serde_json::json!({"type":"object","required":["entry"],"properties":{"entry":{"type":"string"}}}),
        crate::misc_tools::js_module_graph
    );
}

#[async_trait::async_trait]
impl ToolHost for HostTools {
    async fn execute(&self, tool: &str, args: &Value, session: &LoopCtx) -> (String, bool) {
        let args_json = args.to_string();

        // 1. PreToolUse hooks — fail closed (INV-09)
        if let Err(e) = self.hooks.pre_tool_use(tool, &args_json).await {
            return (format!("blocked by hook: {e}"), true);
        }

        // 2. permission check with argv-level analysis for Bash
        let mut argv: Option<Vec<String>> = None;
        let mut risk: Option<ShellRisk> = None;
        if tool == "Bash" {
            if let Some(cmd) = args["command"].as_str() {
                let a = marfim_core::shellparse::analyze(cmd);
                risk = Some(a.risk);
                argv = a.segments.first().cloned();
            }
        }
        let class = self
            .registry
            .get(tool)
            .map(|t| t.class)
            .unwrap_or(ToolClass::Execute);
        let verdict = self
            .permissions
            .check(
                tool,
                class,
                argv.as_deref(),
                risk,
                &format!("{tool} {args_json}"),
            )
            .await;
        if verdict.decision == marfim_core::permissions::Decision::Deny {
            return (format!("permission denied: {}", verdict.reason), true);
        }

        // 3. dispatch — MCP tools first (Ask-class: every external server
        //    tool goes through the permission verdict above), then builtins
        //    (async tools run in-line; sync tools via spawn_blocking
        //    so they never stall the executor — D8)
        if tool.starts_with("mcp__") {
            let client = self
                .mcp
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(tool)
                .cloned();
            let Some(client) = client else {
                return (
                    format!(
                        "mcp tool {tool} has no connected server (server down at session start?)"
                    ),
                    true,
                );
            };
            let full_tool = tool.strip_prefix("mcp__").unwrap_or(tool);
            let tool_name = full_tool
                .split_once("__")
                .map(|(_, t)| t)
                .unwrap_or(full_tool);
            return match client.call_tool(tool_name, args.clone()).await {
                Ok(result) => {
                    let (text, is_err) = mcp_result_to_output(&result);
                    (text, is_err)
                }
                Err(e) => (format!("mcp {tool} failed: {e}"), true),
            };
        }
        let out: ToolOutput = match (tool, self.registry.get(tool)) {
            ("Bash", _) => {
                let cmd = args["command"].as_str().unwrap_or("").to_string();
                let timeout = args["timeout_ms"].as_u64().unwrap_or(120_000);
                let ctx = self.state.clone();
                let cwd = if session.cwd.is_empty() {
                    self.state.cwd.to_string_lossy().to_string()
                } else {
                    session.cwd.clone()
                };
                let mut ctx2 = ctx.clone();
                ctx2.cwd = std::path::PathBuf::from(cwd);
                let sandbox = self.sandbox.clone();
                let cancel = session.cancel.clone();
                let (text, err) =
                    crate::bash::run_bash(&ctx2, &cmd, &sandbox, timeout, &cancel).await;
                ToolOutput {
                    output: text,
                    is_error: err,
                }
            }
            ("WebFetch", _) => {
                let url = args["url"].as_str().unwrap_or("").to_string();
                let raw = args["raw"].as_bool().unwrap_or(false);
                match crate::web::guarded_fetch(&url, crate::web::MAX_BODY_BYTES).await {
                    Ok(body) => ToolOutput::ok(if raw {
                        body
                    } else {
                        crate::web::html_to_text(&body)
                    }),
                    Err(e) => ToolOutput::err(e),
                }
            }
            (_, Some(def)) => {
                // D8: sync tool fns run on the blocking pool with the
                // registry-enforced timeout applied by the loop layer.
                let ctx = self.state.clone();
                let args = args.clone();
                let run = def.run.clone();
                match tokio::task::spawn_blocking(move || run(&ctx, &args)).await {
                    Ok(out) => out,
                    Err(e) => ToolOutput::err(format!("tool task join failed: {e}")),
                }
            }
            (_, None) => ToolOutput::err(format!("unknown tool: {tool}")),
        };

        // 4. PostToolUse hooks (best-effort)
        self.hooks.post_tool_use(tool, &out.output).await;
        (out.output, out.is_error)
    }

    fn schemas(&self) -> Vec<Value> {
        self.registry.schemas(false)
    }

    fn is_mutating(&self, tool: &str) -> bool {
        // D7: Bash counts as mutating — any shell execution can arm verify.
        matches!(
            self.registry.get(tool).map(|t| t.class),
            Some(ToolClass::Edit | ToolClass::Execute)
        )
    }

    fn concurrency_safe(&self, tool: &str) -> bool {
        self.registry
            .get(tool)
            .map(|t| t.concurrency_safe)
            .unwrap_or(false)
    }

    fn tool_timeout_ms(&self, tool: &str) -> Option<u64> {
        self.registry.get(tool).map(|t| t.timeout_ms)
    }

    fn verify_commands(&self) -> Vec<String> {
        // project-aware verification commands
        let cwd = &self.state.cwd;
        if cwd.join("Cargo.toml").exists() {
            vec!["cargo test --quiet".into()]
        } else if cwd.join("package.json").exists() {
            vec!["npm test --silent".into()]
        } else if cwd.join("pyproject.toml").exists() || cwd.join("setup.py").exists() {
            vec!["python -m pytest -q".into()]
        } else {
            vec![]
        }
    }

    async fn run_verify(&self, session: &LoopCtx) -> (bool, String) {
        let cmds = self.verify_commands();
        let mut all_output = String::new();
        for c in &cmds {
            let ctx = self.state.clone();
            let mut ctx2 = ctx;
            ctx2.cwd = if session.cwd.is_empty() {
                self.state.cwd.clone()
            } else {
                std::path::PathBuf::from(&session.cwd)
            };
            let cancel = session.cancel.clone();
            let (out, err) = crate::bash::run_bash(&ctx2, c, &self.sandbox, 300_000, &cancel).await;
            all_output.push_str(&format!("$ {c}\n{out}\n"));
            if err {
                return (false, all_output);
            }
        }
        (true, all_output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marfim_core::permissions::{Decision, Mode, Rule};
    use std::sync::Arc;

    /// The built-in tool table must register cleanly into a fresh registry —
    /// this is the test that justifies the allowlisted startup `expect` in
    /// the `tool!` macro (a duplicate name is a programmer error).
    #[test]
    fn every_builtin_registers_cleanly() {
        let mut reg = crate::registry::ToolRegistry::new();
        register_builtins(&mut reg);
        let mut names: Vec<&str> = reg.list_visible().iter().map(|t| t.name).collect();
        names.extend(reg.list_deferred().iter().map(|t| t.name));
        assert!(!names.is_empty(), "tool table registered nothing");
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "duplicate tool names registered");
    }

    fn host(
        mode: Mode,
        rules: Vec<Rule>,
    ) -> (
        Arc<HostTools>,
        tokio::sync::mpsc::Receiver<marfim_core::permissions::ApprovalJob>,
    ) {
        let (perm, rx) = PermissionEngine::new(mode, rules);
        let dir = std::env::temp_dir().join(format!("marfim-host-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let state = ToolCtx {
            cwd: dir,
            session_id: "t".into(),
            board: Arc::new(Default::default()),
            evidence: Arc::new(Default::default()),
            memory: Arc::new(Default::default()),
            todos: Arc::new(Default::default()),
            plan_mode: Arc::new(Default::default()),
            background: Arc::new(Default::default()),
            monitors: Arc::new(Default::default()),
            last_repomap: Arc::new(Default::default()),
            subagent: Arc::new(crate::subagent::UnwiredSubagents),
        };
        (
            Arc::new(HostTools::new(
                Arc::new(perm),
                state,
                marfim_core::sandbox::Sandbox::probe(),
                marfim_core::hooks::Hooks::default(),
            )),
            rx,
        )
    }

    #[tokio::test]
    async fn read_tool_end_to_end() {
        let (h, _rx) = host(Mode::Default, vec![]);
        let dir = h.state.cwd.clone();
        std::fs::write(dir.join("f.txt"), "content").unwrap();
        let (out, err) = h
            .execute(
                "Read",
                &serde_json::json!({"path": "f.txt"}),
                &LoopCtx::default(),
            )
            .await;
        assert!(!err, "{out}");
        assert!(out.contains("content"));
    }

    #[tokio::test]
    async fn bash_denied_by_rule_never_runs() {
        let (h, _rx) = host(
            Mode::Yolo,
            vec![Rule {
                tool: "Bash".into(),
                decision: Decision::Deny,
                argv_prefix: Some(vec!["rm".into()]),
            }],
        );
        let (out, err) = h
            .execute(
                "Bash",
                &serde_json::json!({"command": "rm -rf /tmp/zzz"}),
                &LoopCtx::default(),
            )
            .await;
        assert!(err);
        assert!(out.contains("permission denied"), "{out}");
    }

    #[tokio::test]
    async fn bash_yolo_runs_simple() {
        let (h, _rx) = host(Mode::Yolo, vec![]);
        let (out, err) = h
            .execute(
                "Bash",
                &serde_json::json!({"command": "echo run-through"}),
                &LoopCtx::default(),
            )
            .await;
        assert!(!err, "{out}");
        assert!(out.contains("run-through"));
    }

    #[tokio::test]
    async fn hook_failure_blocks_tool() {
        let (perm, rx) = PermissionEngine::new(Mode::Yolo, vec![]);
        drop(rx); // no approval listener
        let dir = std::env::temp_dir().join(format!("marfim-host-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let state = ToolCtx {
            cwd: dir,
            session_id: "t".into(),
            board: Arc::new(Default::default()),
            evidence: Arc::new(Default::default()),
            memory: Arc::new(Default::default()),
            todos: Arc::new(Default::default()),
            plan_mode: Arc::new(Default::default()),
            background: Arc::new(Default::default()),
            monitors: Arc::new(Default::default()),
            last_repomap: Arc::new(Default::default()),
            subagent: Arc::new(crate::subagent::UnwiredSubagents),
        };
        let h = HostTools::new(
            Arc::new(perm),
            state,
            marfim_core::sandbox::Sandbox::probe(),
            marfim_core::hooks::Hooks::new(vec![marfim_core::hooks::HookSpec {
                event: marfim_core::hooks::HookEvent::PreToolUse,
                command: "exit 9".into(),
                timeout_ms: 500,
            }]),
        );
        let (out, err) = h
            .execute(
                "Read",
                &serde_json::json!({"path": "f.txt"}),
                &LoopCtx::default(),
            )
            .await;
        assert!(err);
        assert!(out.contains("blocked by hook"), "{out}");
    }

    #[test]
    fn verify_commands_are_project_aware() {
        let (h, _rx) = host(Mode::Default, vec![]);
        // temp dir has no project → no verify commands
        assert!(h.verify_commands().is_empty());
    }

    #[test]
    fn mutating_classes_cover_edit_tools() {
        let (h, _rx) = host(Mode::Default, vec![]);
        assert!(h.is_mutating("Write"));
        assert!(h.is_mutating("Edit"));
        assert!(!h.is_mutating("Read"));
        assert!(h.is_mutating("Bash"), "Bash arms verify (D7)");
    }

    #[test]
    fn concurrency_safety_and_timeouts_come_from_registry() {
        let (h, _rx) = host(Mode::Default, vec![]);
        assert!(h.concurrency_safe("Read"));
        assert!(!h.concurrency_safe("Bash"));
        assert_eq!(h.tool_timeout_ms("Bash"), Some(120_000));
        assert_eq!(h.tool_timeout_ms("unknown"), None);
    }

    /// D8 tighten: sync (exec-class) tools must run via spawn_blocking, so
    /// the async runtime keeps making progress while they block. With a
    /// current-thread runtime, an inline blocking call would starve the
    /// heartbeat task completely; spawn_blocking keeps it ticking.
    #[tokio::test(flavor = "current_thread")]
    async fn d8_sync_tools_run_on_blocking_pool_not_the_runtime() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (h, _rx) = host(Mode::Yolo, vec![]);
        let beat = Arc::new(AtomicUsize::new(0));
        let b2 = beat.clone();
        let heart = tokio::task::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                b2.fetch_add(1, Ordering::SeqCst);
            }
        });
        // Bash is exec-class (not concurrency-safe) → the spawn_blocking path
        let (out, err) = h
            .execute(
                "Bash",
                &serde_json::json!({"command": "sleep 0.4"}),
                &marfim_core::loop_::LoopCtx::default(),
            )
            .await;
        assert!(!err, "{out}");
        heart.abort();
        let beats = beat.load(Ordering::SeqCst);
        assert!(
            beats >= 20,
            "async runtime starved during sync tool (not spawn_blocking?): {beats} beats in 400ms"
        );
    }
}
