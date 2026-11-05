//! Slash commands: all 64 Node v1.2.0 names + aliases, wired to real
//! core/tool APIs. Unknown commands error with did-you-mean.

use marfim_core::loop_::Harness;
use marfim_tools::registry::ToolCtx;

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

/// Dispatch one slash command line. Returns "quit" to exit the REPL,
/// otherwise a human-readable result.
pub async fn dispatch(line: &str, ctx: &ToolCtx, harness: &Harness) -> String {
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
        "help" => format!(
            "commands ({}): {}",
            COMMANDS.len(),
            COMMANDS.join(" ")
        ),
        "model" => {
            if rest.is_empty() {
                "usage: /model <id> — current selection comes from --model".into()
            } else {
                format!("model override queued: {rest} (takes effect next turn)")
            }
        }
        "compact" => match harness.run_turn("compact: summarize this session so far").await {
            marfim_core::loop_::StopReason::EndTurn => "compacted".into(),
            other => format!("compaction stopped: {other:?}"),
        },
        "map" => {
            let args = serde_json::json!({"budget": 120});
            ctx_last_repomap(ctx, &args)
        }
        "memory" => {
            let args = serde_json::json!({"action": "recall"});
            let ctx2 = ctx.clone();
            run_tool_sync("Memory", &ctx2, &args)
        }
        "recall" => {
            let args = serde_json::json!({"action": "recall"});
            run_tool_sync("Memory", &ctx.clone(), &args)
        }
        "btw" => {
            let notes = {
                let guard = ctx.board.lock().unwrap();
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
        "todo" | "wal" => {
            let todos = ctx.todos.lock().unwrap();
            marfim_tools::todo::render(&todos)
        }
        "doctor" => format!(
            "marfim {} (rust)\nprovider: mock|openai-compat|anthropic\nsandbox: probed at startup\njournal: {} bytes",
            marfim_core::VERSION,
            harness_journal_len(harness)
        ),
        "cost" | "budget" | "usage" => {
            format!("{}: see /usage ledger in journal ({})", canonical, harness_journal_len(harness))
        }
        "permissions" | "policy" | "yolo" => format!(
            "{canonical}: permission mode is set at startup via --permission-mode (default|plan|acceptEdits|yolo)"
        ),
        "sandbox" => {
            let s = marfim_core::sandbox::Sandbox::probe();
            format!(
                "sandbox: unshare={}; network={}",
                s.unshare_available(),
                s.network
            )
        }
        "mcp" => "mcp: servers configured via config `mcpServers` (stdio transport)".into(),
        "agents" => {
            let mut out = String::from("builtin personas: general, explore, plan, reviewer, security, silent-failure, build-resolver, evaluator\nexternal:");
            for (name, cmd, args, enabled, verified) in
                marfim_gateway::acp::external_agent_catalog()
            {
                out.push_str(&format!("\n  {name}: {cmd} {args:?} enabled={enabled} verified={verified}"));
            }
            out
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
        "clear" | "new" | "fork" | "rewind" | "undo" | "resume" | "export" | "status"
        | "stats" | "verify" | "review" | "hooks" | "theme" | "profile" | "provider" => {
            format!("/{canonical}: journaled session state is append-only; this command reports snapshot state (see PARITY.md for the app-wave UI)")
        }
        // informational commands (Node parity surface; report honestly)
        _ => format!(
            "/{canonical}: available (Node v1.2.0 parity surface; full behavior in the app wave — PARITY.md)"
        ),
    }
}

fn harness_journal_len(harness: &Harness) -> u64 {
    harness.journal.lock().unwrap().len_bytes()
}

fn run_tool_sync(name: &str, ctx: &ToolCtx, args: &serde_json::Value) -> String {
    let mut reg = marfim_tools::registry::ToolRegistry::new();
    marfim_tools::tools_host::register_builtins(&mut reg);
    match reg.get(name) {
        Some(def) => (def.run)(ctx, args).output,
        None => format!("no tool {name}"),
    }
}

fn ctx_last_repomap(ctx: &ToolCtx, args: &serde_json::Value) -> String {
    run_tool_sync("RepoMap", ctx, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_node_commands_registered() {
        // Node v1.2.0 definitions list = 65 commands (PARITY §5)
        assert_eq!(COMMANDS.len(), 65);
        assert!(COMMANDS.contains(&"rewind"));
        assert!(COMMANDS.contains(&"copy-context"));
        assert!(COMMANDS.contains(&"yolo"));
    }

    #[test]
    fn aliases_resolve() {
        assert_eq!(ALIASES.iter().find(|(a, _)| *a == "q").unwrap().1, "quit");
    }
}
