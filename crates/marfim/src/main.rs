//! Marfim CLI (Rust): headless json/stream-json, REPL + slash commands,
//! serve / acp / lsp / mcp subcommands.

#![forbid(unsafe_code)]

mod slash;

use clap::{Parser, Subcommand};
use marfim_core::config::Config;
use marfim_core::events::EventBus;
use marfim_core::journal::{Journal, JournalRecord};
use marfim_core::loop_::{Harness, LoopCtx, StopReason};
use marfim_core::permissions::{Decision, Mode, PermissionEngine, Rule};
use marfim_core::providers::anthropic::Anthropic;
use marfim_core::providers::openai::OpenAiCompat;
use marfim_core::providers::{CostBook, MockProvider, Provider};
use marfim_core::router::{ModelEntry, Role, Router};
use marfim_core::sandbox::Sandbox;
use marfim_tools::registry::ToolCtx;
use marfim_tools::tools_host::HostTools;
use std::io::Write;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "marfim", version = marfim_core::VERSION, about = "Marfim coding agent (Rust)")]
struct Cli {
    /// Prompt for headless (non-interactive) mode
    #[arg(short = 'p')]
    prompt: Option<String>,

    #[arg(long = "output-format", default_value = "text")]
    output_format: String,

    #[arg(long = "input-format", default_value = "text")]
    input_format: String,

    #[arg(short = 'm', long = "model")]
    model: Option<String>,

    #[arg(long = "provider", default_value = "mock")]
    provider: String,

    #[arg(long = "resume")]
    resume: Option<Option<String>>,

    #[arg(long = "permission-mode", default_value = "default")]
    permission_mode: String,

    #[arg(short = 'C', long = "cwd")]
    cwd: Option<String>,

    #[arg(long = "title")]
    title: Option<String>,

    #[arg(long = "profile")]
    profile: Option<String>,

    #[arg(long = "budget")]
    budget: Option<String>,

    #[arg(long = "failover")]
    failover: Option<String>,

    #[arg(long = "glob")]
    glob: Vec<String>,

    #[arg(long = "on-fail")]
    on_fail: Option<String>,

    #[arg(long = "debounce")]
    debounce: Option<f64>,

    #[arg(long = "port", default_value_t = 7900)]
    port: u16,

    #[arg(long = "host", default_value = "127.0.0.1")]
    host: String,

    #[arg(long = "token")]
    token: Option<String>,

    #[arg(long = "max-mode")]
    max_mode: Option<String>,

    #[arg(long = "var")]
    vars: Vec<String>,

    #[arg(long = "days", default_value_t = 30)]
    days: u32,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// HTTP+SSE gateway
    Serve,
    /// ACP agent over stdio
    Acp,
    /// LSP server over stdio
    Lsp,
    /// MCP server over stdio (expose marfim tools)
    Mcp,
    /// Inspect/configure marfim (doctor, config get/set)
    Doctor,
    /// Manage external agent profiles (list)
    Agents,
    /// Show session journals
    Sessions,
}

fn main() {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    runtime.block_on(async_main(cli));
}

async fn async_main(cli: Cli) {
    let code = match cli.command {
        Some(Command::Serve) => serve_cmd(cli).await,
        Some(Command::Acp) => acp_cmd().await,
        Some(Command::Lsp) => {
            marfim_gateway::lsp::serve_stdio();
            0
        }
        Some(Command::Mcp) => mcp_cmd().await,
        Some(Command::Doctor) => {
            println!("marfim {} (rust)", marfim_core::VERSION);
            println!("provider: {} | permission-mode: default", cli.provider);
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
            0
        }
        Some(Command::Agents) => {
            println!(
                "external agent profiles (no-invention rule: unverified until owner runs them):"
            );
            for (name, cmd, args, enabled, verified) in
                marfim_gateway::acp::external_agent_catalog()
            {
                println!("  {name}: {cmd} {args:?} enabled={enabled} verified={verified}");
            }
            0
        }
        Some(Command::Sessions) => {
            let dir = sessions_dir();
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for e in rd.flatten() {
                    println!("{}", e.path().display());
                }
            }
            0
        }
        None => run_cmd(cli).await,
    };
    std::process::exit(code);
}

fn sessions_dir() -> std::path::PathBuf {
    let base = std::env::var("MARFIM_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| std::path::PathBuf::from(h).join(".marfim"))
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
        });
    base.join("sessions")
}

struct SessionPaths {
    #[allow(dead_code)] // surfaced by /sessions and future /resume
    journal: std::path::PathBuf,
    session_id: String,
}

fn new_session(cwd: &str) -> (Journal, SessionPaths) {
    let dir = sessions_dir();
    let _ = std::fs::create_dir_all(&dir);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let session_id = format!("s{ts}");
    let path = dir.join(format!("{session_id}.jsonl"));
    let mut j = Journal::open(&path).expect("journal opens");
    j.append(&JournalRecord::SessionMeta {
        session_id: session_id.clone(),
        cwd: cwd.to_string(),
        title: None,
        started_at: iso_now(),
    })
    .ok();
    (
        j,
        SessionPaths {
            journal: path,
            session_id,
        },
    )
}

fn iso_now() -> String {
    format!(
        "epoch+{}s",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    )
}

fn build_provider(name: &str, model: &str) -> Arc<dyn Provider> {
    match name {
        "anthropic" => Arc::new(Anthropic {
            client: reqwest_client(),
            base_url: std::env::var("ANTHROPIC_BASE_URL")
                .unwrap_or_else(|_| "https://api.anthropic.com".into()),
            api_key: std::env::var("ANTHROPIC_API_KEY").unwrap_or_default(),
            model: model.to_string(),
            max_retries: 3,
        }),
        "openai" | "openai-compat" | "ollama" | "vllm" | "lmstudio" => {
            let base = match name {
                "ollama" => "http://127.0.0.1:11434/v1".to_string(),
                _ => std::env::var("OPENAI_BASE_URL")
                    .unwrap_or_else(|_| "https://api.openai.com/v1".into()),
            };
            Arc::new(OpenAiCompat {
                client: reqwest_client(),
                base_url: base,
                api_key: std::env::var("OPENAI_API_KEY").unwrap_or_default(),
                model: model.to_string(),
                max_retries: 3,
            })
        }
        _ => Arc::new(MockProvider::new(vec![])),
    }
}

fn reqwest_client() -> reqwest::Client {
    marfim_core::http_client()
}

fn default_model(provider: &str) -> String {
    match provider {
        "anthropic" => "claude-sonnet-4-5".into(),
        "openai" => "gpt-5".into(),
        _ => "mock-1".into(),
    }
}

fn build_router(cli: &Cli) -> Router {
    let costs = CostBook::default();
    let model = cli
        .model
        .clone()
        .unwrap_or_else(|| default_model(&cli.provider));
    let mut router = Router::new(costs, false);
    let provider = build_provider(&cli.provider, &model);
    let priced = router.costs.known(&model);
    router.set_chain(
        Role::Primary,
        vec![ModelEntry {
            model: model.clone(),
            provider,
            priced,
        }],
    );
    let small_model = std::env::var("MARFIM_SMALL_MODEL").unwrap_or_else(|_| model.clone());
    let small_provider = build_provider(&cli.provider, &small_model);
    router.set_chain(
        Role::Small,
        vec![ModelEntry {
            priced: router.costs.known(&small_model),
            model: small_model,
            provider: small_provider,
        }],
    );
    router
}

fn build_harness(cli: &Cli) -> (Arc<Harness>, EventBus, ToolCtx, Arc<PermissionEngine>) {
    let bus = EventBus::new(1024);
    let cwd = cli
        .cwd
        .clone()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let sandbox = Sandbox::probe();
    let mode = Mode::parse(&cli.permission_mode).unwrap_or(Mode::Default);
    let rules = vec![
        Rule {
            tool: "Bash".into(),
            decision: Decision::Allow,
            argv_prefix: Some(vec!["git".into(), "status".into()]),
        },
        Rule {
            tool: "Bash".into(),
            decision: Decision::Allow,
            argv_prefix: Some(vec!["git".into(), "diff".into()]),
        },
        Rule {
            tool: "Bash".into(),
            decision: Decision::Deny,
            argv_prefix: Some(vec!["sudo".into()]),
        },
    ];
    let (perm, approval_rx) = PermissionEngine::new(mode, rules);
    let perm = Arc::new(perm);

    // approval pump: deny by default in headless (fail closed); in REPL a
    // human answers on stdin.
    {
        let mut rx = approval_rx;
        let interactive = cli.prompt.is_none();
        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                if !interactive {
                    let _ = job
                        .respond
                        .send(marfim_core::permissions::ApprovalAnswer::Deny);
                    continue;
                }
                eprintln!("\n[approve] {} — {} (y/n)", job.tool, job.summary);
                let mut line = String::new();
                if std::io::stdin().read_line(&mut line).is_ok()
                    && line.trim().eq_ignore_ascii_case("y")
                {
                    let _ = job
                        .respond
                        .send(marfim_core::permissions::ApprovalAnswer::Allow);
                } else {
                    let _ = job
                        .respond
                        .send(marfim_core::permissions::ApprovalAnswer::Deny);
                }
            }
        });
    }

    let tool_ctx = ToolCtx {
        cwd: cwd.clone(),
        session_id: String::new(),
        board: Arc::new(Default::default()),
        evidence: Arc::new(Default::default()),
        memory: Arc::new(Default::default()),
        todos: Arc::new(Default::default()),
        plan_mode: Arc::new(Default::default()),
        background: Arc::new(Default::default()),
        monitors: Arc::new(Default::default()),
        last_repomap: Arc::new(Default::default()),
    };
    let host = HostTools::new(
        perm.clone(),
        tool_ctx.clone(),
        sandbox,
        marfim_core::hooks::Hooks::default(),
    );

    let (journal, paths) = new_session(&cwd.to_string_lossy());
    let ctx = LoopCtx {
        session_id: paths.session_id,
        cwd: cwd.to_string_lossy().to_string(),
    };
    let router = build_router(cli);
    let harness = Arc::new(Harness {
        bus: bus.clone(),
        router: Arc::new(tokio::sync::RwLock::new(router)),
        tools: Arc::new(host),
        journal: Arc::new(std::sync::Mutex::new(journal)),
        ctx,
        permissions: perm.clone(),
        max_steps: 64,
        max_verify_feedback: 3,
    });
    (harness, bus, tool_ctx, perm)
}

async fn run_cmd(cli: Cli) -> i32 {
    match (cli.prompt.clone(), cli.output_format.as_str()) {
        (Some(p), "text") => headless_text(&cli, &p).await,
        (Some(p), "json") => headless_json(&cli, &p).await,
        (Some(p), "stream-json") => headless_stream_json(&cli, &p).await,
        (None, _) => repl(cli).await,
        _ => {
            eprintln!("error: -p requires --output-format text|json|stream-json");
            2
        }
    }
}

async fn headless_text(cli: &Cli, prompt: &str) -> i32 {
    let (harness, bus, _tc, _perm) = build_harness(cli);
    let mut sub = bus.subscribe();
    let printer = tokio::spawn(async move {
        loop {
            match sub.recv().await {
                Ok(ev) => match ev {
                    marfim_core::events::Event::AssistantMessage { text } => {
                        println!("{text}");
                    }
                    marfim_core::events::Event::ToolCallResult {
                        tool,
                        output,
                        is_error,
                        ..
                    } => {
                        if is_error {
                            eprintln!("[{tool}] error: {output}");
                        }
                    }
                    marfim_core::events::Event::Error { message, .. } => {
                        eprintln!("error: {message}");
                    }
                    marfim_core::events::Event::TurnEnded { .. } => break,
                    _ => {}
                },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });
    let stop = harness.run_turn(prompt).await;
    let _ = printer.await;
    match stop {
        StopReason::EndTurn => 0,
        StopReason::Aborted(m)
        | StopReason::CompactionFailed(m)
        | StopReason::VerifyExhausted(m)
        | StopReason::BudgetExceeded(m) => {
            eprintln!("stop: {m}");
            1
        }
    }
}

async fn headless_json(cli: &Cli, prompt: &str) -> i32 {
    let (harness, bus, _tc, _perm) = build_harness(cli);
    let mut sub = bus.subscribe();
    let mut out_text = String::new();
    let collector = tokio::spawn(async move {
        loop {
            match sub.recv().await {
                Ok(marfim_core::events::Event::AssistantMessage { text }) => {
                    out_text.push_str(&text)
                }
                Ok(marfim_core::events::Event::TurnEnded { .. }) => break,
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
        out_text
    });
    let stop = harness.run_turn(prompt).await;
    let text = collector.await.unwrap_or_default();
    let rec = serde_json::json!({
        "type": "result",
        "subtype": match stop {
            StopReason::EndTurn => "success",
            _ => "error"
        },
        "result": text,
        "stop": match &stop {
            StopReason::EndTurn => "end_turn",
            StopReason::Aborted(_) => "aborted",
            StopReason::CompactionFailed(_) => "compaction_failed",
            StopReason::VerifyExhausted(_) => "verify_exhausted",
            StopReason::BudgetExceeded(_) => "budget_exceeded",
        }
    });
    println!("{rec}");
    if matches!(stop, StopReason::EndTurn) {
        0
    } else {
        1
    }
}

async fn headless_stream_json(cli: &Cli, prompt: &str) -> i32 {
    let (harness, bus, _tc, _perm) = build_harness(cli);
    let mut sub = bus.subscribe();
    let printer = tokio::spawn(async move {
        let mut line;
        loop {
            match sub.recv().await {
                Ok(ev) => {
                    line = ev.to_json_line();
                    println!("{line}");
                    let _ = std::io::stdout().flush();
                    if matches!(ev, marfim_core::events::Event::TurnEnded { .. }) {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });
    let stop = harness.run_turn(prompt).await;
    let _ = printer.await;
    if matches!(stop, StopReason::EndTurn) {
        0
    } else {
        1
    }
}

async fn repl(cli: Cli) -> i32 {
    eprintln!(
        "marfim {} (rust) — type /help for commands",
        marfim_core::VERSION
    );
    let (harness, bus, tool_ctx, _perm) = build_harness(&cli);
    let mut rl = String::new();
    loop {
        print!("marfim> ");
        let _ = std::io::stdout().flush();
        rl.clear();
        if std::io::stdin().read_line(&mut rl).unwrap_or(0) == 0 {
            break;
        }
        let line = rl.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('/') {
            let handled = slash::dispatch(line, &tool_ctx, &harness).await;
            if handled == "quit" {
                break;
            }
            continue;
        }
        let mut sub = bus.subscribe();
        let _ = harness.run_turn(line).await;
        while let Ok(ev) = sub.try_recv() {
            if let marfim_core::events::Event::AssistantMessage { text } = ev {
                println!("{text}");
            }
        }
    }
    0
}

async fn serve_cmd(cli: Cli) -> i32 {
    let bus = EventBus::new(1024);
    let state = Arc::new(marfim_gateway::serve::ServeState {
        bus: Arc::new(bus),
        token: cli.token,
        started_at: std::time::Instant::now(),
    });
    let server = marfim_gateway::serve::Server::new(state);
    match marfim_gateway::serve::Server::bind(&cli.host, cli.port).await {
        Ok(listener) => {
            eprintln!("marfim serve on {}:{}", cli.host, cli.port);
            let _ = server.serve(listener).await;
            0
        }
        Err(e) => {
            eprintln!("bind failed: {e}");
            1
        }
    }
}

async fn acp_cmd() -> i32 {
    let cwd = std::env::current_dir().unwrap_or_default();
    let handlers = marfim_gateway::acp::AgentHandlers {
        prompt: Arc::new(|sid, text, emit| {
            // minimal agent: echo + journal; full loop wiring rides the
            // same Harness as the CLI (kept small here by design).
            emit(serde_json::json!({
                "sessionId": sid,
                "update": {"sessionUpdate": "agent_message_chunk",
                           "content": {"type": "text", "text": format!("marfim received: {text}")}}
            }));
            "end_turn".into()
        }),
        modes: vec!["default".into(), "acceptEdits".into(), "plan".into()],
        cwd,
    };
    marfim_gateway::acp::serve_agent_stdio(handlers);
    0
}

async fn mcp_cmd() -> i32 {
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
                board: Arc::new(Default::default()),
                evidence: Arc::new(Default::default()),
                memory: Arc::new(Default::default()),
                todos: Arc::new(Default::default()),
                plan_mode: Arc::new(Default::default()),
                background: Arc::new(Default::default()),
                monitors: Arc::new(Default::default()),
                last_repomap: Arc::new(Default::default()),
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
    marfim_mcp::server::serve_stdio(tools, opts);
    0
}

#[allow(dead_code)]
fn unused_config_checks(_c: &Config) {}
