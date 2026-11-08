//! Tool registry: ToolDef contract (canonical JSON schema, permission
//! class, concurrency-safety flag, enforced timeout, deferred flag) and
//! ToolSearch over deferred tools (codex distillation — PARITY §4).

use marfim_core::permissions::ToolClass;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ToolOutput {
    /// Model-facing output string.
    pub output: String,
    /// Error flag: tool results round-trip as errors on the wire.
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: false,
        }
    }

    pub fn err(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: true,
        }
    }
}

pub type ToolFn = dyn Fn(&ToolCtx, &Value) -> ToolOutput + Send + Sync;

#[derive(Clone)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub class: ToolClass,
    pub concurrency_safe: bool,
    /// Hidden from the default tool list; discoverable via ToolSearch.
    pub deferred: bool,
    pub timeout_ms: u64,
    pub parameters: Value,
    pub run: std::sync::Arc<ToolFn>,
}

impl std::fmt::Debug for ToolDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolDef")
            .field("name", &self.name)
            .field("class", &self.class)
            .field("deferred", &self.deferred)
            .finish()
    }
}

/// Execution context handed to every tool.
#[derive(Clone)]
pub struct ToolCtx {
    pub cwd: std::path::PathBuf,
    pub session_id: String,
    pub board: std::sync::Arc<std::sync::Mutex<marfim_core::board::MessageBoard>>,
    pub evidence: std::sync::Arc<std::sync::Mutex<marfim_core::guardian::EvidenceLedger>>,
    pub memory: std::sync::Arc<std::sync::Mutex<marfim_core::memory::MemoryStore>>,
    pub todos: std::sync::Arc<std::sync::Mutex<Vec<crate::todo::Todo>>>,
    pub plan_mode: std::sync::Arc<std::sync::Mutex<bool>>,
    pub background: std::sync::Arc<crate::bash::BackgroundShells>,
    pub monitors: std::sync::Arc<crate::monitor::Monitors>,
    /// W3.2: PTY terminal sessions (the Terminal tool). Bounded, honest;
    /// children of subagents get their own EMPTY server plus a whole-tool
    /// deny — long-lived process surfaces are a parent's responsibility.
    pub terminals: std::sync::Arc<marfim_term::TermServer>,
    /// W5.7: stamp-based directory listing cache (LS rides it).
    pub dircache: std::sync::Arc<crate::walkcache::DirCache>,
    /// W12: the REA evidence/obligation/unknowns ledger (the gate).
    pub rea: std::sync::Arc<std::sync::Mutex<marfim_core::rea::ReaLedger>>,
    pub last_repomap: std::sync::Arc<std::sync::Mutex<String>>,
    /// D16: real subagent spawner (UnwiredSubagents by default).
    pub subagent: std::sync::Arc<dyn crate::subagent::SubagentSpawner>,
    /// Config truth: `bash.defaultTimeoutMs` reaches the Bash tool here
    /// (explicit per-call `timeout_ms` still wins).
    pub bash_default_timeout_ms: u64,
    /// §S session_search corpus: the CURRENT session journal path (set by
    /// the binary when a session opens; `None` → the tool says so).
    pub journal_path: std::sync::Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
}

impl Default for ToolCtx {
    fn default() -> Self {
        Self {
            cwd: std::path::PathBuf::new(),
            session_id: String::new(),
            board: Arc::new(Default::default()),
            evidence: Arc::new(Default::default()),
            memory: Arc::new(Default::default()),
            todos: Arc::new(Default::default()),
            plan_mode: Arc::new(Default::default()),
            background: Arc::new(Default::default()),
            monitors: Arc::new(Default::default()),
            terminals: Arc::new(Default::default()),
            dircache: Arc::new(Default::default()),
            rea: Arc::new(Default::default()),
            last_repomap: Arc::new(Default::default()),
            subagent: Arc::new(crate::subagent::UnwiredSubagents),
            bash_default_timeout_ms: 120_000,
            journal_path: Arc::new(std::sync::Mutex::new(None)),
        }
    }
}

pub struct ToolRegistry {
    tools: BTreeMap<&'static str, ToolDef>,
    mcp_tools: BTreeMap<String, McpToolRef>,
}

#[derive(Debug, Clone)]
pub struct McpToolRef {
    pub server: String,
    pub tool: String,
    pub schema: Value,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: BTreeMap::new(),
            mcp_tools: BTreeMap::new(),
        }
    }

    pub fn register(&mut self, def: ToolDef) -> Result<(), String> {
        if self.tools.contains_key(def.name) {
            return Err(format!("duplicate tool: {}", def.name));
        }
        self.tools.insert(def.name, def);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&ToolDef> {
        self.tools.get(name)
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.tools.keys().copied().collect()
    }

    pub fn list_visible(&self) -> Vec<&ToolDef> {
        self.tools.values().filter(|t| !t.deferred).collect()
    }

    pub fn list_deferred(&self) -> Vec<&ToolDef> {
        self.tools.values().filter(|t| t.deferred).collect()
    }

    /// Wire schemas (OpenAI function-calling shape; dialect-agnostic core).
    pub fn schemas(&self, include_deferred: bool) -> Vec<Value> {
        let mut out: Vec<Value> = self
            .tools
            .values()
            .filter(|t| include_deferred || !t.deferred)
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters
                    }
                })
            })
            .collect();
        // D16: namespaced MCP tools are part of the visible catalog
        for (name, _ref) in &self.mcp_tools {
            out.push(serde_json::json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": format!("MCP tool `{name}`"),
                    "parameters": _ref.schema
                }
            }));
        }
        out
    }

    /// Keyword search across visible + deferred tools (ToolSearch).
    /// Ranks: name substring > description keyword hits.
    pub fn search(&self, query: &str, limit: usize) -> Vec<&ToolDef> {
        let q = query.to_lowercase();
        let kws: Vec<&str> = q.split_whitespace().collect();
        let mut scored: Vec<(i64, &ToolDef)> = Vec::new();
        for t in self.tools.values() {
            let mut score = 0i64;
            let name = t.name.to_lowercase();
            if name.contains(&q) {
                score += 100;
            }
            if name == q {
                score += 100;
            }
            let desc = t.description.to_lowercase();
            for kw in &kws {
                if name.contains(kw) {
                    score += 10;
                }
                if desc.contains(kw) {
                    score += 2;
                }
            }
            if score > 0 {
                scored.push((score, t));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(b.1.name)));
        scored.into_iter().take(limit).map(|(_, t)| t).collect()
    }

    /// Register a namespaced MCP tool: `mcp__<server>__<tool>`.
    /// Server names must not contain `__` (documented resolution rule).
    pub fn register_mcp(&mut self, server: &str, tool: &str, schema: Value) {
        let name = format!("mcp__{server}__{tool}");
        self.mcp_tools.insert(
            name,
            McpToolRef {
                server: server.into(),
                tool: tool.into(),
                schema,
            },
        );
    }

    pub fn mcp_tool(&self, name: &str) -> Option<&McpToolRef> {
        self.mcp_tools.get(name)
    }

    pub fn mcp_names(&self) -> Vec<String> {
        self.mcp_tools.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy(name: &'static str, desc: &'static str, deferred: bool) -> ToolDef {
        ToolDef {
            name,
            description: desc,
            class: ToolClass::ReadOnly,
            concurrency_safe: true,
            deferred,
            timeout_ms: 1000,
            parameters: serde_json::json!({"type": "object"}),
            run: std::sync::Arc::new(|_ctx, _args| ToolOutput::ok("ok")),
        }
    }

    #[test]
    fn duplicate_registration_rejected() {
        let mut r = ToolRegistry::new();
        r.register(dummy("Read", "read a file", false)).unwrap();
        assert!(r.register(dummy("Read", "dup", false)).is_err());
    }

    #[test]
    fn deferred_hidden_from_visible_list() {
        let mut r = ToolRegistry::new();
        r.register(dummy("Read", "read", false)).unwrap();
        r.register(dummy("AstQuery", "ast queries", true)).unwrap();
        assert_eq!(r.list_visible().len(), 1);
        assert_eq!(r.list_deferred().len(), 1);
        assert_eq!(r.schemas(false).len(), 1);
        assert_eq!(r.schemas(true).len(), 2);
    }

    #[test]
    fn search_ranks_name_over_description() {
        let mut r = ToolRegistry::new();
        r.register(dummy("RepoMap", "repository map overview", false))
            .unwrap();
        r.register(dummy("Grep", "search files for a pattern", false))
            .unwrap();
        r.register(dummy("Read", "read file contents", false))
            .unwrap();
        let hits = r.search("map", 5);
        assert_eq!(hits[0].name, "RepoMap");
        let hits2 = r.search("files", 5);
        assert!(hits2.iter().any(|t| t.name == "Grep"));
    }

    #[test]
    fn mcp_namespacing() {
        let mut r = ToolRegistry::new();
        r.register_mcp("fs", "read", serde_json::json!({}));
        assert!(r.mcp_tool("mcp__fs__read").is_some());
        assert_eq!(r.mcp_names(), vec!["mcp__fs__read".to_string()]);
        // server names with __ would be ambiguous — the resolution rule
        // strips "mcp__" then splits at the FIRST "__"
        let full = "mcp__fs__read";
        let rest = full.strip_prefix("mcp__").unwrap();
        let (server, tool) = rest.split_once("__").unwrap();
        assert_eq!((server, tool), ("fs", "read"));
    }
}
