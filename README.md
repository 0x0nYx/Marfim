# Marfim — one harness, every discipline

**Marfim** is a unified, clean-room coding-agent harness for the terminal. It was designed
after a forensic analysis of ten state-of-the-art agent codebases, adopting the strongest
*verified* discipline from each — no copied code, only proven ideas re-implemented from
scratch (see [docs/CLEAN-ROOM.md](docs/CLEAN-ROOM.md) and [docs/forensic/](docs/forensic/)).

```
◆ Marfim v0.1.0 — one harness, every discipline
Type /help for commands · Ctrl-C interrupts · Ctrl-D exits
```

## Lineage — what Marfim takes from each predecessor

| Source | What Marfim adopted |
|---|---|
| **opencode v2** | models.dev catalog with offline cache; free-model surfacing; layered config; session fork/export; usage + cost accounting; provider/model ids `provider/model` |
| **Codex CLI** | `apply_patch` DSL; execpolicy deny rules; sandbox backends (bubblewrap/seatbelt); honest headless exit codes; AGENTS.md walk; JSONL date-sharded rollouts; profiles-ready config |
| **Goose** | "undecidable → ask" permission fallback; explicit read-only tool tier; tool output boundedness |
| **Aider** | fuzzy search/replace match ladder (exact → rstrip → outdent → ellipsis → nearest); reflection cap; guarded `/undo` (only Marfim-made commits); repo map; weak-model routing slots |
| **Claude Code** | permission rule syntax `Tool(pattern)` with allow/ask/deny; permission modes (default/plan/accept-edits/bypass); hooks (PreToolUse/PostToolUse/…) with exit-2 veto; file checkpoints + `/rewind`; headless `stream-json`; CLAUDE.md memory compatibility |
| **Hermes Agent** | prompt-cache discipline (byte-stable system prompt sections); `/doctor`; honest security model ("the sandbox is not a security boundary"); usage ledger per session |
| **Oh My Pi** | model roles (plan/commit/task/smol); steering queue delivered at safe step boundaries; cross-harness dir compatibility (.claude/, .codex/, AGENTS.md); skills progressive disclosure |
| **dsh-superagent** | durable session records (store-first persistence); explicit `is_error` channel on every tool result; anti-injection posture in the system prompt (untrusted content is data) |
| **REA** | summary-first bounded tool results with explicit `[truncated]` markers; evidence-first editing (read before edit) |
| **ECC** | config-protection (the model cannot weaken the harness's own guardrails); GateGuard (edits refuse unread files); research-first + verify-your-work behavioral contract; skills-first context economics |

## Install

```bash
pip install -e .        # zero runtime dependencies, Python 3.11+
marfim doctor           # environment check
```

## Quick start

```bash
# 1. pick any provider (20 presets; keys come from the standard env vars)
export OPENROUTER_API_KEY=sk-or-...
marfim models free                  # browse zero-cost models (models.dev catalog)
marfim --provider openrouter --model deepseek/deepseek-chat-v3.1:free

# 2. chat
marfim                              # interactive REPL
marfim -p "explain this repo"       # headless one-shot
marfim -p "fix the failing test" -y --output-format json   # autonomous + JSON

# 3. sessions
marfim session list
marfim --resume last
```

### Example: wire up MCP servers + hooks + roles (`.marfim/config.toml`)

```toml
default_provider = "groq"
default_model    = "llama-3.3-70b-versatile"

[model_roles]
plan  = "openai/o4-mini"
task  = "groq/llama-3.3-70b-versatile"

[permissions]
mode  = "accept-edits"
allow = ["Bash(git *)", "Bash(pytest *)"]
deny  = ["Bash(sudo *)", "Bash(rm -rf *)"]

[mcp_servers.filesystem]
command = "npx"
args    = ["-y", "@modelcontextprotocol/server-filesystem", "."]

[hooks]
PreToolUse = ["python3 scripts/audit-tool.py"]   # exit 2 vetoes the tool call

[sandbox]
mode = "bubblewrap"          # or "seatbelt" on macOS, "none" to disable
```

## Feature surface

- **Providers** — OpenAI-compatible + Anthropic wire protocols (SSE streaming, tool calls,
  usage accounting), 20 presets incl. local (Ollama/LM Studio/vLLM), live models.dev
  catalog (235 providers / 8,935 models) with 24h cache and offline fallback, free-model
  onboarding, custom `base_url` providers, mock provider for tests/CI.
- **Agent loop** — streaming turns, tool dispatch, exponential-backoff retries (auth errors
  never retried), reflection-capped failures, auto-compaction (threshold-of-window with
  verbatim tail) + manual `/compact`, mid-turn steering, subagents (build/plan/explore/review)
  with depth limits and optional git-worktree isolation.
- **Tools** — read, write, edit (fuzzy ladder + GateGuard), glob, grep (ripgrep), bash
  (timeouts, background jobs, sandbox wrap, execpolicy), patch (apply_patch DSL), todo,
  repomap, webfetch, websearch, skill, task, + every tool from your MCP servers
  (`mcp__server__tool`).
- **Permissions** — modes, ordered deny→protect→allow→ask rules, session "always" grants,
  plan-mode read-only enforcement, config-protection, ECC-style protected files.
- **Memory & skills** — MARFIM.md/AGENTS.md/CLAUDE.md hierarchy (root→cwd walk), SKILL.md
  progressive disclosure, custom `!commands` from markdown with `$ARGUMENTS`.
- **Sessions** — JSONL date-sharded rollouts, resume/fork/export/rm, checkpoints with
  `/rewind`, usage + estimated cost per session.
- **UX** — REPL with streaming output, markdown-lite rendering, ~24 slash commands,
  headless mode with `text|json|stream-json`, `marfim doctor`, `marfim map`.

## Testing

```bash
python3 -m unittest discover -s tests     # 44 tests: config, permissions, tools,
                                          # patch DSL, agent loop (mock provider),
                                          # MCP JSON-RPC, wire protocols (canned SSE)
```

## Repository layout

```
marfim/                  the harness (Python, stdlib-only core)
  providers/             wire protocols + models.dev registry + mock
  agent/                 loop, prompts, compaction
  tools/                 13 built-in tools + MCP bridge
  ui/                    REPL, slash commands, headless
  mcp/                   MCP stdio client (JSON-RPC)
  session/               JSONL session store + checkpoints
docs/
  forensic/              11 forensic dossiers of the analyzed codebases (3,084 lines)
  FEATURE-MATRIX.md      capability-by-source synthesis matrix
  ARCHITECTURE.md        how Marfim is built
  ROADMAP.md             what comes next (LSP/DAP, CodeMode, mandates, ...)
  CLEAN-ROOM.md          methodology + legal notice
examples/                config.toml, skill, custom command, hook script
tests/                   44 unittest cases
```

## License

MIT — see [LICENSE](LICENSE). Marfim shares no code with any analyzed project; the
forensic dossiers describe publicly observable behavior and cite file paths for review.
