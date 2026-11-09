//! Session factory + application plumbing (W1.1): config home, trust
//! store, journal session paths, the provider-chain builder, the real
//! subagent host, MCP connection, and the SessionHost factory. Extracted
//! verbatim from the `marfim` bin — the bin is thin (clap + tui + slash);
//! every embedding surface (FFI, terminal, Flutter transport) builds here.

#![forbid(unsafe_code)]

use crate::settings::{self, Cli, EffectiveSettings};
use marfim_core::config::Config;
use marfim_core::events::EventBus;
use marfim_core::journal::{Journal, JournalRecord};
use marfim_core::loop_::{Harness, LoopCtx, StopReason};
use marfim_core::permissions::PermissionEngine;
use marfim_core::providers::CostBook;
use marfim_core::router::Router;
use marfim_core::sandbox::Sandbox;
use marfim_core::session::SessionCommand;
use marfim_core::trust::{Trust, TrustStore};
use marfim_tools::registry::ToolCtx;
use marfim_tools::tools_host::HostTools;
use std::path::PathBuf;
use std::sync::Arc;

/// Connect every configured `mcpServers` entry and register its tools on
/// the host (registry + dispatch table). Returns connected server names.
pub async fn connect_mcp_servers(
    config: &Config,
    host: &mut marfim_tools::tools_host::HostTools,
) -> Vec<String> {
    let Some(serde_json::Value::Object(map)) = config.get("mcpServers").cloned() else {
        return vec![];
    };
    let mut connected = Vec::new();
    for (name, entry) in map.into_iter() {
        let cfg = match marfim_mcp::client::McpServerConfig::from_json(&entry) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("warning: mcpServers.{name}: {e} (skipped)");
                continue;
            }
        };
        match marfim_mcp::client::McpClient::connect(&name, &cfg).await {
            Ok(client) => {
                let tools = client.tools.lock().await.clone();
                let n = host.register_mcp_client(&client, &tools);
                eprintln!("mcp server {name}: connected, {n} tool(s)");
                connected.push(name);
            }
            Err(e) => {
                eprintln!("warning: mcp server {name}: unavailable: {e} (skipped)");
            }
        }
    }
    connected
}
/// One SessionHost per surface: the factory closes over config+settings
/// and assembles the full harness (providers, tools, journal) per session.
pub fn make_host(
    config: &Config,
    settings: Option<settings::EffectiveSettings>,
    cli: Cli,
    cwd: PathBuf,
) -> Arc<marfim_core::session_host::SessionHost> {
    let config = config.clone();
    let cli = cli.clone();
    Arc::new(marfim_core::session_host::SessionHost::new(Arc::new(
        move |req| {
            let config = config.clone();
            let cli = cli.clone();
            let cwd = cwd.clone();
            let captured = settings.clone();
            Box::pin(async move {
                // CLI-provided resume/title are the baseline; the request
                // may only refine them (an empty SessionRequest must NOT
                // wipe a `--resume` — regression caught by the resume e2e).
                let refine = |s: &mut settings::EffectiveSettings| {
                    if req.resume.is_some() {
                        s.resume = req.resume.clone();
                    }
                    if req.title.is_some() {
                        s.title = req.title.clone();
                    }
                };
                let mut s = match captured {
                    Some(s) => s,
                    // zero-config: resolve at first session creation so a
                    // provider added mid-flight is honored
                    None => settings::effective(&cli, &config, cwd)
                        .map_err(|e| format!("session unavailable: {e}"))?,
                };
                refine(&mut s);
                let (harness, bus, _tc, _perm, approval_rx) = build_session(&config, &s).await;
                Ok(marfim_core::session_host::SessionDeps {
                    harness,
                    bus: Arc::new(bus),
                    approval_rx,
                })
            })
        },
    )))
}
pub fn serve_tool_catalog() -> Vec<marfim_gateway::serve::ToolInfo> {
    let mut registry = marfim_tools::registry::ToolRegistry::new();
    marfim_tools::tools_host::register_builtins(&mut registry);
    registry
        .list_visible()
        .into_iter()
        .chain(registry.list_deferred())
        .map(|t| marfim_gateway::serve::ToolInfo {
            name: t.name.to_string(),
            description: t.description.to_string(),
            parameters: t.parameters.clone(),
        })
        .collect()
}

/// W2.1 fs route behavior: run the named tool through the REAL host path
/// (hooks → permission → sandbox → dispatch) — the gateway surface never
/// bypasses the permission engine. Tool errors become handler errors
/// (HTTP 400); success wraps the verbatim tool output.
fn gateway_tool_handler(
    host: Arc<HostTools>,
    cwd: String,
    tool: &'static str,
) -> marfim_gateway::serve::Handler {
    Arc::new(move |args| {
        let host = host.clone();
        let cwd = cwd.clone();
        Box::pin(async move {
            let ctx = marfim_core::loop_::LoopCtx {
                session_id: "gateway".into(),
                cwd,
                cancel: marfim_core::loop_::CancelToken::default(),
            };
            let (out, is_err) =
                marfim_core::loop_::ToolHost::execute(host.as_ref(), tool, &args, &ctx).await;
            if is_err {
                Err(out)
            } else {
                Ok(serde_json::json!({"tool": tool, "output": out}))
            }
        })
    })
}

/// W2.1 git route behavior: the real GitBackend's READ-ONLY operations
/// (status/diff/log) against the session cwd, on a blocking thread.
fn gateway_git_handler(cwd: PathBuf, op: GatewayGitOp) -> marfim_gateway::serve::Handler {
    Arc::new(move |args| {
        let cwd = cwd.clone();
        Box::pin(async move {
            let limit = args["limit"].as_u64().unwrap_or(20).min(200) as usize;
            let res = tokio::task::spawn_blocking(move || {
                let be = marfim_core::git::default_backend();
                if !be.is_repo(&cwd) {
                    return Err("not a git repository".to_string());
                }
                match op {
                    GatewayGitOp::Status => {
                        let s = be.status(&cwd)?;
                        Ok(serde_json::json!({
                            "backend": be.name(),
                            "dirty": s.dirty,
                            "modified": s.modified,
                        }))
                    }
                    GatewayGitOp::Diff => {
                        let d = be.diff(&cwd)?;
                        Ok(serde_json::json!({"backend": be.name(), "diff": d}))
                    }
                    GatewayGitOp::Log => {
                        let commits: Vec<serde_json::Value> = be
                            .log(&cwd, limit)?
                            .into_iter()
                            .map(|c| serde_json::json!({"id": c.id, "summary": c.summary}))
                            .collect();
                        Ok(serde_json::json!({"backend": be.name(), "commits": commits}))
                    }
                }
            })
            .await
            .map_err(|e| format!("git task: {e}"))??;
            Ok(res)
        })
    })
}

#[derive(Clone, Copy)]
enum GatewayGitOp {
    Status,
    Diff,
    Log,
}

/// W2.1 mcp route behavior: actually connected servers + their
/// registered tool names (schema-bearing registry entries).
fn gateway_mcp_handler(host: Arc<HostTools>) -> marfim_gateway::serve::Handler {
    Arc::new(move |_args| {
        let host = host.clone();
        Box::pin(async move {
            let servers: Vec<String> = {
                let map = host.mcp.lock().unwrap_or_else(|p| p.into_inner());
                let mut names: Vec<String> = map.values().map(|c| c.server_name.clone()).collect();
                names.sort();
                names.dedup();
                names
            };
            let tools = host.registry.mcp_names();
            Ok(serde_json::json!({"servers": servers, "tools": tools}))
        })
    })
}

/// The `marfim serve` embedding: fs/git/mcp route handlers + the
/// redacted config view. The gateway never holds credential material —
/// secrets stay in the credential chain; the config view exposes only
/// non-secret shape.
pub async fn serve_route_handlers(
    config: &Config,
    settings: &EffectiveSettings,
) -> (marfim_gateway::serve::RouteHandlers, serde_json::Value) {
    // ONE permission engine owns every gateway route (deny>ask>allow):
    // AppUid level still force-asks mutations; asks are DENIED fail-closed
    // (D14) because serve is unattended — a route can never hang on an
    // approval nobody will answer.
    let (perm, approval_rx) =
        PermissionEngine::new(settings.permission_mode, settings.rules.clone());
    let perm = match settings.sandbox_level {
        marfim_core::sandbox::SandboxLevel::AppUid => perm.with_force_ask_mutations(true),
        _ => perm,
    };
    let perm = Arc::new(
        perm.with_privacy(Some(marfim_core::privacy::PrivacyGuard {
            email: settings.account_email.clone(),
        }))
        .with_deny_instead_of_prompting(settings.deny_instead_of_prompting),
    );
    tokio::spawn(async move {
        let mut approval_rx = approval_rx;
        while let Some(job) = approval_rx.recv().await {
            let _ = job
                .respond
                .send(marfim_core::permissions::ApprovalAnswer::Deny);
        }
    });

    let tool_ctx = ToolCtx {
        cwd: settings.cwd.clone(),
        session_id: "gateway".into(),
        ..Default::default()
    };
    let mut host = HostTools::new(
        perm,
        tool_ctx,
        Sandbox::probe().with_level(settings.sandbox_level),
        marfim_core::hooks::Hooks::default(),
    );
    let _connected = connect_mcp_servers(config, &mut host).await;
    let host = Arc::new(host);
    let cwd_str = settings.cwd.to_string_lossy().to_string();

    let handlers = marfim_gateway::serve::RouteHandlers {
        fs_read: Some(gateway_tool_handler(host.clone(), cwd_str.clone(), "Read")),
        fs_list: Some(gateway_tool_handler(host.clone(), cwd_str.clone(), "LS")),
        fs_write: Some(gateway_tool_handler(host.clone(), cwd_str.clone(), "Write")),
        git_status: Some(gateway_git_handler(
            settings.cwd.clone(),
            GatewayGitOp::Status,
        )),
        git_diff: Some(gateway_git_handler(
            settings.cwd.clone(),
            GatewayGitOp::Diff,
        )),
        git_log: Some(gateway_git_handler(settings.cwd.clone(), GatewayGitOp::Log)),
        mcp_list: Some(gateway_mcp_handler(host.clone())),
        // W3.2: terminal routes dispatch through the REAL Terminal tool
        // (hooks → permission → sandbox argv wrap → dispatch) — the HTTP
        // surface never bypasses the permission engine.
        terminals: Some(gateway_tool_handler(host, cwd_str, "Terminal")),
    };
    (handlers, redacted_config_view(config, settings))
}

/// The config view served by GET /v1/config: provider/routing/sandbox
/// shape only. Credential material (api keys, env values, auth tokens)
/// is structurally absent — redaction happens HERE, not in transport.
pub fn redacted_config_view(config: &Config, settings: &EffectiveSettings) -> serde_json::Value {
    let mut mcp_servers = Vec::new();
    if let Some(serde_json::Value::Object(map)) = config.get("mcpServers").cloned() {
        mcp_servers.extend(map.keys().cloned());
    }
    serde_json::json!({
        "provider": settings.provider_name,
        "model": settings.model,
        "small_model": settings.small_model,
        "routing": settings.routing,
        "permission_mode": format!("{:?}", settings.permission_mode),
        "sandbox_level": format!("{:?}", settings.sandbox_level),
        "budgeted": settings.budgeted,
        "compact_threshold_tokens": settings.compact_threshold_tokens,
        "bash_default_timeout_ms": settings.bash_default_timeout_ms,
        "cwd": settings.cwd,
        "mcp_servers": mcp_servers,
        "secrets": "redacted"
    })
}
pub fn latest_session(dir: &std::path::Path) -> PathBuf {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                if let Ok(meta) = e.metadata() {
                    let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                    if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
                        best = Some((mtime, p));
                    }
                }
            }
        }
    }
    best.map(|(_, p)| p)
        .unwrap_or_else(|| dir.join("missing.jsonl"))
}
pub async fn build_session(
    config: &Config,
    settings: &EffectiveSettings,
) -> (
    Arc<Harness>,
    EventBus,
    ToolCtx,
    Arc<PermissionEngine>,
    tokio::sync::mpsc::Receiver<marfim_core::permissions::ApprovalJob>,
) {
    let bus = EventBus::new(4096);
    // W1.4: the attested sandbox level. AppUid/None print their honest
    // attestation once at session start; AppUid additionally forces
    // ask-all-mutations on the engine (deny floors stay absolute).
    let sandbox = Sandbox::probe().with_level(settings.sandbox_level);
    match settings.sandbox_level {
        marfim_core::sandbox::SandboxLevel::Enforced => {}
        level => eprintln!("{}", level.attestation()),
    }
    // W9.2: the network enforcement proxy — bound BEFORE the tools exist
    // (fail-closed: a demanded proxy that cannot bind refuses the start),
    // then injected into every sandboxed child's env as HTTP(S)_PROXY.
    let mut sandbox = sandbox;
    // W9.4 secret scoping: the config-named secrets never reach a child.
    sandbox.secret_scope = settings.secret_scope.clone();
    if settings.network_proxy {
        let allow = marfim_core::netproxy::Allowlist::from_entries(&settings.network_allow)
            .unwrap_or_else(|e| {
                eprintln!("error: {e}");
                std::process::exit(1);
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|e| {
                eprintln!("error: network proxy bind failed (fail-closed): {e}");
                std::process::exit(1);
            });
        let proxy_port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
        let (cancel, cancel_handle) = marfim_core::netproxy::tokio_util_like::cancel_channel();
        tokio::spawn(marfim_core::netproxy::serve(
            listener,
            Arc::new(allow),
            cancel,
        ));
        // The proxy lives for the process lifetime; the cancel handle is
        // deliberately leaked so the serve loop never sees a sender drop
        // (a vanished enforcement point would silently un-restrict bash).
        std::mem::forget(cancel_handle);
        eprintln!("network proxy: 127.0.0.1:{proxy_port} (allowlist enforced)");
        let p = format!("http://127.0.0.1:{proxy_port}");
        sandbox.extra_env = vec![
            ("HTTP_PROXY".into(), p.clone()),
            ("HTTPS_PROXY".into(), p.clone()),
            ("http_proxy".into(), p.clone()),
            ("https_proxy".into(), p),
        ];
    }
    let (perm, approval_rx) =
        PermissionEngine::new(settings.permission_mode, settings.rules.clone());
    let perm = match settings.sandbox_level {
        marfim_core::sandbox::SandboxLevel::AppUid => perm.with_force_ask_mutations(true),
        _ => perm,
    };
    // §privacy (claude-code `privacy` mod) + §P denyInsteadOfPrompting
    // (2.1.319): wired from config truth, not CLI.
    let perm = Arc::new(
        perm.with_privacy(Some(marfim_core::privacy::PrivacyGuard {
            email: settings.account_email.clone(),
        }))
        .with_deny_instead_of_prompting(settings.deny_instead_of_prompting)
        .with_observe(settings.observe),
    );

    // D16: real subagent host — Task/TaskOutput spawn nested agent loops
    let subagents = Arc::new(ProductionSubagents {
        tasks: Arc::new(marfim_tools::subagent::TaskTable::default()),
        config: config.clone(),
        provider_name: settings.provider_name.clone(),
        model: settings.model.clone(),
    });
    let tool_ctx = ToolCtx {
        cwd: settings.cwd.clone(),
        session_id: String::new(),
        board: Arc::new(Default::default()),
        evidence: Arc::new(Default::default()),
        // W10.1: the memory store persists at <cwd>/.marfim/memory.json
        // (the Memory tool saves on every mutation).
        memory: Arc::new(std::sync::Mutex::new(
            marfim_core::memory::MemoryStore::load(&marfim_core::memory::memory_path(
                &settings.cwd,
            )),
        )),
        todos: Arc::new(Default::default()),
        plan_mode: Arc::new(Default::default()),
        background: Arc::new(Default::default()),
        monitors: Arc::new(Default::default()),
        terminals: Arc::new(Default::default()),
        dircache: Arc::new(Default::default()),
        rea: Arc::new(Default::default()),
        last_repomap: Arc::new(Default::default()),
        subagent: subagents.clone(),
        bash_default_timeout_ms: settings.bash_default_timeout_ms,
        journal_path: Arc::new(std::sync::Mutex::new(None)),
        // W10.1: instincts persist at <cwd>/.marfim/instincts.json.
        instincts: Arc::new(std::sync::Mutex::new(
            marfim_core::instincts::InstinctStore::load(&marfim_core::instincts::instincts_path(
                &settings.cwd,
            )),
        )),
    };
    // W9.1: the exec policy table (when `[execpolicy]` is configured) —
    // attached to the PRODUCTION host only; subagent children inherit the
    // engine floors, not the policy table.
    let mut host = HostTools::new(
        perm.clone(),
        tool_ctx.clone(),
        sandbox,
        marfim_core::hooks::Hooks::default(),
    );
    if let Some(policy) = &settings.exec_policy {
        host = host.with_exec_policy(Arc::new(policy.clone()));
    }
    // MCP (step 2): construct McpClient per config `mcpServers` BEHIND the
    // trust gate — config layers are already trust-gated (D13), so an
    // untrusted cwd can never contribute mcpServers. Servers that fail to
    // connect are skipped with a warning (never silently); their tools
    // register as `mcp__<server>__<tool>` and dispatch through the SAME
    // PermissionEngine path as builtin tools, with scrubbed env.
    let _mcp_connected = connect_mcp_servers(config, &mut host).await;

    // --resume: reopen the named (or latest) journal
    let (journal, paths) = match &settings.resume {
        Some(name) => {
            let dir = sessions_dir();
            let path = if name.is_empty() {
                latest_session(&dir)
            } else {
                let p = dir.join(format!("{name}.jsonl"));
                if !p.is_file() {
                    dir.join(name)
                } else {
                    p
                }
            };
            match path.is_file() {
                true => {
                    let sid = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| "resumed".into());
                    let journal = match Journal::open(&path) {
                        Ok(j) => j,
                        Err(e) => {
                            eprintln!("error: cannot open journal {}: {e}", path.display());
                            std::process::exit(1);
                        }
                    };
                    (
                        journal,
                        SessionPaths {
                            journal: path,
                            session_id: sid,
                        },
                    )
                }
                false => {
                    eprintln!("error: no session journal at {}", path.display());
                    std::process::exit(1);
                }
            }
        }
        None => new_session(&settings.cwd.to_string_lossy(), settings.title.as_deref()),
    };

    // pricing: built-ins + the offline models.dev catalog for the active
    // provider chain (unknown models still fail closed under --budget)
    let mut costs = CostBook::default();
    costs.extend_from_catalog(&[
        "anthropic",
        "openai",
        "xai",
        "google",
        "groq",
        "openrouter",
        "deepseek",
        "mistral",
    ]);
    let mut router = Router::new(costs, settings.budgeted);
    if settings.routing == "bandit" {
        router.set_bandit(true);
    }
    // W7.4: the chain materials are prepared ONCE; the OAuth sources
    // refresh here (async context — a real HTTP round-trip), then every
    // chain (primary + small, pool-expanded) builds from them.
    let materials = settings::prepare_chain_materials(config).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(1);
    });
    if !materials.oauth.is_empty() {
        let mut any_ok = true;
        for src in &materials.oauth {
            match src.refresh_if_needed().await {
                Ok(true) => {
                    eprintln!(
                        "oauth: refreshed access token for `{}`",
                        src.credential_name()
                    )
                }
                Ok(false) => {}
                Err(e) => {
                    // A failed refresh keeps the previous token in place;
                    // the provider request will fail its own auth visibly
                    // if the token is actually dead.
                    eprintln!("oauth: refresh for `{}` failed: {e}", src.credential_name());
                    any_ok = false;
                }
            }
        }
        let _ = any_ok;
    }
    let primary =
        settings::build_primary_chain_with(settings, config, &materials).unwrap_or_else(|e| {
            eprintln!("error: {e}");
            std::process::exit(1);
        });
    router.set_chain(marfim_core::router::Role::Primary, primary);
    let small =
        settings::build_small_chain_with(settings, config, &materials).unwrap_or_else(|e| {
            eprintln!("error: {e}");
            std::process::exit(1);
        });
    router.set_chain(marfim_core::router::Role::Small, small);

    let ctx = LoopCtx {
        session_id: paths.session_id.clone(),
        cwd: settings.cwd.to_string_lossy().to_string(),
        cancel: marfim_core::loop_::CancelToken::default(),
    };
    let mut harness = Harness::new(
        bus.clone(),
        Arc::new(tokio::sync::RwLock::new(router)),
        Arc::new(host),
        Arc::new(std::sync::Mutex::new(journal)),
        ctx,
        perm.clone(),
        64,
        3,
        settings.compact_threshold_tokens,
        settings.stop_on_tool_error,
    );
    // Config truth: sampling overrides reach every provider request.
    harness.sampling = marfim_core::loop_::Sampling {
        max_completion_tokens: settings.max_tokens,
        temperature: settings.temperature,
        thinking_budget: settings.thinking_budget,
        effort: settings.effort,
    };
    // W7.2 config truth: the [cache] section reaches every primary request
    // (ttl / cachedContentName) and arms the bounded warming ping (warm).
    harness.cache = settings.cache.clone();
    // W7.5 config truth: the custom-grammar adoption form reaches the wire.
    harness.allow_custom_tools = settings.allow_custom_tools;
    // W8 config truth: the compaction stage order.
    harness.compaction_strategies = settings.compaction_strategies.clone();
    // Distillation §C1: the compaction trigger math needs the primary
    // model's context window from the offline catalog (best-effort —
    // unknown models keep the plain configured threshold).
    harness.context_window = marfim_core::catalog::catalog_provider_for(&settings.provider_name)
        .and_then(|pid| marfim_core::catalog::lookup_fuzzy(pid, &settings.model))
        .and_then(|m| m.context_tokens);
    // §S session_search corpus: the tool context reaches the journal.
    *tool_ctx
        .journal_path
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = Some(paths.journal.clone());
    // W10.1: the session-start prompt surfaces — capped memory render,
    // active instincts, active skills (each None when empty; the base
    // prompt stays byte-identical when all three are absent).
    harness.surfaces = marfim_core::loop_::PromptSurfaces {
        memory: {
            let m = tool_ctx
                .memory
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .render();
            (!m.trim().is_empty()).then(|| marfim_core::memcaps::cap_memory(&m))
        },
        instincts: tool_ctx
            .instincts
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .render_active(),
        skills: marfim_core::curator::SkillBook::new(&settings.cwd.join(".marfim").join("skills"))
            .render_active(),
    };
    // P5/Step 5: the brain's LearningFaculty is attached to the production
    // harness — observational hooks + turn-scoped validated actuations
    // (model_select / permission_tighten), journaled end to end. The Arc
    // is returned so the /why and /forget slash commands can inspect and
    // reset the SAME instance the loop feeds.
    let learning: Arc<dyn marfim_core::faculty::Faculty> =
        Arc::new(marfim_brain::faculty::LearningFaculty::new());
    harness.add_faculty(learning);
    let harness = Arc::new(harness);
    // P2 resume semantics: a resumed session rebuilds its conversation
    // history from the journal (user/assistant turns + compaction
    // summaries) — otherwise --resume would forget everything.
    if settings.resume.is_some() {
        match marfim_core::session::rebuild_history_from_journal(&harness, &paths.journal).await {
            Ok(n) => eprintln!(
                "resumed session {}: rebuilt {n} message(s) from journal",
                paths.session_id
            ),
            Err(e) => eprintln!(
                "warning: resume history rebuild failed for {}: {e}",
                paths.journal.display()
            ),
        }
    }
    (harness, bus, tool_ctx, perm, approval_rx)
}
/// D16: production subagent host — Task/TaskOutput spawn REAL nested
/// agent loops with role-restricted permissions and worktree isolation.
pub struct ProductionSubagents {
    tasks: Arc<marfim_tools::subagent::TaskTable>,
    config: Config,
    provider_name: String,
    model: String,
}

impl marfim_tools::subagent::SubagentSpawner for ProductionSubagents {
    fn spawn(&self, agent: &str, prompt: &str) -> Result<u64, String> {
        use marfim_core::loop_::{Harness, LoopCtx};
        use marfim_core::permissions::PermissionEngine;
        use marfim_core::providers::CostBook;
        use marfim_core::router::{Role, Router};
        let (mode, kind) = marfim_tools::subagent::agent_permissions(agent);
        let tasks = self.tasks.clone();
        let (id, slot) = tasks.alloc(agent);
        let config = self.config.clone();
        let provider = self.provider_name.clone();
        let model = self.model.clone();
        let cwd = std::env::current_dir().unwrap_or_default();
        let agent = agent.to_string();
        let prompt = prompt.to_string();

        // worktree isolation for git projects
        let guard = marfim_tools::subagent::WorktreeGuard::create(&cwd)?;
        let wt_path = guard.path.clone();
        {
            let mut s = slot.lock().unwrap_or_else(|p| p.into_inner());
            s.worktree = Some(wt_path.clone());
        }

        let handle = tokio::spawn(async move {
            // role-restricted permissions (D16) + §G blocked-tool deny
            // rules (children never recurse, never write the parent's
            // memory/journal): ONE engine owns both the approval queue
            // and the host path — deny>ask>allow stays authoritative.
            let rules = marfim_tools::subagent::blocked_rules(&agent);
            let (engine, mut approval_rx) = PermissionEngine::new(mode, rules);
            let perm = Arc::new(engine);
            tokio::spawn(async move {
                // subagents never prompt the user: asks fail closed
                while let Some(job) = approval_rx.recv().await {
                    let _ = job
                        .respond
                        .send(marfim_core::permissions::ApprovalAnswer::Deny);
                }
            });
            let tool_ctx = ToolCtx {
                cwd: wt_path.clone(),
                session_id: format!("task-{id}"),
                ..Default::default()
            };
            let host = HostTools::new(
                perm.clone(),
                tool_ctx.clone(),
                Sandbox::probe(),
                marfim_core::hooks::Hooks::default(),
            );
            let mut router = Router::new(CostBook::default(), false);
            if let Ok(preset) = settings::resolve_preset(&provider, &config) {
                let creds = marfim_core::credentials::CredentialChain::standard(&config);
                if let Ok(entries) = build_chain_from_preset(&preset, &model, &creds) {
                    router.set_chain(Role::Primary, entries);
                    let small_model = preset.default_model.clone();
                    if let Ok(small) = build_chain_from_preset(&preset, &small_model, &creds) {
                        router.set_chain(Role::Small, small);
                    }
                }
            }
            let ctx = LoopCtx {
                session_id: format!("task-{id}"),
                cwd: wt_path.display().to_string(),
                cancel: marfim_core::loop_::CancelToken::default(),
            };
            let harness = Harness::new(
                EventBus::new(64),
                Arc::new(tokio::sync::RwLock::new(router)),
                Arc::new(host),
                Arc::new(std::sync::Mutex::new(
                    match marfim_core::journal::Journal::open(
                        &std::env::temp_dir().join(format!("marfim-task-{id}.jsonl")),
                    ) {
                        Ok(j) => j,
                        Err(e) => {
                            // D6: fail the subagent honestly instead of panicking
                            // the spawned task
                            let mut s = slot.lock().unwrap_or_else(|p| p.into_inner());
                            s.status = Some(marfim_tools::subagent::TaskStatus::Failed {
                                error: format!("task journal: {e}"),
                            });
                            drop(guard); // worktree removed
                            return;
                        }
                    },
                )),
                ctx,
                perm,
                24,
                1,
                120_000,
                false,
            );
            let stop = harness
                .run_turn(&format!("[subagent {agent} — {kind}] {prompt}"))
                .await;
            let hist = harness.history.read().await;
            let output = hist
                .iter()
                .rev()
                .find(|m| m.role == "assistant")
                .map(|m| m.content.clone())
                .unwrap_or_default();
            drop(hist);
            let status = match stop {
                StopReason::EndTurn if !output.is_empty() => {
                    marfim_tools::subagent::TaskStatus::Done { output }
                }
                other => marfim_tools::subagent::TaskStatus::Failed {
                    error: format!("subagent stopped: {}", other.normalized()),
                },
            };
            {
                let mut s = slot.lock().unwrap_or_else(|p| p.into_inner());
                s.status = Some(status);
            }
            drop(guard); // worktree removed here
        });
        // detach: the subagent runs to completion independently; TaskOutput
        // polls the task table slot
        drop(handle);
        Ok(id)
    }

    fn poll(&self, task_id: u64) -> Result<marfim_tools::subagent::TaskStatus, String> {
        let slot = self
            .tasks
            .inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&task_id)
            .cloned()
            .ok_or_else(|| format!("no task {task_id}"))?;
        let s = slot.lock().unwrap_or_else(|p| p.into_inner());
        match &s.status {
            Some(st) => Ok(st.clone()),
            None => Ok(marfim_tools::subagent::TaskStatus::Running),
        }
    }
}

pub fn build_chain_from_preset(
    preset: &settings::Preset,
    model: &str,
    creds: &marfim_core::credentials::CredentialChain,
) -> Result<Vec<marfim_core::router::ModelEntry>, String> {
    Ok(vec![marfim_core::router::ModelEntry {
        model: model.to_string(),
        provider: settings::build_provider_with(preset, model, creds)?,
        priced: true,
    }])
}
/// D14 fail-closed approver for surfaces with NO interactive stdin owner
/// (headless, ACP): every pending approval is DENIED through the actor's
/// authoritative Approve command (the forwarded job's responder is a
/// tombstone by design — answering it directly would be a silent no-op).
pub fn spawn_fail_closed_denier(hosted: &Arc<marfim_core::session_host::HostedSession>) {
    let Some(mut approval_rx) = hosted.take_approval_receiver() else {
        return;
    };
    let cmds = hosted.actor.commands.clone();
    tokio::spawn(async move {
        while let Some(job) = approval_rx.recv().await {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = cmds
                .send(SessionCommand::Approve {
                    tool: job.tool.clone(),
                    summary: "*".into(),
                    answer: marfim_core::permissions::ApprovalAnswer::Deny,
                    reply: tx,
                })
                .await;
            let _ = rx.await;
        }
    });
}
pub fn marfim_home() -> PathBuf {
    marfim_core::platform::default_platform().config_dir()
}

/// D13: persisted trust store (~/.marfim/trust.toml). Unknown dirs default
/// to Workspace (the user cd'd there); explicitly marked dirs honour their
/// level. A dir marked untrusted can never contribute a config layer.
pub fn load_trust_store(home: &std::path::Path) -> TrustStore {
    let mut ts = TrustStore::default();
    let p = home.join("trust.toml");
    if let Ok(text) = std::fs::read_to_string(&p) {
        if let Ok(table) = text.parse::<toml::Table>() {
            if let Some(dirs) = table.get("dirs").and_then(|d| d.as_array()) {
                for d in dirs {
                    let path = d.get("path").and_then(|v| v.as_str()).unwrap_or("");
                    let level = d.get("level").and_then(|v| v.as_str()).unwrap_or("");
                    if !path.is_empty() {
                        if let Some(t) = Trust::parse(level) {
                            ts.set(path, t);
                        }
                    }
                }
            }
        }
    }
    ts
}

pub fn sessions_dir() -> PathBuf {
    marfim_home().join("sessions")
}

/// ISO-8601 UTC timestamp without external crates (civil-from-days).
pub fn iso_now() -> String {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

pub struct SessionPaths {
    pub journal: PathBuf,
    pub session_id: String,
}

pub fn new_session(cwd: &str, title: Option<&str>) -> (Journal, SessionPaths) {
    let dir = sessions_dir();
    let _ = std::fs::create_dir_all(&dir);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let session_id = format!("s{ts}");
    let path = dir.join(format!("{session_id}.jsonl"));
    let mut j = match Journal::open(&path) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("error: cannot open session journal {}: {e}", path.display());
            std::process::exit(1);
        }
    };
    let _ = j.append(&JournalRecord::SessionMeta {
        session_id: session_id.clone(),
        cwd: cwd.to_string(),
        title: title.map(String::from),
        started_at: iso_now(),
    });
    (
        j,
        SessionPaths {
            journal: path,
            session_id,
        },
    )
}
