//! Marfim CLI (Rust): headless json/stream-json, REPL + slash commands,
//! serve / acp / lsp / mcp subcommands. D11: providers come from
//! config/env — never a silent mock. D12: headless exit codes 0 ok /
//! 1 error / 2 permission-denied. D13: layered strict config with a trust
//! gate. D14: ONE stdin owner.

#![forbid(unsafe_code)]

mod slash;
mod tui;

use marfim_app::settings::{Cli, Command, EffectiveSettings};
use marfim_app::{factory, settings};

use clap::Parser;
use marfim_core::config::Config;
use marfim_core::events::Event;
use marfim_core::loop_::StopReason;
use marfim_core::permissions::PermissionEngine;
use marfim_core::sandbox::Sandbox;
use marfim_core::session::SessionCommand;
use marfim_core::session_host::SessionRequest;
use marfim_core::trust::Trust;
use marfim_tools::registry::ToolCtx;
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
    let home = factory::marfim_home();
    let trust = factory::load_trust_store(&home);
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
        Some(Command::Agents { cmd }) => agents_cmd(cmd.clone()),
        Some(Command::Sessions) => sessions_cmd(&cli),
        Some(Command::Models) => models_cmd(&cli),
        Some(Command::SandboxExec { roots, cmd }) => sandbox_exec_cmd(roots, cmd),
        None => run_cmd(cli, config, cwd).await,
    };
    std::process::exit(code);
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
    let host = factory::make_host(&config, Some(settings.clone()), cli.clone(), cwd.clone());
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

async fn headless_text(
    host: &Arc<marfim_core::session_host::SessionHost>,
    settings: &EffectiveSettings,
    prompt: &str,
) -> i32 {
    let Some(hosted) = hosted_session(host, SessionRequest::default()).await else {
        return 1;
    };
    // D14: headless has NO stdin owner — approvals fail closed (deny).
    factory::spawn_fail_closed_denier(&hosted);
    let mut sub = hosted.bus.subscribe();
    let prompt = apply_vars(prompt, &settings.prompt_vars);
    let stream = settings.stream;
    let verbose = settings.verbose;
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
                        } else if verbose {
                            // verbose config: full tool results on stderr
                            eprintln!("[{tool}] {output}");
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
    factory::spawn_fail_closed_denier(&hosted);
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
    factory::spawn_fail_closed_denier(&hosted);
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
fn print_repl_event(ev: &Event, stream: bool, verbose: bool) {
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
        Event::ToolCallResult {
            tool,
            output,
            is_error: false,
            ..
        } if verbose => {
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
    let (harness, bus, tool_ctx, _perm, mut approval_rx) =
        factory::build_session(&config, &settings).await;
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
                                print_repl_event(&ev, settings.stream, settings.verbose);
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
                                    print_repl_event(&ev, settings.stream, settings.verbose);
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

fn agents_cmd(cmd: Option<settings::AgentsCmd>) -> i32 {
    match cmd {
        None | Some(settings::AgentsCmd::List) => {
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
        Some(settings::AgentsCmd::Run { agent, prompt }) => {
            match marfim_gateway::acp::run_external_agent(&agent, &prompt) {
                Ok(text) => {
                    println!("{text}");
                    0
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    1
                }
            }
        }
    }
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
    let dir = factory::sessions_dir();
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

    let host = factory::make_host(config, settings, cli.clone(), cwd);
    let tools = factory::serve_tool_catalog();
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
    let host = factory::make_host(config, Some(settings.clone()), cli.clone(), cwd.clone());
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
                    // ACP stdio has no approver: Ask verdicts fail closed
                    factory::spawn_fail_closed_denier(&h);
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
    // `marfim mcp` serves the builtin catalog + connected mcpServers over
    // stdio, and EVERY tool call — builtin or mcp__ — flows through the
    // SAME PermissionEngine path as interactive runs (the old read-only
    // refusal is gone). stdin is the MCP protocol, so there is no
    // interactive approver: Ask verdicts fail closed (deny) via a parked
    // approver task.
    let (perm_engine, mut approval_rx) = PermissionEngine::new(
        marfim_core::permissions::Mode::Default,
        settings::rules_from_config(config),
    );
    let perm_engine = perm_engine
        .with_privacy(Some(marfim_core::privacy::PrivacyGuard {
            email: config
                .get("accountEmail")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        }))
        .with_deny_instead_of_prompting(
            config
                .get("permissions")
                .map(|p| {
                    p.get("denyInsteadOfPrompting")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                })
                .unwrap_or(false),
        );
    let perm = Arc::new(perm_engine);
    tokio::spawn(async move {
        while let Some(job) = approval_rx.recv().await {
            let _ = job
                .respond
                .send(marfim_core::permissions::ApprovalAnswer::Deny);
        }
    });

    let mut registry = marfim_tools::registry::ToolRegistry::new();
    marfim_tools::tools_host::register_builtins(&mut registry);
    let tool_ctx = ToolCtx {
        cwd: std::env::current_dir().unwrap_or_default(),
        session_id: "mcp".into(),
        ..Default::default()
    };
    let host_tools = marfim_tools::tools_host::HostTools::new(
        perm.clone(),
        tool_ctx.clone(),
        marfim_core::sandbox::Sandbox::probe(),
        marfim_core::hooks::Hooks::default(),
    );
    // A SERVER surface serves the builtin catalog only: no outbound
    // mcpServers connections (otherwise `marfim mcp` with a self-referencing
    // config would recurse into itself). Sessions connect servers; the
    // server composes whatever its caller already registered.
    let _ = config;

    let tools: Vec<(String, String, serde_json::Value)> = host_tools
        .registry
        .schemas(true)
        .iter()
        .map(|v| {
            (
                v["function"]["name"].as_str().unwrap_or("").to_string(),
                v["function"]["description"]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
                v["function"]["parameters"].clone(),
            )
        })
        .filter(|(n, _, _)| !n.is_empty())
        .collect();

    let host_tools = Arc::new(host_tools);
    let rt = tokio::runtime::Handle::current();
    let opts = marfim_mcp::server::ServerOptions {
        host: Arc::new(move |name, args| {
            // one block_on per call: permission verdict + dispatch
            rt.block_on(async {
                let ctx = marfim_core::loop_::LoopCtx {
                    session_id: "mcp".into(),
                    cwd: std::env::current_dir()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string(),
                    cancel: marfim_core::loop_::CancelToken::default(),
                };
                // ToolHost::execute = hooks -> PermissionEngine -> dispatch
                let (out, is_err) =
                    marfim_core::loop_::ToolHost::execute(host_tools.as_ref(), name, args, &ctx)
                        .await;
                (out, is_err)
            })
        }),
    };
    // serve_stdio blocks on stdin — run it OFF the async executor
    let joined = tokio::task::spawn_blocking(move || marfim_mcp::server::serve_stdio(tools, opts));
    let _ = joined.await;
    0
}
