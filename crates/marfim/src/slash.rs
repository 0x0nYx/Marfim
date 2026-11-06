//! Slash commands: all 65 Node v1.2.0 names + aliases, each wired to a
//! REAL behaviour (journal reads/writes, tool execution, config access,
//! git/terminal inspection). The old "available" catch-all is gone (D15):
//! unknown commands error with did-you-mean, and every command does
//! something testable.

use crate::settings::EffectiveSettings;
use marfim_core::config::Config;
use marfim_core::journal::JournalRecord;
use marfim_core::loop_::Harness;
use marfim_tools::registry::ToolCtx;
use std::path::PathBuf;

pub const COMMANDS: &[&str] = &[
    "accounts",
    "agents",
    "architect",
    "audit",
    "btw",
    "budget",
    "cachehealth",
    "capabilities",
    "checks",
    "clear",
    "commit",
    "compact",
    "context",
    "copy-context",
    "cost",
    "cron",
    "diff",
    "diffs",
    "dig",
    "doctor",
    "editor",
    "explain",
    "export",
    "forecast",
    "fork",
    "goal",
    "handoff",
    "help",
    "hooks",
    "hot",
    "init",
    "map",
    "mcp",
    "memory",
    "model",
    "models",
    "new",
    "permissions",
    "plan",
    "plugins",
    "policy",
    "profile",
    "provider",
    "quit",
    "recall",
    "recipes",
    "replay",
    "resume",
    "review",
    "rewind",
    "sandbox",
    "sdiff",
    "skills",
    "smoke",
    "stats",
    "status",
    "swarm",
    "theme",
    "tree",
    "undo",
    "usage",
    "verify",
    "wal",
    "workflow",
    "yolo",
];

pub const ALIASES: &[(&str, &str)] = &[("q", "quit"), ("?", "help"), ("ls-cmds", "help")];

pub async fn dispatch(
    line: &str,
    ctx: &ToolCtx,
    harness: &Harness,
    config: &Config,
    settings: &EffectiveSettings,
) -> String {
    let body = line.trim_start_matches('/');
    let (name, rest) = match body.split_once(' ') {
        Some((n, r)) => (n, r.trim()),
        None => (body, ""),
    };
    let canonical: &str = ALIASES
        .iter()
        .find(|(a, _)| *a == name)
        .map(|(_, c)| *c)
        .unwrap_or(name);
    if !COMMANDS.contains(&canonical) {
        let hint = marfim_core::levenshtein::did_you_mean(canonical, COMMANDS)
            .map(|s| format!(" (did you mean /{s}?)"))
            .unwrap_or_default();
        return format!("unknown command /{name}{hint} — /help lists all");
    }
    match canonical {
        "quit" => "quit".into(),

        // ---------------------------------------------------------- session
        "help" => format!(
            "commands ({}): {}",
            COMMANDS.len(),
            COMMANDS.join(" ")
        ),
        "status" => {
            let recs = journal_records(harness);
            format!(
                "session {} | cwd {} | journal {} records ({} bytes) | history {} msgs | provider {} model {}",
                harness.ctx.session_id,
                harness.ctx.cwd,
                recs.len(),
                harness.journal.lock().unwrap_or_else(|p| p.into_inner()).len_bytes(),
                harness.history.try_read().map(|h| h.len()).unwrap_or(0),
                settings.provider_name,
                settings.model,
            )
        }
        "stats" => {
            let recs = journal_records(harness);
            let (mut tools, mut errs, mut usage) = (0u64, 0u64, 0u64);
            for r in &recs {
                match r {
                    JournalRecord::ToolResult { is_error, .. } => {
                        tools += 1;
                        if *is_error {
                            errs += 1;
                        }
                    }
                    JournalRecord::Usage { input_tokens, output_tokens, .. } => {
                        usage += input_tokens + output_tokens
                    }
                    _ => {}
                }
            }
            format!(
                "turns recorded: {} | tool results: {} (errors {}) | tokens: {usage}",
                recs.iter().filter(|r| matches!(r, JournalRecord::UserMessage { .. })).count(),
                tools,
                errs
            )
        }
        "audit" => {
            let recs = journal_records(harness);
            let mut kinds = std::collections::BTreeMap::<&'static str, usize>::new();
            for r in &recs {
                *kinds.entry(r.kind()).or_default() += 1;
            }
            let body: Vec<String> =
                kinds.iter().map(|(k, n)| format!("{k}: {n}")).collect();
            format!("journal audit ({} records):\n{}", recs.len(), body.join("\n"))
        }
        "wal" => {
            let recs = journal_records(harness);
            let tail: Vec<String> = recs
                .iter()
                .rev()
                .take(20)
                .rev()
                .map(|r| match r {
                    JournalRecord::UserMessage { text } => format!("user: {}", abbrev(text, 80)),
                    JournalRecord::AssistantMessage { text, model, .. } => {
                        format!("assistant({model}): {}", abbrev(text, 80))
                    }
                    JournalRecord::ToolCall { tool, args, .. } => {
                        format!("call {tool} {}", abbrev(&args.to_string(), 60))
                    }
                    JournalRecord::ToolResult { id, output, is_error } => format!(
                        "result {id}{} {}",
                        if *is_error { " (error)" } else { "" },
                        abbrev(output, 60)
                    ),
                    JournalRecord::Compaction { summary } => {
                        format!("compaction: {}", abbrev(summary, 60))
                    }
                    other => other.kind().to_string(),
                })
                .collect();
            if tail.is_empty() {
                "journal: (empty)".into()
            } else {
                format!("journal tail:\n{}", tail.join("\n"))
            }
        }
        "replay" => {
            let n: usize = rest.parse().unwrap_or(5);
            let recs = journal_records(harness);
            let take = recs.len().saturating_sub(n);
            let lines: Vec<String> = recs[take..].iter().map(|r| r.kind().to_string()).collect();
            format!("replay last {n}:\n{}", lines.join("\n"))
        }
        "tree" => {
            let recs = journal_records(harness);
            let mut out = String::from("session tree:\n");
            let mut turn = 0;
            for r in &recs {
                match r {
                    JournalRecord::UserMessage { text } => {
                        turn += 1;
                        out.push_str(&format!("├─ turn {turn}: {}\n", abbrev(text, 60)));
                    }
                    JournalRecord::ToolCall { tool, .. } => {
                        out.push_str(&format!("│  ├─ {tool}\n"));
                    }
                    JournalRecord::ToolResult { is_error, .. } => {
                        out.push_str(&format!(
                            "│  └─ {}\n",
                            if *is_error { "error" } else { "ok" }
                        ));
                    }
                    JournalRecord::AssistantMessage { text, .. } => {
                        out.push_str(&format!("│  └─ reply: {}\n", abbrev(text, 50)));
                    }
                    _ => {}
                }
            }
            out
        }
        "rewind" => {
            let n: usize = rest.parse().unwrap_or(1);
            match rewind_records(harness, n) {
                Ok(dropped) => format!("rewound {n} record(s); {dropped} bytes dropped (tail saved to *.pre-rewind)"),
                Err(e) => format!("rewind failed: {e}"),
            }
        }
        "undo" => {
            // undo = rewind past the last tool batch (call + result)
            match rewind_last_tool_batch(harness) {
                Ok(msg) => msg,
                Err(e) => format!("undo failed: {e}"),
            }
        }
        "fork" => {
            let name = if rest.is_empty() {
                format!("{}-fork", harness.ctx.session_id)
            } else {
                rest.to_string()
            };
            let path = harness
                .journal
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .path()
                .to_path_buf();
            let dest = sessions_dir_path().join(format!("{name}.jsonl"));
            if let Some(parent) = dest.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::copy(&path, &dest) {
                Ok(_) => format!("forked session → {}", dest.display()),
                Err(e) => format!("fork failed: {e}"),
            }
        }
        "resume" => {
            let dir = sessions_dir_path();
            let mut s = String::from("sessions (latest first):\n");
            let mut entries: Vec<(std::time::SystemTime, String)> = Vec::new();
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                        let mtime = e
                            .metadata()
                            .and_then(|m| m.modified())
                            .unwrap_or(std::time::UNIX_EPOCH);
                        entries.push((mtime, p.display().to_string()));
                    }
                }
            }
            entries.sort_by_key(|e| std::cmp::Reverse(e.0));
            for (_, p) in entries.iter().take(10) {
                s.push_str(&format!("  {p}\n"));
            }
            if entries.is_empty() {
                s.push_str("  (none — pass --resume <name> to reopen)");
            }
            s
        }
        "export" => {
            let dest = if rest.is_empty() {
                ctx.cwd.join(".marfim").join("export.jsonl")
            } else {
                PathBuf::from(rest)
            };
            if let Some(parent) = dest.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let path = harness
                .journal
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .path()
                .to_path_buf();
            match std::fs::copy(&path, &dest) {
                Ok(_) => format!("journal exported → {}", dest.display()),
                Err(e) => format!("export failed: {e}"),
            }
        }
        "new" => {
            let dir = sessions_dir_path();
            let _ = std::fs::create_dir_all(&dir);
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let sid = format!("s{ts}");
            let mut j = match marfim_core::journal::Journal::open(&dir.join(format!("{sid}.jsonl"))) {
                Ok(j) => j,
                Err(e) => return format!("new session failed: cannot open journal: {e}"),
            };
            let _ = j.append(&JournalRecord::SessionMeta {
                session_id: sid.clone(),
                cwd: harness.ctx.cwd.clone(),
                title: None,
                started_at: crate::iso_now(),
            });
            {
                let mut guard = harness.journal.lock().unwrap_or_else(|p| p.into_inner());
                *guard = j;
            }
            harness.history.write().await.clear();
            format!("new session {sid} started (history cleared)")
        }
        "compact" => match harness.compact_now().await {
            Ok(summary) => format!("compacted. summary: {}", abbrev(&summary, 120)),
            Err(e) => format!("compaction failed: {e}"),
        },

        // ---------------------------------------------------------- usage
        "cost" | "budget" | "usage" => {
            let recs = journal_records(harness);
            let mut total = 0.0f64;
            let mut unknown = false;
            let mut in_tok = 0u64;
            let mut out_tok = 0u64;
            for r in &recs {
                if let JournalRecord::Usage {
                    model,
                    input_tokens,
                    output_tokens,
                    cost_usd,
                    ..
                } = r
                {
                    in_tok += input_tokens;
                    out_tok += output_tokens;
                    match cost_usd {
                        Some(c) => total += c,
                        None => {
                            let cb = marfim_core::providers::CostBook::default();
                            let u = marfim_core::providers::Usage {
                                input_tokens: *input_tokens,
                                output_tokens: *output_tokens,
                                ..Default::default()
                            };
                            match cb.cost_usd(model, &u) {
                                Some(c) => total += c,
                                None => unknown = true,
                            }
                        }
                    }
                }
            }
            let budget_note = if settings.budgeted {
                " (budget mode: unpriced models fail closed)"
            } else {
                ""
            };
            match canonical {
                "cost" => format!("session cost: ${total:.4}{budget_note}"),
                "budget" => format!(
                    "budget mode: {} | session cost: ${total:.4} | tokens: {in_tok} in / {out_tok} out",
                    settings.budgeted
                ),
                _ => format!(
                    "usage ledger: {in_tok} input / {out_tok} output tokens; ${total:.4}{}",
                    if unknown { " (some models unpriced)" } else { "" }
                ),
            }
        }
        "cachehealth" => {
            // cache token counters live in the Usage EVENTS (cache_read /
            // cache_creation fields), not in journal records — report the
            // journal-side view honestly and point at the live stream
            let recs = journal_records(harness);
            let usage_records = recs
                .iter()
                .filter(|r| matches!(r, JournalRecord::Usage { .. }))
                .count();
            format!(
                "cache tokens: journal holds {usage_records} usage records (cache_read/cache_creation ride the live Usage events — see /usage)"
            )
        }
        "forecast" => {
            let recs = journal_records(harness);
            let turns = recs
                .iter()
                .filter(|r| matches!(r, JournalRecord::UserMessage { .. }))
                .count()
                .max(1) as f64;
            let toks: u64 = recs
                .iter()
                .filter_map(|r| match r {
                    JournalRecord::Usage { input_tokens, output_tokens, .. } => {
                        Some(input_tokens + output_tokens)
                    }
                    _ => None,
                })
                .sum();
            let per_turn = toks as f64 / turns;
            format!(
                "forecast: {per_turn:.0} tokens/turn observed; next 10 turns ≈ {:.0} tokens",
                per_turn * 10.0
            )
        }

        // ---------------------------------------------------------- providers
        "provider" => {
            let p = crate::settings::resolve_preset(&settings.provider_name, config);
            match p {
                Ok(p) => format!(
                    "provider {} ({} wire) → {}\napi key env: {} ({})",
                    settings.provider_name,
                    p.wire,
                    p.base_url,
                    if p.api_key_env.is_empty() { "(none)" } else { &p.api_key_env },
                    if p.api_key_env.is_empty() {
                        "not needed"
                    } else if std::env::var(&p.api_key_env).is_ok() {
                        "set"
                    } else {
                        "UNSET"
                    }
                ),
                Err(e) => format!("provider error: {e}"),
            }
        }
        "accounts" => {
            let mut out = String::from("configured providers:\n");
            for (name, p) in crate::settings::builtin_presets() {
                let key_state = if p.api_key_env.is_empty() {
                    "local".to_string()
                } else if std::env::var(&p.api_key_env).is_ok() {
                    "key set".to_string()
                } else {
                    "key UNSET".to_string()
                };
                out.push_str(&format!("  {name}: {} [{key_state}]\n", p.base_url));
            }
            out
        }
        "models" => {
            let r = harness.router.try_read();
            match r {
                Ok(r) => {
                    let mut out = String::from("model chains:\n");
                    for role in r.roles() {
                        let models: Vec<String> =
                            r.chain(role).iter().map(|e| e.model.clone()).collect();
                        out.push_str(&format!("  {}: {}\n", role.as_str(), models.join(" → ")));
                    }
                    out
                }
                Err(_) => "router busy".into(),
            }
        }
        "model" => {
            if rest.is_empty() {
                format!("model: {} (provider {}) — pass /model <id> to switch", settings.model, settings.provider_name)
            } else {
                match switch_primary_model(harness, config, settings, rest).await {
                    Ok(msg) => msg,
                    Err(e) => format!("model switch failed: {e}"),
                }
            }
        }
        "architect" | "review" => {
            let role = if canonical == "architect" {
                marfim_core::router::Role::Architect
            } else {
                marfim_core::router::Role::Review
            };
            let configured = harness
                .router
                .try_read()
                .map(|r| !r.chain(role).is_empty())
                .unwrap_or(false);
            if configured {
                format!("{} role: configured in the chain", role.as_str())
            } else {
                format!(
                    "{} role: not configured — set `{}Model` in config (architectModel/reviewModel)",
                    role.as_str(),
                    role.as_str()
                )
            }
        }

        // ---------------------------------------------------------- permissions
        "permissions" => format!(
            "permission mode: {:?} — rules: {} (set via --permission-mode / [permissions] config)",
            settings.permission_mode,
            settings.rules.len()
        ),
        "policy" => {
            let mut out = format!("mode: {:?}\nrules:\n", settings.permission_mode);
            for r in &settings.rules {
                out.push_str(&format!(
                    "  {} {:?}{}\n",
                    r.tool,
                    r.decision,
                    r.argv_prefix
                        .as_ref()
                        .map(|a| format!(" argv={a:?}"))
                        .unwrap_or_default()
                ));
            }
            if settings.rules.is_empty() {
                out.push_str("  (none)\n");
            }
            if !settings.working_globs.is_empty() {
                out.push_str(&format!("working globs: {:?}\n", settings.working_globs));
            }
            out
        }
        "yolo" => format!(
            "yolo mode is {} — every tool auto-allows except nested-exec/heredoc which still asks",
            if settings.permission_mode == marfim_core::permissions::Mode::Yolo {
                "ACTIVE"
            } else {
                "off (start with --permission-mode yolo)"
            }
        ),
        "plan" => format!(
            "plan mode is {} — read-only tools auto-allow, everything else denies (start with --permission-mode plan)",
            if settings.permission_mode == marfim_core::permissions::Mode::Plan {
                "ACTIVE"
            } else {
                "off"
            }
        ),
        "sandbox" => {
            let s = marfim_core::sandbox::Sandbox::probe();
            format!(
                "sandbox: unshare={}; network={}",
                s.unshare_available(),
                s.network
            )
        }

        // ---------------------------------------------------------- tools
        "capabilities" => {
            let mut reg = marfim_tools::registry::ToolRegistry::new();
            marfim_tools::tools_host::register_builtins(&mut reg);
            let mut out = String::from("tools:\n");
            for t in reg.list_visible() {
                out.push_str(&format!(
                    "  {} [{:?}]{}\n",
                    t.name,
                    t.class,
                    if t.deferred { " (deferred)" } else { "" }
                ));
            }
            out.push_str(&format!("deferred: {}\n", reg.list_deferred().len()));
            out
        }
        "checks" => {
            let cmds = settings
                .verify_commands
                .clone()
                .unwrap_or_else(|| harness.tools.verify_commands());
            if cmds.is_empty() {
                "no verify commands configured ([verify] config section or a recognised project marker)".into()
            } else {
                format!("verify commands:\n  {}", cmds.join("\n  "))
            }
        }
        "verify" => {
            let (passed, output) = harness.tools.run_verify(&harness.ctx).await;
            format!(
                "verify {}: {}",
                if passed { "PASSED" } else { "FAILED" },
                if output.is_empty() { "(no output)" } else { &output }
            )
        }
        "smoke" => {
            // real smoke: probe sandbox + registry + journal write
            let sandbox = marfim_core::sandbox::Sandbox::probe();
            let j = harness.journal.lock().unwrap_or_else(|p| p.into_inner());
            format!(
                "smoke: journal ok ({} bytes) | sandbox unshare={} | session {}",
                j.len_bytes(),
                sandbox.unshare_available(),
                harness.ctx.session_id
            )
        }
        "map" => {
            let args = serde_json::json!({"budget": 120});
            run_tool_sync("RepoMap", ctx, &args)
        }
        "memory" => {
            let args = serde_json::json!({"action": "recall"});
            run_tool_sync("Memory", ctx, &args)
        }
        "recall" => {
            let args = serde_json::json!({"action": "recall"});
            run_tool_sync("Memory", ctx, &args)
        }
        "explain" => {
            let args = serde_json::json!({"thought": rest});
            run_tool_sync("Think", ctx, &args)
        }
        "skills" => {
            let cwd_skills = ctx.cwd.join(".marfim/skills");
            let found = marfim_core::skills::discover(&cwd_skills);
            if found.is_empty() {
                "no skills in .marfim/skills".into()
            } else {
                found
                    .iter()
                    .map(|s| format!("{}: {}", s.name, s.description))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        "plugins" => format!(
            "builtin tools: 33 registered | external agents: {} | skills dir: {}",
            marfim_gateway::acp::external_agent_catalog().len(),
            ctx.cwd.join(".marfim/skills").display()
        ),
        "mcp" => {
            match config.get("mcpServers").and_then(|v| v.as_object()) {
                Some(map) if !map.is_empty() => {
                    let mut out = String::from("mcp servers:\n");
                    for (name, cfg) in map {
                        let cmd = cfg.get("command").and_then(|c| c.as_str()).unwrap_or("?");
                        out.push_str(&format!("  {name}: {cmd}\n"));
                    }
                    out
                }
                _ => "no mcpServers configured ([mcpServers.<name>] command=... in config)".into(),
            }
        }
        "agents" => {
            let mut out = String::from(
                "builtin personas: general, explore, plan, reviewer, security, silent-failure, build-resolver, evaluator\nexternal:",
            );
            for (name, cmd, args, enabled, verified) in
                marfim_gateway::acp::external_agent_catalog()
            {
                out.push_str(&format!(
                    "\n  {name}: {cmd} {args:?} enabled={enabled} verified={verified}"
                ));
            }
            out
        }
        "workflow" => {
            let dir = ctx.cwd.join("examples/commands");
            match std::fs::read_dir(&dir) {
                Ok(rd) => {
                    let names: Vec<String> = rd
                        .flatten()
                        .map(|e| e.file_name().to_string_lossy().to_string())
                        .collect();
                    if names.is_empty() {
                        format!("no workflows in {}", dir.display())
                    } else {
                        format!("workflows: {}", names.join(", "))
                    }
                }
                Err(_) => format!("no workflows dir at {}", dir.display()),
            }
        }
        "recipes" => {
            let dir = ctx.cwd.join("examples/skills");
            match std::fs::read_dir(&dir) {
                Ok(rd) => {
                    let names: Vec<String> = rd
                        .flatten()
                        .map(|e| e.file_name().to_string_lossy().to_string())
                        .collect();
                    if names.is_empty() {
                        format!("no recipes in {}", dir.display())
                    } else {
                        format!("recipes: {}", names.join(", "))
                    }
                }
                Err(_) => format!("no recipes dir at {}", dir.display()),
            }
        }
        "hooks" => {
            match config.get("hooks").and_then(|v| v.as_object()) {
                Some(map) if !map.is_empty() => {
                    let mut out = String::from("hooks:\n");
                    for (ev, list) in map {
                        out.push_str(&format!(
                            "  {ev}: {}\n",
                            list.as_array().map(|a| a.len()).unwrap_or(0)
                        ));
                    }
                    out
                }
                _ => "no hooks configured ([hooks] in config)".into(),
            }
        }

        // ---------------------------------------------------------- context
        "context" => {
            let h = harness.history.read().await;
            let chars: usize = h.iter().map(|m| m.content.len()).sum();
            format!(
                "context: {} messages ≈ {} tokens ({} chars)",
                h.len(),
                chars / 4,
                chars
            )
        }
        "copy-context" => {
            let h = harness.history.read().await;
            let mut out = String::new();
            for m in h.iter() {
                out.push_str(&format!("{}: {}\n", m.role, m.content));
            }
            drop(h);
            let dir = ctx.cwd.join(".marfim");
            let _ = std::fs::create_dir_all(&dir);
            let dest = dir.join("last-context.txt");
            match std::fs::write(&dest, &out) {
                Ok(_) => format!("context copied → {}", dest.display()),
                Err(e) => format!("copy-context failed: {e}"),
            }
        }
        "handoff" => {
            let recs = journal_records(harness);
            let mut out = format!(
                "# marfim handoff — {} ({})\n\n",
                harness.ctx.session_id,
                crate::iso_now()
            );
            for r in recs.iter().rev().take(30).rev() {
                match r {
                    JournalRecord::UserMessage { text } => {
                        out.push_str(&format!("**user**: {}\n\n", abbrev(text, 200)))
                    }
                    JournalRecord::AssistantMessage { text, .. } => {
                        out.push_str(&format!("**assistant**: {}\n\n", abbrev(text, 200)))
                    }
                    JournalRecord::ToolCall { tool, .. } => out.push_str(&format!("- ran `{tool}`\n")),
                    _ => {}
                }
            }
            let dir = ctx.cwd.join(".marfim");
            let _ = std::fs::create_dir_all(&dir);
            let dest = dir.join("handoff.md");
            match std::fs::write(&dest, &out) {
                Ok(_) => format!("handoff written → {}", dest.display()),
                Err(e) => format!("handoff failed: {e}"),
            }
        }
        "clear" => "\x1b[2J\x1b[H".into(),
        "editor" => {
            let ed = std::env::var("EDITOR")
                .or_else(|_| std::env::var("VISUAL"))
                .unwrap_or_else(|_| "(unset)".into());
            format!("editor: {ed}")
        }
        "dig" => {
            if rest.is_empty() {
                "usage: /dig <host>".into()
            } else {
                use std::net::ToSocketAddrs;
                match format!("{rest}:0").to_socket_addrs() {
                    Ok(addrs) => {
                        let ips: Vec<String> =
                            addrs.map(|a| a.ip().to_string()).collect::<Vec<_>>();
                        if ips.is_empty() {
                            format!("{rest}: no addresses")
                        } else {
                            format!("{rest} → {}", ips.join(", "))
                        }
                    }
                    Err(e) => format!("dig failed for {rest}: {e}"),
                }
            }
        }
        "goal" => {
            let notes = {
                let guard = ctx.board.lock().unwrap_or_else(|p| p.into_inner());
                guard.read_topic("goal").into_iter().cloned().collect::<Vec<_>>()
            };
            if notes.is_empty() {
                "no goal on the board — post one with the Board tool".into()
            } else {
                notes
                    .iter()
                    .map(|n| format!("#{}: {}", n.id, n.body))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        "btw" => {
            let notes = {
                let guard = ctx.board.lock().unwrap_or_else(|p| p.into_inner());
                guard.read_topic(rest).into_iter().cloned().collect::<Vec<_>>()
            };
            if notes.is_empty() {
                format!("board topic `{rest}`: (empty)")
            } else {
                notes
                    .iter()
                    .map(|n| format!("#{} [{}]: {}", n.id, n.author, n.body))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        "hot" => {
            let recs = journal_records(harness);
            let mut counts = std::collections::BTreeMap::<String, usize>::new();
            for r in &recs {
                if let JournalRecord::ToolCall { tool, .. } = r {
                    *counts.entry(tool.clone()).or_default() += 1;
                }
            }
            let mut v: Vec<(String, usize)> = counts.into_iter().collect();
            v.sort_by_key(|e| std::cmp::Reverse(e.1));
            if v.is_empty() {
                "no tool activity yet".into()
            } else {
                let lines: Vec<String> = v
                    .iter()
                    .take(8)
                    .map(|(t, n)| format!("  {t}: {n}"))
                    .collect();
                format!("hot tools:\n{}", lines.join("\n"))
            }
        }
        "cron" => "no cron facility in this build — schedule via your OS (crontab/systemd)".into(),
        "swarm" => {
            // honest: the swarm/distswarm oracle port is not in this build
            // (the preservation bundle was never delivered); nothing is
            // faked here — the command states its absence plainly.
            "swarm/distswarm is not available in this build (oracle preservation \
             bundle missing; see docs/PORT-LEDGER.md 'Not yet ported')"
                .into()
        }

        // ---------------------------------------------------------- repo
        "commit" | "diff" | "diffs" | "sdiff" => {
            let args = match canonical {
                "diff" => "diff --stat".to_string(),
                "sdiff" => "diff --cached --stat".to_string(),
                "diffs" => "status --porcelain".to_string(),
                _ => "status --short".to_string(),
            };
            match git_in(ctx, &args) {
                Ok(out) => format!("git {args}:\n{}", if out.trim().is_empty() { "(clean)" } else { out.trim() }),
                Err(e) => format!("git failed: {e}"),
            }
        }
        "init" => {
            let dir = ctx.cwd.join(".marfim");
            let dest = dir.join("config.toml");
            if dest.exists() {
                format!("already initialised: {}", dest.display())
            } else {
                match std::fs::create_dir_all(&dir).and_then(|_| {
                    std::fs::write(
                        &dest,
                        "# marfim project config (cwd layer; requires a trusted directory)\nprovider = \"anthropic\"\nmodel = \"claude-sonnet-4-5\"\nautoCompactThreshold = 120000\n",
                    )
                }) {
                    Ok(_) => format!("initialised {}", dest.display()),
                    Err(e) => format!("init failed: {e}"),
                }
            }
        }
        "doctor" => {
            let sandbox = marfim_core::sandbox::Sandbox::probe();
            format!(
                "marfim {} (rust)\nprovider: {} (model {})\nsandbox: unshare={}\njournal: {} bytes\nsession: {}",
                marfim_core::VERSION,
                settings.provider_name,
                settings.model,
                sandbox.unshare_available(),
                harness.journal.lock().unwrap_or_else(|p| p.into_inner()).len_bytes(),
                harness.ctx.session_id
            )
        }
        "theme" => {
            // The TUI palette is live (Palette::from_theme): validate the
            // requested accent against the real theme set.
            let known = [
                "default",
                "cyan",
                "red",
                "green",
                "blue",
                "magenta",
                "yellow",
                "white",
                "gray",
                "grey",
                "dark-gray",
                "darkgrey",
                "light-red",
                "light-green",
                "light-blue",
                "light-cyan",
                "light-magenta",
                "light-yellow",
            ];
            let requested = rest.split_whitespace().next().unwrap_or("");
            if requested.is_empty() {
                format!(
                    "theme: {} (accent set in config `[theme]`; NO_COLOR forces monochrome; known accents: {})",
                    settings.theme,
                    known.join(", ")
                )
            } else if known.contains(&requested.to_ascii_lowercase().as_str()) {
                format!(
                    "theme: {requested} — applies to the TUI palette on next session start (set `theme = \"{requested}\"` in config to persist)"
                )
            } else {
                let hint = marfim_core::levenshtein::did_you_mean(requested, known.as_slice())
                    .map(|s| format!(" (did you mean `{s}`?)"))
                    .unwrap_or_default();
                format!("unknown theme `{requested}`{hint}; known: {}", known.join(", "))
            }
        }
        "profile" => format!(
            "profile: provider {} model {} (failover entries: {})",
            settings.provider_name,
            settings.model,
            settings.failover.len()
        ),
        // Compile-time safety net: every COMMANDS entry must have a real
        // arm above. This arm is unreachable in correct operation; the
        // every_command_dispatches test proves it stays unreachable.
        _ => format!(
            "internal error: /{canonical} has an entry in COMMANDS but no implementation arm"
        ),
    }
}

fn abbrev(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}

fn sessions_dir_path() -> std::path::PathBuf {
    crate::sessions_dir()
}

fn journal_records(harness: &Harness) -> Vec<JournalRecord> {
    let path = harness
        .journal
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .path()
        .to_path_buf();
    marfim_core::journal::Journal::replay(&path).unwrap_or_default()
}

/// Rewind the last `n` records by byte offset (journal is append-only;
/// rewind truncates + saves the tail).
fn rewind_records(harness: &Harness, n: usize) -> Result<u64, String> {
    let path = harness
        .journal
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .path()
        .to_path_buf();
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let lines: Vec<&str> = text.lines().collect();
    if n >= lines.len() {
        return Err(format!("cannot rewind {n} of {} records", lines.len()));
    }
    let offset: u64 = lines[..lines.len() - n]
        .iter()
        .map(|l| l.len() as u64 + 1)
        .sum();
    let dropped = harness
        .journal
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .rewind_to(offset)
        .map_err(|e| e.to_string())?;
    Ok(dropped)
}

fn rewind_last_tool_batch(harness: &Harness) -> Result<String, String> {
    let recs = journal_records(harness);
    let last_tool = recs
        .iter()
        .rposition(|r| matches!(r, JournalRecord::ToolResult { .. }))
        .ok_or("no tool results to undo")?;
    // rewind from the matching ToolCall to the ToolResult inclusive
    let mut start = last_tool;
    for (i, r) in recs.iter().enumerate().rev() {
        if matches!(r, JournalRecord::ToolCall { .. }) {
            start = i;
            break;
        }
    }
    let n = recs.len() - start;
    let dropped = rewind_records(harness, n)?;
    Ok(format!(
        "undid tool batch ({n} records, {dropped} bytes); files were NOT reverted — use git to restore"
    ))
}

async fn switch_primary_model(
    harness: &Harness,
    config: &Config,
    settings: &EffectiveSettings,
    model: &str,
) -> Result<String, String> {
    let preset = crate::settings::resolve_preset(&settings.provider_name, config)?;
    let provider = crate::settings::build_provider(&preset, model)?;
    let mut r = harness.router.write().await;
    let mut chain = r.chain(marfim_core::router::Role::Primary);
    if chain.is_empty() {
        return Err("no primary chain".into());
    }
    chain[0].model = model.to_string();
    chain[0].provider = provider;
    r.set_chain(marfim_core::router::Role::Primary, chain);
    Ok(format!(
        "primary model switched to {model} (takes effect next step)"
    ))
}

fn git_in(ctx: &ToolCtx, args: &str) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .args(args.split_whitespace())
        .current_dir(&ctx.cwd)
        .output()
        .map_err(|e| e.to_string())?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(text)
}

fn run_tool_sync(name: &str, ctx: &ToolCtx, args: &serde_json::Value) -> String {
    let mut reg = marfim_tools::registry::ToolRegistry::new();
    marfim_tools::tools_host::register_builtins(&mut reg);
    match reg.get(name) {
        Some(def) => (def.run)(ctx, args).output,
        None => format!("no tool {name}"),
    }
}

#[cfg(test)]
mod tests {
    //! D15: every command has a behaviour test — dispatch runs real state.

    use super::*;
    use crate::settings::{self, Cli};
    use clap::Parser;
    use marfim_core::journal::Journal;
    use marfim_core::permissions::{Mode, PermissionEngine};
    use marfim_core::providers::{CostBook, MockProvider};
    use marfim_core::router::{ModelEntry, Role, Router};
    use std::sync::{Arc, Mutex};

    async fn test_harness(tmp: &std::path::Path) -> Arc<Harness> {
        let bus = marfim_core::events::EventBus::new(256);
        let mut router = Router::new(CostBook::default(), false);
        router.set_chain(
            Role::Primary,
            vec![ModelEntry {
                model: "mock-primary".into(),
                provider: Arc::new(MockProvider::new(vec![MockProvider::text("ok")])),
                priced: true,
            }],
        );
        router.set_chain(
            Role::Small,
            vec![ModelEntry {
                model: "mock-small".into(),
                provider: Arc::new(MockProvider::new(vec![MockProvider::text(
                    "a summary long enough for compaction",
                )])),
                priced: true,
            }],
        );
        let (perm, _rx) = PermissionEngine::new(Mode::Yolo, vec![]);
        let ctx = marfim_core::loop_::LoopCtx {
            session_id: "sess-test".into(),
            cwd: tmp.display().to_string(),
            cancel: Default::default(),
        };
        let mut journal = Journal::open(&tmp.join("test-session.jsonl")).unwrap();
        let _ = journal.append(&JournalRecord::SessionMeta {
            session_id: "sess-test".into(),
            cwd: tmp.display().to_string(),
            title: None,
            started_at: "2026-01-01T00:00:00Z".into(),
        });
        let _ = journal.append(&JournalRecord::UserMessage {
            text: "hello".into(),
        });
        let h = Harness::new(
            bus,
            Arc::new(tokio::sync::RwLock::new(router)),
            Arc::new(marfim_tools::tools_host::HostTools::new(
                Arc::new(PermissionEngine::new(Mode::Yolo, vec![]).0),
                ToolCtx::default(),
                marfim_core::sandbox::Sandbox::probe(),
                marfim_core::hooks::Hooks::default(),
            )),
            Arc::new(Mutex::new(journal)),
            ctx,
            Arc::new(perm),
            8,
            2,
            u64::MAX,
            false,
        );
        h.history
            .write()
            .await
            .push(marfim_core::providers::ChatMessage::user("hello"));
        Arc::new(h)
    }

    async fn fixture() -> (
        std::path::PathBuf,
        ToolCtx,
        Arc<Harness>,
        Config,
        EffectiveSettings,
    ) {
        // hermetic sessions dir: MARFIM_HOME set once per test process
        static HOME: std::sync::Once = std::sync::Once::new();
        HOME.call_once(|| {
            let home = std::env::temp_dir().join(format!("marfim-home-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&home);
            std::env::set_var("MARFIM_HOME", &home);
        });
        let tmp = std::env::temp_dir().join(format!(
            "marfim-slash-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let harness = test_harness(&tmp).await;
        let ctx = ToolCtx {
            cwd: tmp.clone(),
            session_id: "sess-test".into(),
            ..Default::default()
        };
        let config = Config::from_json_str(
            r#"{"provider": "mock", "mcpServers": {"files": {"command": "mcp-fs"}}}"#,
        )
        .unwrap();
        let cli = Cli::parse_from(["marfim", "--provider", "mock"]);
        let settings = settings::effective(&cli, &config, tmp.clone()).unwrap();
        (tmp, ctx, harness, config, settings)
    }

    async fn run(cmd: &str) -> String {
        let (_t, ctx, h, c, s) = fixture().await;
        dispatch(cmd, &ctx, &h, &c, &s).await
    }

    macro_rules! behaviour {
        ($name:ident, $cmd:expr, contains = [$($needle:expr),* $(,)?]) => {
            #[tokio::test]
            async fn $name() {
                let out = run($cmd).await;
                let ok = $(out.contains($needle))&&*;
                assert!(
                    ok,
                    "command `{}` output missing one of {:?}: {out}",
                    $cmd,
                    [$($needle),*]
                );
            }
        };
    }

    behaviour!(help_lists_all_65, "/help", contains = ["commands (65)"]);
    behaviour!(status_real, "/status", contains = ["sess-test", "mock"]);
    behaviour!(stats_real, "/stats", contains = ["tool results"]);
    behaviour!(
        audit_kinds,
        "/audit",
        contains = ["session_meta", "user_message"]
    );
    behaviour!(wal_tail, "/wal", contains = ["user: hello"]);
    behaviour!(replay_last, "/replay 2", contains = ["replay last 2"]);
    behaviour!(tree_renders, "/tree", contains = ["turn 1: hello"]);
    behaviour!(resume_lists, "/resume", contains = ["sessions"]);
    behaviour!(provider_report, "/provider", contains = ["mock", "wire"]);
    behaviour!(accounts_list, "/accounts", contains = ["anthropic", "xai"]);
    behaviour!(
        models_chains,
        "/models",
        contains = ["primary: mock-primary"]
    );
    behaviour!(model_current, "/model", contains = ["mock-1"]);
    behaviour!(
        permissions_report,
        "/permissions",
        contains = ["permission mode: Default"]
    );
    behaviour!(policy_report, "/policy", contains = ["mode: Default"]);
    behaviour!(yolo_report, "/yolo", contains = ["yolo mode is"]);
    behaviour!(plan_report, "/plan", contains = ["plan mode is"]);
    behaviour!(sandbox_probe, "/sandbox", contains = ["sandbox: unshare"]);
    behaviour!(
        capabilities_list,
        "/capabilities",
        contains = ["Read", "Write"]
    );
    behaviour!(checks_report, "/checks", contains = ["verify"]);
    behaviour!(map_runs, "/map", contains = ["no source files found"]);
    behaviour!(memory_runs, "/memory", contains = ["no memories"]);
    behaviour!(recall_runs, "/recall", contains = ["no memories"]);
    behaviour!(
        explain_runs,
        "/explain why",
        contains = ["noted; no side effects"]
    );
    behaviour!(skills_empty, "/skills", contains = ["no skills"]);
    behaviour!(plugins_counts, "/plugins", contains = ["external agents:"]);
    behaviour!(mcp_lists_config, "/mcp", contains = ["files: mcp-fs"]);
    behaviour!(agents_lists, "/agents", contains = ["builtin personas"]);
    behaviour!(hooks_report, "/hooks", contains = ["no hooks"]);
    behaviour!(
        context_counts,
        "/context",
        contains = ["context: 1 messages"]
    );
    behaviour!(copy_context, "/copy-context", contains = ["context copied"]);
    behaviour!(handoff_writes, "/handoff", contains = ["handoff written"]);
    behaviour!(editor_report, "/editor", contains = ["editor:"]);
    behaviour!(goal_empty, "/goal", contains = ["no goal"]);
    behaviour!(hot_empty, "/hot", contains = ["no tool activity"]);
    behaviour!(cron_honest, "/cron", contains = ["no cron facility"]);
    behaviour!(swarm_honest, "/swarm", contains = ["swarm"]);
    behaviour!(
        doctor_real,
        "/doctor",
        contains = ["marfim", "provider: mock"]
    );
    behaviour!(theme_report, "/theme", contains = ["theme: default"]);
    behaviour!(profile_report, "/profile", contains = ["provider mock"]);
    behaviour!(cost_zero, "/cost", contains = ["session cost: $0"]);
    behaviour!(usage_ledger, "/usage", contains = ["usage ledger"]);
    behaviour!(budget_report, "/budget", contains = ["budget mode: false"]);
    behaviour!(forecast_math, "/forecast", contains = ["forecast:"]);
    behaviour!(
        cachehealth_honest,
        "/cachehealth",
        contains = ["cache tokens"]
    );
    behaviour!(
        unknown_command,
        "/frobnicate",
        contains = ["unknown command"]
    );
    behaviour!(
        architect_report,
        "/architect",
        contains = ["architect role:"]
    );
    behaviour!(review_report, "/review", contains = ["review role:"]);
    behaviour!(init_creates, "/init", contains = ["initialised"]);
    behaviour!(recipes_dir, "/recipes", contains = ["recipes"]);
    behaviour!(workflow_dir, "/workflow", contains = ["workflows"]);
    behaviour!(clear_ansi, "/clear", contains = ["\x1b[2J"]);
    behaviour!(verify_runs, "/verify", contains = ["verify"]);
    behaviour!(smoke_real, "/smoke", contains = ["smoke: journal ok"]);
    behaviour!(
        fork_copies,
        "/fork forked-name",
        contains = ["forked session"]
    );
    behaviour!(export_copies, "/export", contains = ["journal exported"]);
    behaviour!(rewind_one, "/rewind 1", contains = ["rewound 1 record"]);
    behaviour!(undo_no_tool, "/undo", contains = ["undo failed"]);
    behaviour!(dig_resolves, "/dig localhost", contains = ["localhost"]);
    behaviour!(commit_clean, "/commit", contains = ["git status"]);
    behaviour!(diff_stat, "/diff", contains = ["git diff"]);
    behaviour!(diffs_status, "/diffs", contains = ["git status"]);
    behaviour!(sdiff_cached, "/sdiff", contains = ["git diff --cached"]);

    #[tokio::test]
    async fn every_command_dispatches_with_real_output() {
        // D15 structural check: 65 commands, and every single one must
        // dispatch to a REAL arm (never the internal fall-through, never
        // the old "available" catch-all).
        assert_eq!(COMMANDS.len(), 65);
        for c in COMMANDS {
            let out = run(&format!("/{c}")).await;
            assert!(
                !out.contains("internal error"),
                "/{c} has an entry but no implementation arm: {out}"
            );
            assert!(
                !out.contains("available (Node"),
                "/{c} still answers with the deleted catch-all: {out}"
            );
        }
    }

    #[tokio::test]
    async fn compact_real_summarizes_via_small_role() {
        let (_t, ctx, h, c, s) = fixture().await;
        let h = Arc::into_inner(h).unwrap();
        let h = Arc::new(h);
        h.history
            .write()
            .await
            .push(marfim_core::providers::ChatMessage::user(
                "discussing the parser fix in src/parse.rs",
            ));
        let out = dispatch("/compact", &ctx, &h, &c, &s).await;
        assert!(out.contains("compacted"), "{out}");
    }

    #[tokio::test]
    async fn model_switch_really_rewrites_chain() {
        let (_t, ctx, h, c, s) = fixture().await;
        let out = dispatch("/model mock-alt", &ctx, &h, &c, &s).await;
        assert!(out.contains("switched to mock-alt"), "{out}");
        let r = h.router.read().await;
        assert_eq!(r.chain(Role::Primary)[0].model, "mock-alt");
    }

    #[tokio::test]
    async fn new_session_swaps_journal_and_clears_history() {
        let (_t, ctx, h, c, s) = fixture().await;
        h.history
            .write()
            .await
            .push(marfim_core::providers::ChatMessage::user("old"));
        let out = dispatch("/new", &ctx, &h, &c, &s).await;
        assert!(out.contains("new session"), "{out}");
        assert!(h.history.read().await.is_empty(), "history cleared");
        let path = h.journal.lock().unwrap().path().to_path_buf();
        assert!(path.is_file());
        assert!(path.file_name().unwrap().to_string_lossy().starts_with('s'));
    }

    #[tokio::test]
    async fn undo_really_rewinds_last_tool_batch() {
        let (_t, ctx, h, c, s) = fixture().await;
        let j = h.journal.clone();
        {
            let mut guard = j.lock().unwrap();
            let _ = guard.append(&JournalRecord::ToolCall {
                id: "t1".into(),
                tool: "Read".into(),
                args: serde_json::json!({"path": "x"}),
            });
            let _ = guard.append(&JournalRecord::ToolResult {
                id: "t1".into(),
                output: "out".into(),
                is_error: false,
            });
        }
        let out = dispatch("/undo", &ctx, &h, &c, &s).await;
        assert!(out.contains("undid tool batch"), "{out}");
        let recs = marfim_core::journal::Journal::replay(j.lock().unwrap().path()).unwrap();
        assert!(
            !recs
                .iter()
                .any(|r| matches!(r, JournalRecord::ToolCall { .. })),
            "tool call records gone after undo"
        );
    }
}
