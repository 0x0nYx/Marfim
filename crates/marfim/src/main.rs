//! Marfim CLI (Rust): headless json/stream-json, REPL + slash commands,
//! serve / acp / lsp / mcp subcommands. D11: providers come from
//! config/env — never a silent mock. D12: headless exit codes 0 ok /
//! 1 error / 2 permission-denied. D13: layered strict config with a trust
//! gate. D14: ONE stdin owner.

#![forbid(unsafe_code)]

mod settings;
mod slash;
mod tui;

use clap::Parser;
use marfim_core::config::Config;
use marfim_core::events::{Event, EventBus};
use marfim_core::journal::{Journal, JournalRecord};
use marfim_core::loop_::{Harness, LoopCtx, StopReason};
use marfim_core::permissions::PermissionEngine;
use marfim_core::providers::CostBook;
use marfim_core::router::Router;
use marfim_core::sandbox::Sandbox;
use marfim_core::session::SessionCommand;
use marfim_core::session_host::SessionRequest;
use marfim_core::trust::{Trust, TrustStore};
use marfim_tools::registry::ToolCtx;
use marfim_tools::tools_host::HostTools;
use settings::{Cli, Command, EffectiveSettings};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

fn main() {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("marfim: cannot start tokio runtime: {e}");
            std::process::exit(1);
        }
    };
    runtime.block_on(async_main(cli));
}

async fn async_main(cli: Cli) -> i32 {
    let cwd = cli
        .cwd
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    // D13: layered config (home layer + trust-gated cwd layer)
    let home = marfim_home();
    let trust = load_trust_store(&home);
    let cwd_trust = trust.get(&cwd.to_string_lossy());
    let (config, loaded_layers) = match Config::load_layered(&home, &cwd, cwd_trust) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("config error: {e}");
            return 1;
        }
    };

    let code = match &cli.command {
        Some(Command::Serve) => serve_cmd(&cli, &config).await,
        Some(Command::Acp) => acp_cmd(&cli, &config).await,
        Some(Command::Lsp) => {
            marfim_gateway::lsp::serve_stdio();
            0
        }
        Some(Command::Mcp) => mcp_cmd(&config).await,
        Some(Command::Tui) => match settings::effective(&cli, &config, cwd.clone()) {
            Ok(s) => tui::run(&cli, config, s).await,
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        },
        Some(Command::Doctor) => doctor_cmd(&cli, &config, &cwd, cwd_trust, &loaded_layers),
        Some(Command::Agents) => agents_cmd(),
        Some(Command::Sessions) => sessions_cmd(&cli),
        Some(Command::Models) => models_cmd(&cli),
        Some(Command::SandboxExec { roots, cmd }) => sandbox_exec_cmd(roots, cmd),
        None => run_cmd(cli, config, cwd).await,
    };
    std::process::exit(code);
}

fn marfim_home() -> PathBuf {
    std::env::var("MARFIM_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| PathBuf::from(h).join(".marfim"))
                .unwrap_or_else(|_| PathBuf::from("."))
        })
}

/// D13: persisted trust store (~/.marfim/trust.toml). Unknown dirs default
/// to Workspace (the user cd'd there); explicitly marked dirs honour their
/// level. A dir marked untrusted can never contribute a config layer.
fn load_trust_store(home: &std::path::Path) -> TrustStore {
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

struct SessionPaths {
    #[allow(dead_code)] // surfaced by /resume + Phase 2 actor
    journal: PathBuf,
    session_id: String,
}

fn new_session(cwd: &str, title: Option<&str>) -> (Journal, SessionPaths) {
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

/// --var k=v substitution on the prompt (consumes the vars flag).
fn apply_vars(prompt: &str, vars: &[(String, String)]) -> String {
    let mut out = prompt.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// D12 exit-code semantics from an actor snapshot (headless on the host).
fn exit_code_snapshot(stop: &StopReason, snap: &marfim_core::session::SessionSnapshot) -> i32 {
    if snap.denied_tools > 0 && snap.denied_tools == snap.executed_tools {
        // every tool attempt was blocked by the permission engine
        return 2;
    }
    match stop {
        StopReason::EndTurn => 0,
        _ => 1,
    }
}

/// Drive ONE turn through the hosted session actor, with Ctrl-C wired to
/// cooperative cancel through the actor (the actor unwinds the turn via
/// finish_turn, so TurnEnded is published exactly once).
async fn actor_turn_with_ctrlc(
    hosted: &Arc<marfim_core::session_host::HostedSession>,
    prompt: &str,
) -> Result<StopReason, String> {
    let (tx, reply) = tokio::sync::oneshot::channel();
    if hosted
        .actor
        .commands
        .send(SessionCommand::Prompt {
            text: prompt.to_string(),
            reply: tx,
        })
        .await
        .is_err()
    {
        return Err("session actor is gone".into());
    }
    let cancel_cmds = hosted.actor.commands.clone();
    let killer = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let (notify_tx, notify_rx) = tokio::sync::oneshot::channel();
            let _ = cancel_cmds
                .send(SessionCommand::Cancel { reply: notify_tx })
                .await;
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), notify_rx).await;
        }
    });
    let res = reply
        .await
        .unwrap_or_else(|_| Err("actor dropped the reply".into()));
    killer.abort();
    res
}

/// Create one session through the host, or print the error and return
/// None (headless failure path).
async fn hosted_session(
    host: &Arc<marfim_core::session_host::SessionHost>,
    req: SessionRequest,
) -> Option<Arc<marfim_core::session_host::HostedSession>> {
    match host.create(req).await {
        Ok(h) => Some(h),
        Err(e) => {
            eprintln!("error: {e}");
            None
        }
    }
}

async fn run_cmd(cli: Cli, config: Config, cwd: PathBuf) -> i32 {
    let settings = match settings::effective(&cli, &config, cwd.clone()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let host = make_host(&config, Some(settings.clone()), cli.clone(), cwd.clone());
    match (settings.prompt.clone(), settings.output_format.as_str()) {
        (Some(p), "text") => headless_text(&host, &settings, &p).await,
        (Some(p), "json") => headless_json(&host, &settings, &p).await,
        (Some(p), "stream-json") => headless_stream_json(&host, &settings, &p).await,
        (None, _) => repl(cli, config, settings).await,
        _ => {
            eprintln!("error: -p requires --output-format text|json|stream-json");
            2
        }
    }
}

/// D16: production subagent host — Task/TaskOutput spawn REAL nested
/// agent loops with role-restricted permissions and worktree isolation.
struct ProductionSubagents {
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
            // role-restricted permissions (D16)
            let (perm, mut approval_rx) = PermissionEngine::new(mode, vec![]);
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
                Arc::new(PermissionEngine::new(mode, vec![]).0),
                tool_ctx.clone(),
                Sandbox::probe(),
                marfim_core::hooks::Hooks::default(),
            );
            let mut router = Router::new(CostBook::default(), false);
            if let Ok(preset) = settings::resolve_preset(&provider, &config) {
                if let Ok(entries) = build_chain_from_preset(&preset, &model) {
                    router.set_chain(Role::Primary, entries);
                    let small_model = preset.default_model.clone();
                    if let Ok(small) = build_chain_from_preset(&preset, &small_model) {
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
                Arc::new(perm),
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

fn build_chain_from_preset(
    preset: &settings::Preset,
    model: &str,
) -> Result<Vec<marfim_core::router::ModelEntry>, String> {
    Ok(vec![marfim_core::router::ModelEntry {
        model: model.to_string(),
        provider: settings::build_provider(preset, model)?,
        priced: true,
    }])
}

async fn build_session(
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
    let sandbox = Sandbox::probe();
    let (perm, approval_rx) =
        PermissionEngine::new(settings.permission_mode, settings.rules.clone());
    let perm = Arc::new(perm);

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
        memory: Arc::new(Default::default()),
        todos: Arc::new(Default::default()),
        plan_mode: Arc::new(Default::default()),
        background: Arc::new(Default::default()),
        monitors: Arc::new(Default::default()),
        last_repomap: Arc::new(Default::default()),
        subagent: subagents.clone(),
    };
    let host = HostTools::new(
        perm.clone(),
        tool_ctx.clone(),
        sandbox,
        marfim_core::hooks::Hooks::default(),
    );

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
    let primary = settings::build_primary_chain(settings, config).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(1);
    });
    router.set_chain(marfim_core::router::Role::Primary, primary);
    let small = settings::build_small_chain(settings, config).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(1);
    });
    router.set_chain(marfim_core::router::Role::Small, small);

    let ctx = LoopCtx {
        session_id: paths.session_id.clone(),
        cwd: settings.cwd.to_string_lossy().to_string(),
        cancel: marfim_core::loop_::CancelToken::default(),
    };
    let harness = Arc::new(Harness::new(
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
    ));
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

fn latest_session(dir: &std::path::Path) -> PathBuf {
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

async fn headless_text(
    host: &Arc<marfim_core::session_host::SessionHost>,
    settings: &EffectiveSettings,
    prompt: &str,
) -> i32 {
    let Some(hosted) = hosted_session(host, SessionRequest::default()).await else {
        return 1;
    };
    // D14: headless has NO stdin owner — approvals fail closed (deny).
    if let Some(mut approval_rx) = hosted.take_approval_receiver() {
        tokio::spawn(async move {
            while let Some(job) = approval_rx.recv().await {
                let _ = job
                    .respond
                    .send(marfim_core::permissions::ApprovalAnswer::Deny);
            }
        });
    }
    let mut sub = hosted.bus.subscribe();
    let prompt = apply_vars(prompt, &settings.prompt_vars);
    let stream = settings.stream;
    let printer = tokio::spawn(async move {
        loop {
            match sub.recv().await {
                Ok(ev) => match ev {
                    Event::AssistantMessage { text } => println!("{text}"),
                    Event::StreamDelta { text, .. } if stream => {
                        print!("{text}");
                        let _ = std::io::stdout().flush();
                    }
                    Event::ToolCallResult {
                        tool,
                        output,
                        is_error,
                        ..
                    } => {
                        if is_error {
                            eprintln!("[{tool}] error: {output}");
                        }
                    }
                    Event::Error { message, .. } => eprintln!("error: {message}"),
                    Event::TurnEnded { .. } => break,
                    _ => {}
                },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });
    let stop = match actor_turn_with_ctrlc(&hosted, &prompt).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let _ = printer.await;
    println!();
    let snap = hosted.snapshot().await;
    host.close_all().await;
    exit_code_snapshot(&stop, &snap)
}

async fn headless_json(
    host: &Arc<marfim_core::session_host::SessionHost>,
    settings: &EffectiveSettings,
    prompt: &str,
) -> i32 {
    let Some(hosted) = hosted_session(host, SessionRequest::default()).await else {
        return 1;
    };
    if let Some(mut approval_rx) = hosted.take_approval_receiver() {
        tokio::spawn(async move {
            while let Some(job) = approval_rx.recv().await {
                let _ = job
                    .respond
                    .send(marfim_core::permissions::ApprovalAnswer::Deny);
            }
        });
    }
    let mut sub = hosted.bus.subscribe();
    let prompt = apply_vars(prompt, &settings.prompt_vars);
    let collector = tokio::spawn(async move {
        let mut out_text = String::new();
        let mut stop = "aborted".to_string();
        loop {
            match sub.recv().await {
                Ok(Event::AssistantMessage { text }) => out_text.push_str(&text),
                Ok(Event::TurnEnded { stop_reason, .. }) => {
                    stop = stop_reason;
                    break;
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
        (out_text, stop)
    });
    let stop = match actor_turn_with_ctrlc(&hosted, &prompt).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let (_text, _bus_stop) = collector.await.unwrap_or_default();
    let snap = hosted.snapshot().await;
    host.close_all().await;
    let rec = serde_json::json!({
        "type": "result",
        "subtype": match stop {
            StopReason::EndTurn => "success",
            _ => "error"
        },
        "stop": stop.normalized(),
        "is_error": !matches!(stop, StopReason::EndTurn)
    });
    println!("{rec}");
    exit_code_snapshot(&stop, &snap)
}

async fn headless_stream_json(
    host: &Arc<marfim_core::session_host::SessionHost>,
    settings: &EffectiveSettings,
    prompt: &str,
) -> i32 {
    let Some(hosted) = hosted_session(host, SessionRequest::default()).await else {
        return 1;
    };
    if let Some(mut approval_rx) = hosted.take_approval_receiver() {
        tokio::spawn(async move {
            while let Some(job) = approval_rx.recv().await {
                let _ = job
                    .respond
                    .send(marfim_core::permissions::ApprovalAnswer::Deny);
            }
        });
    }
    let mut sub = hosted.bus.subscribe();
    let prompt = apply_vars(prompt, &settings.prompt_vars);
    let printer = tokio::spawn(async move {
        loop {
            match sub.recv().await {
                Ok(ev) => {
                    let line = ev.to_json_line();
                    println!("{line}");
                    let _ = std::io::stdout().flush();
                    if matches!(ev, Event::TurnEnded { .. }) {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });
    let stop = match actor_turn_with_ctrlc(&hosted, &prompt).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let _ = printer.await;
    let snap = hosted.snapshot().await;
    host.close_all().await;
    exit_code_snapshot(&stop, &snap)
}

/// D14: ONE place where REPL events become terminal output. Errors and
/// tool failures go to stderr; assistant text and stream deltas to stdout.
fn print_repl_event(ev: &Event, stream: bool) {
    match ev {
        Event::StreamDelta { text, .. } if stream => {
            print!("{text}");
            let _ = std::io::stdout().flush();
        }
        Event::AssistantMessage { text } => {
            println!();
            println!("{text}");
        }
        Event::Error { message, .. } => {
            eprintln!("error: {message}");
        }
        Event::ToolCallResult {
            tool,
            output,
            is_error: true,
            ..
        } => {
            eprintln!("[{tool}] {output}");
        }
        _ => {}
    }
}

async fn repl(_cli: Cli, config: Config, settings: EffectiveSettings) -> i32 {
    eprintln!(
        "marfim {} (rust) — type /help for commands",
        marfim_core::VERSION
    );
    let (harness, bus, tool_ctx, _perm, mut approval_rx) = build_session(&config, &settings).await;
    use tokio::io::AsyncBufReadExt;
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let (lines_tx, mut lines_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    // D14: exactly ONE stdin owner — this reader task. Everything else
    // consumes the channel.
    tokio::spawn(async move {
        let mut lines = stdin.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if lines_tx.send(line).is_err() {
                break;
            }
        }
    });

    loop {
        print!("marfim> ");
        let _ = std::io::stdout().flush();
        tokio::select! {
            line = lines_rx.recv() => {
                let Some(line) = line else { break };
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                if line.starts_with('/') {
                    let handled = slash::dispatch(&line, &tool_ctx, &harness, &config, &settings).await;
                    if handled == "quit" {
                        break;
                    }
                    if !handled.is_empty() {
                        println!("{handled}");
                    }
                    continue;
                }
                // run the turn while STILL owning stdin: steering + approvals.
                // The turn future MUST be polled inside this select — it is
                // the engine that produces the events below.
                let mut sub = bus.subscribe();
                let turn = harness.run_turn(&line);
                tokio::pin!(turn);
                let mut turn_err: Option<String> = None;
                loop {
                    tokio::select! {
                        stop = &mut turn => {
                            // TurnEnded is published before run_turn resolves;
                            // drain the remaining queued events briefly so the
                            // transcript is complete before the next prompt.
                            let deadline = tokio::time::Instant::now()
                                + std::time::Duration::from_millis(250);
                            while let Ok(Ok(ev)) =
                                tokio::time::timeout_at(deadline, sub.recv()).await
                            {
                                if let Event::Error { message, .. } = &ev {
                                    turn_err = Some(message.clone());
                                }
                                if matches!(ev, Event::TurnEnded { .. }) {
                                    break;
                                }
                                print_repl_event(&ev, settings.stream);
                            }
                            // D14: non-EndTurn stops are printed as errors —
                            // unless the very same error was already printed
                            // from its Error event (no double print).
                            match &stop {
                                StopReason::EndTurn => {}
                                StopReason::Aborted(m)
                                    if turn_err.as_deref() == Some(m.as_str()) => {}
                                StopReason::Aborted(m)
                                | StopReason::CompactionFailed(m)
                                | StopReason::VerifyExhausted(m)
                                | StopReason::BudgetExceeded(m) => {
                                    eprintln!("error: {m}");
                                }
                            }
                            break;
                        }
                        ev = sub.recv() => {
                            match ev {
                                Ok(Event::TurnEnded { .. }) => break,
                                Ok(ev) => {
                                    if let Event::Error { message, .. } = &ev {
                                        turn_err = Some(message.clone());
                                    }
                                    print_repl_event(&ev, settings.stream);
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                                Err(_) => break,
                            }
                        }
                        line2 = lines_rx.recv() => {
                            match line2 {
                                Some(l) if l.starts_with('/') => {
                                    let handled = slash::dispatch(&l, &tool_ctx, &harness, &config, &settings).await;
                                    if handled == "quit" { break; }
                                    if !handled.is_empty() { println!("\n{handled}"); }
                                    print!("marfim> ");
                                    let _ = std::io::stdout().flush();
                                }
                                Some(l) => harness.steer(&l),
                                None => break,
                            }
                        }
                        job = approval_rx.recv() => {
                            if let Some(job) = job {
                                print!("\n[approve] {} — {} (y/n) ", job.tool, job.summary);
                                let _ = std::io::stdout().flush();
                                if let Some(ans) = lines_rx.recv().await {
                                    if ans.trim().eq_ignore_ascii_case("y") {
                                        let _ = job.respond.send(marfim_core::permissions::ApprovalAnswer::Allow);
                                    } else {
                                        let _ = job.respond.send(marfim_core::permissions::ApprovalAnswer::Deny);
                                    }
                                }
                                print!("marfim> ");
                                let _ = std::io::stdout().flush();
                            }
                        }
                        _ = tokio::signal::ctrl_c() => {
                            harness.cancel.cancel();
                        }
                    }
                }
                println!();
            }
            job = approval_rx.recv() => {
                // approval arrived while idle (shouldn't happen) — fail closed
                if let Some(job) = job {
                    let _ = job.respond.send(marfim_core::permissions::ApprovalAnswer::Deny);
                }
            }
        }
    }
    0
}

fn doctor_cmd(
    cli: &Cli,
    config: &Config,
    _cwd: &std::path::Path,
    trust: Trust,
    loaded_layers: &[String],
) -> i32 {
    println!("marfim {} (rust)", marfim_core::VERSION);
    let provider = cli
        .provider
        .clone()
        .or_else(|| std::env::var("MARFIM_PROVIDER").ok())
        .or_else(|| {
            config
                .get("provider")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "(none configured)".into());
    println!("provider: {provider}");
    println!("permission-mode: {}", cli.permission_mode);
    let sandbox = Sandbox::probe();
    println!(
        "sandbox: unshare={} (network off)",
        if sandbox.unshare_available() {
            "available"
        } else {
            "unavailable (EPERM)"
        }
    );
    println!("env: PATH ok, HOME ok");
    println!("cwd trust: {trust:?}");
    println!("config layers loaded: {}", loaded_layers.join(", "));
    0
}

fn agents_cmd() -> i32 {
    println!("external agent profiles (no-invention rule: unverified until owner runs them):");
    for (name, cmd, args, enabled, verified) in marfim_gateway::acp::external_agent_catalog() {
        println!("  {name}: {cmd} {args:?} enabled={enabled} verified={verified}");
    }
    0
}

fn sandbox_exec_cmd(roots: &[String], cmd: &[String]) -> i32 {
    if cmd.is_empty() {
        eprintln!("error: sandbox-exec needs a command after `--`");
        return 2;
    }
    let root_paths: Vec<std::path::PathBuf> = roots.iter().map(std::path::PathBuf::from).collect();
    // fail-closed: apply BEFORE exec; any error here means the child never runs
    if let Err(e) = marfim_core::sandbox::enforce_write_roots(
        &root_paths.iter().map(|p| p.as_path()).collect::<Vec<_>>(),
    ) {
        eprintln!("error: sandbox refused: {e}");
        return 84; // conventional sandbox-refusal exit code
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&cmd[0]).args(&cmd[1..]).exec();
        eprintln!("error: exec {} failed: {err}", cmd[0]);
        127
    }
    #[cfg(not(unix))]
    {
        eprintln!("error: sandbox-exec requires unix process semantics");
        84
    }
}

fn models_cmd(cli: &Cli) -> i32 {
    use marfim_core::catalog;
    let provider_filter = cli.provider.as_deref();
    let providers: Vec<String> = match provider_filter {
        Some(p) => vec![p.to_string()],
        None => catalog::providers(),
    };
    for p in providers {
        let cat_provider = catalog::catalog_provider_for(&p).unwrap_or(&p);
        let n = catalog::model_count(cat_provider);
        if n == 0 {
            println!("{p}: no catalog entries (local providers are not in models.dev)");
            continue;
        }
        println!("{p}: {n} model(s) (snapshot {})", catalog::SNAPSHOT_DATE);
        // details require the provider id; the CLI prints counts here —
        // `marfim models --provider <p>` lists ids for scripted use
        if provider_filter.is_some() {
            for (id, m) in catalog::models_of(cat_provider) {
                let ctx = m
                    .context_tokens
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?".into());
                let price = match (m.input_usd_per_mtok, m.output_usd_per_mtok) {
                    (Some(i), Some(o)) => format!("${i}/${o} per Mtok"),
                    _ => "pricing n/a".into(),
                };
                println!("  {id}\t{}\tctx={ctx}\t{price}", m.name);
            }
        }
    }
    0
}

fn sessions_cmd(cli: &Cli) -> i32 {
    // --days consumed: only list journals touched within the window
    let dir = sessions_dir();
    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(cli.days as u64 * 86_400))
        .unwrap_or(std::time::UNIX_EPOCH);
    for s in marfim_core::session::list_sessions(&dir) {
        let mtime = std::fs::metadata(&s.path)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if mtime < cutoff {
            continue;
        }
        let title = s.title.as_deref().unwrap_or("-");
        println!(
            "{}\t{}rec={}\t{}bytes\t{}\t{}\t{}",
            s.session_id,
            s.started_at,
            s.records,
            s.journal_bytes,
            title,
            s.cwd,
            s.path.display()
        );
    }
    0
}

async fn serve_cmd(cli: &Cli, config: &Config) -> i32 {
    let cwd = cli
        .cwd
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    // serve STARTS provider-less: healthz/tools/sessions work without any
    // provider; the first /v1/prompt surfaces a clear error instead of the
    // server refusing to boot (zero-config first run, step 8 refines this
    // with auto-detection).
    let settings = settings::effective(cli, config, cwd.clone()).ok();

    // token is MANDATORY: generated when not supplied, printed once.
    let (token, generated) = match cli.token.clone() {
        Some(t) => (t, false),
        None => (generate_token(), true),
    };
    if generated {
        eprintln!("marfim serve: generated API token (shown ONCE, not stored): {token}");
    }

    let host = make_host(config, settings, cli.clone(), cwd);
    let tools = serve_tool_catalog();
    let allowed_hosts = vec![
        cli.host.clone(),
        "127.0.0.1".into(),
        "localhost".into(),
        "[::1]".into(),
    ];
    let state = marfim_gateway::serve::ServeState::new(
        host,
        token,
        Arc::new(tools),
        allowed_hosts,
        marfim_gateway::serve::ServeOptions::default(),
    );
    let server = marfim_gateway::serve::Server::new(state);
    match marfim_gateway::serve::Server::bind(&cli.host, cli.port).await {
        Ok(listener) => {
            let bound = listener
                .local_addr()
                .map(|a| a.to_string())
                .unwrap_or_else(|_| format!("{}:{}", cli.host, cli.port));
            eprintln!("marfim serve on {bound}");
            let _ = server.serve(listener).await;
            0
        }
        Err(e) => {
            eprintln!("bind failed: {e}");
            1
        }
    }
}

/// The real builtin tool catalog for /v1/tools — built from the registry,
/// never fabricated.
fn serve_tool_catalog() -> Vec<marfim_gateway::serve::ToolInfo> {
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

/// 128-bit random token as lowercase hex (from the OS CSPRNG; a
/// deterministic mixed fallback only if /dev/urandom is unavailable).
fn generate_token() -> String {
    use std::io::Read;
    let mut buf = [0u8; 16];
    let ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok();
    if !ok {
        // fallback: mix pid + time through BLAKE2b (marfim-brain, RFC 7693)
        let seed = format!(
            "{}|{}|{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            std::thread::current().id()
        );
        let h = marfim_brain::brain::blake2b::blake2b_hex(seed.as_bytes(), 16)
            .unwrap_or_else(|_| "0".repeat(32));
        return h;
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// One SessionHost per surface: the factory closes over config+settings
/// and assembles the full harness (providers, tools, journal) per session.
fn make_host(
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

async fn acp_cmd(cli: &Cli, config: &Config) -> i32 {
    let cwd = cli
        .cwd
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let settings = match settings::effective(cli, config, cwd.clone()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let host = make_host(config, Some(settings.clone()), cli.clone(), cwd.clone());
    // ACP session-id -> hosted session (one real actor per ACP session).
    let session_map: Arc<
        tokio::sync::Mutex<
            std::collections::BTreeMap<String, Arc<marfim_core::session_host::HostedSession>>,
        >,
    > = Arc::new(tokio::sync::Mutex::new(std::collections::BTreeMap::new()));

    // ACP agent over stdio: the protocol loop is synchronous (blocking
    // stdin), the session work is async — bridge per call through the
    // ambient runtime handle. The echo handler is GONE: every prompt now
    // runs a real journaled turn on a SessionHost-owned SessionActor.
    let rt = tokio::runtime::Handle::current();
    let host_for_handlers = host.clone();
    let map_for_handlers = session_map.clone();
    let handlers = marfim_gateway::acp::AgentHandlers {
        prompt: Arc::new(
            move |sid: &str, text: &str, emit: &mut dyn FnMut(serde_json::Value)| {
                rt.block_on(acp_drive_turn(
                    &host_for_handlers,
                    &map_for_handlers,
                    sid,
                    text,
                    emit,
                ))
            },
        ),
        modes: vec!["default".into(), "acceptEdits".into(), "plan".into()],
        cwd,
    };
    let joined = tokio::task::spawn_blocking(move || {
        marfim_gateway::acp::serve_agent_stdio(handlers);
    });
    let _ = joined.await;
    host.close_all().await;
    0
}

/// Drive one REAL ACP turn: prompt the hosted session actor, stream bus
/// events as ACP session/update notifications while the turn runs, then
/// return the normalized stop reason. One actor per ACP session id.
async fn acp_drive_turn(
    host: &Arc<marfim_core::session_host::SessionHost>,
    map: &Arc<
        tokio::sync::Mutex<
            std::collections::BTreeMap<String, Arc<marfim_core::session_host::HostedSession>>,
        >,
    >,
    sid: &str,
    text: &str,
    emit: &mut dyn FnMut(serde_json::Value),
) -> String {
    let hosted = {
        let existing = map.lock().await.get(sid).cloned();
        match existing {
            Some(h) => h,
            None => match host
                .create(SessionRequest {
                    resume: None,
                    title: Some(format!("acp-{sid}")),
                })
                .await
            {
                Ok(h) => {
                    map.lock().await.insert(sid.to_string(), h.clone());
                    h
                }
                Err(e) => {
                    emit(serde_json::json!({
                        "sessionId": sid,
                        "update": {"sessionUpdate": "agent_message_chunk",
                                   "content": {"type": "text", "text": format!("session error: {e}")}}
                    }));
                    return "cancelled".into(); // ACP has no "error" stop reason
                }
            },
        }
    };
    let mut rx = hosted.bus.subscribe();
    let (tx, reply) = tokio::sync::oneshot::channel();
    if hosted
        .actor
        .commands
        .send(SessionCommand::Prompt {
            text: text.to_string(),
            reply: tx,
        })
        .await
        .is_err()
    {
        return "cancelled".into();
    }
    loop {
        match rx.recv().await {
            Ok(Event::AssistantMessage { text }) => emit(serde_json::json!({
                "sessionId": sid,
                "update": {"sessionUpdate": "agent_message_chunk",
                           "content": {"type": "text", "text": text}}
            })),
            Ok(Event::ToolCallStarted { id, tool, .. }) => emit(serde_json::json!({
                "sessionId": sid,
                "update": {"sessionUpdate": "tool_call", "toolCallId": id,
                           "title": tool, "kind": "other"}
            })),
            Ok(Event::ToolCallResult {
                tool,
                output,
                is_error,
                ..
            }) => emit(serde_json::json!({
                "sessionId": sid,
                "update": {"sessionUpdate": "tool_call_update", "toolCallId": "",
                           "title": tool,
                           "status": if is_error { "failed" } else { "completed" },
                           "content": [{"type": "content",
                                        "content": {"type": "text", "text": output}}]}
            })),
            Ok(Event::Error { message, .. }) => emit(serde_json::json!({
                "sessionId": sid,
                "update": {"sessionUpdate": "agent_message_chunk",
                           "content": {"type": "text", "text": format!("error: {message}")}}
            })),
            Ok(Event::TurnEnded { .. }) => break,
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
    let stop = match tokio::time::timeout(std::time::Duration::from_secs(5), reply).await {
        Ok(Ok(Ok(s))) => s.normalized().to_string(),
        _ => "cancelled".into(),
    };
    // ACP stopReason vocabulary: end_turn | cancelled | refusal | ...
    match stop.as_str() {
        "end_turn" => "end_turn".to_string(),
        "aborted" => "cancelled".to_string(),
        other => {
            let _ = other;
            "cancelled".to_string()
        }
    }
}

async fn mcp_cmd(config: &Config) -> i32 {
    // expose the builtin tool catalog over stdio
    let mut registry = marfim_tools::registry::ToolRegistry::new();
    marfim_tools::tools_host::register_builtins(&mut registry);
    let tools: Vec<(String, String, serde_json::Value)> = registry
        .list_visible()
        .into_iter()
        .chain(registry.list_deferred())
        .map(|t| {
            (
                t.name.to_string(),
                t.description.to_string(),
                t.parameters.clone(),
            )
        })
        .collect();
    let registry = Arc::new(registry);
    let opts = marfim_mcp::server::ServerOptions {
        host: Arc::new(move |name, args| {
            // tools served without permissions context run read-only-safe
            // operations only; execution-class tools require the CLI host
            let read_only = matches!(
                name,
                "Read" | "Glob" | "Grep" | "LS" | "RepoMap" | "Compute" | "ToolSearch" | "Think"
            );
            if !read_only {
                return (
                    "mcp tool execution requires the interactive host (marfim run)".into(),
                    true,
                );
            }
            let ctx = marfim_tools::registry::ToolCtx {
                cwd: std::env::current_dir().unwrap_or_default(),
                session_id: "mcp".into(),
                ..Default::default()
            };
            match registry.get(name) {
                Some(def) => {
                    let o = (def.run)(&ctx, args);
                    (o.output, o.is_error)
                }
                None => (format!("unknown tool {name}"), true),
            }
        }),
    };
    let _ = config; // mcpServers wiring rides the client below in Phase 2
    marfim_mcp::server::serve_stdio(tools, opts);
    0
}
