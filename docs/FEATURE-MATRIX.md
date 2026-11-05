# Marfim Feature Matrix — capability-by-source synthesis

> **1.0.0:** rows marked 🦢✅ are implemented in the Rust workspace
> (see docs/PORT-LEDGER.md for the honest, fixture-verified port status
> of the intelligence modules). The Python column is historical (the
> Python harness is gone; the oracle tree is preserved under the
> archive/ tags).

Every row is a Marfim capability. "Source" names the harness(es) whose *verified
behavior* it synthesizes (clean-room; see CLEAN-ROOM.md). "Status" reflects what
ships in v0.1.0. Forensic evidence for each source claim: `docs/forensic/<name>.md`.

## 0. Rust parity snapshot (v0.3.0)

| Surface | Rust status |
|---|---|
| 34 tools (Node v1.2.0 names) | 🦢✅ (`marfim-tools`, AstQuery/Eval/JsModuleGraph honestly gated on external binaries) |
| 65 slash commands + aliases | 🦢✅ (`marfim` REPL) |
| Headless text/json/stream-json | 🦢✅ (e2e-tested against the binary) |
| OpenAI-compat + Anthropic wires + mock | 🦢✅ (differential-tested vs Node goldens) |
| Router roles + failover + fail-closed pricing | 🦢✅ |
| Permissions (deny>ask>allow>mode, argv-level shell analysis, single queue) | 🦢✅ |
| Hooks (PreToolUse fail-closed) | 🦢✅ |
| Sandbox (env clean, process-group kill, unshare probe) | 🦢✅ |
| Session journal (append-only, fsync, rewind w/ audit tail) | 🦢✅ |
| Strict config (unknown key + did-you-mean) | 🦢✅ |
| Skills / memory (tighten-only) / guardian / evidence / board / trust | 🦢✅ |
| MCP client + server | 🦢✅ |
| serve (HTTP+SSE) / ACP agent+client / LSP | 🦢✅ |
| External agents (opencode, hermes) | 🦢✅ profiles shipped `unverified=true enabled=false` pending owner binaries |
| Google / Zen provider dialects | 🦢 partial (OpenAI-compat covers compatible endpoints; documented in the provider presets) |
| CDP devtools browser tools | 🦢 deferred to app wave (PARITY §10.4) |

Every row is a Marfim capability. "Source" names the harness(es) whose *verified
behavior* it synthesizes (clean-room; see CLEAN-ROOM.md). "Status" reflects what
ships in v0.1.0. Forensic evidence for each source claim: `docs/forensic/<name>.md`.

## 1. Model & provider layer

| Capability | Source doctrine | Status |
|---|---|---|
| OpenAI-compatible wire protocol (SSE, tool calls, usage) | opencode (45 protocols), Codex `wire_api` | ✅ |
| Anthropic Messages wire protocol (blocks, tool_use, cache tokens) | opencode, claude code | ✅ |
| models.dev catalog sync + 24h disk cache + offline fallback | opencode v2 catalog/KV cache | ✅ (235 providers / 8,935 models) |
| Free-model surfacing for onboarding | opencode free-model doctrine | ✅ (`marfim models free`) |
| Custom providers via `base_url` + `api_key_env` | Codex `model_providers` | ✅ |
| 20 provider presets incl. local Ollama/LM Studio/vLLM | goose (~40 providers), omp | ✅ |
| Model roles (plan/commit/task/smol) routed per call | Oh My Pi 16 roles | ✅ (5 roles, task/plan routing live) |
| Weak-model slots for summaries/commits | aider weak-model routing | ◐ (roles exist; auto-routing v0.2) |
| Mock provider for deterministic tests | opencode `simulation` backend | ✅ |
| Prompt-cache discipline: byte-stable system prompt sections | hermes invariant law | ✅ (fixed section order) |

## 2. Agent loop

| Capability | Source doctrine | Status |
|---|---|---|
| Streaming turn loop with tool dispatch | all | ✅ |
| Retries w/ exponential backoff; never retry 401/403/404/422 | Codex retry ladder | ✅ |
| Reflection cap on repeated failures | aider (3) | ✅ |
| Auto-compaction at % of context window, verbatim tail keep | goose 0.8/225k, opencode | ✅ |
| Manual `/compact` sharing the same code path | codex pre-sampling + /compact | ✅ |
| Deterministic tool-result pruning before summarization | hermes layered compression | ✅ |
| Mid-turn steering delivered at safe step boundaries | opencode v2 inbox, omp live steering | ✅ (between tool batches) |
| Subagents with roles + depth limit | claude code, hermes delegate, omp | ✅ |
| Subagent git-worktree isolation | omp CoW isolation (full CoW backends v0.2+) | ◐ |
| Per-turn usage + cost ledger | goose usage ledger, opencode stats | ✅ |
| Interrupt (Ctrl-C) races the loop | goose effect machine cancellation | ✅ |

## 3. Tools

| Capability | Source doctrine | Status |
|---|---|---|
| read / write / edit / glob / grep (ripgrep fallback) | all | ✅ |
| Edit must-follow-read (GateGuard) | ECC GateGuard | ✅ |
| Fuzzy edit ladder: exact→rstrip→outdent→ellipsis→nearest | aider editblock matcher | ✅ |
| apply_patch multi-file DSL with @@ anchors | codex apply_patch | ✅ |
| bash with timeout, background jobs + log polling | claude code background shells | ✅ |
| execpolicy deny-prefix rules | codex Starlark execpolicy (simplified) | ✅ |
| Sandbox wrappers: bubblewrap / seatbelt / none | codex sandbox manager | ✅ |
| webfetch (HTML→text) + websearch | claude code, ECC research-first | ✅ |
| todo list persisted in session | claude code TodoWrite | ✅ |
| Repo map (symbols, relevance-ranked, budget) | aider repomap (PageRank/tree-sitter = v0.2) | ◐ |
| MCP stdio client; MCP tools appear natively (`mcp__srv__tool`) | claude code, goose MCP-first, omp | ✅ |
| Summary-first bounded outputs with `[truncated]` markers | REA evidence records | ✅ |

## 4. Permissions & safety

| Capability | Source doctrine | Status |
|---|---|---|
| Modes: default / plan / accept-edits / bypass | claude code permission modes | ✅ |
| Rules `Tool(pattern)` allow/ask/deny, deny wins | claude code + goose inspectors | ✅ |
| Undecidable → ask fallback | goose | ✅ |
| Session "always allow" grants persisted in session | omp always-ask-cached | ✅ |
| Plan mode = read-only incl. readonly-bash allowlist | codex read-only sandbox | ✅ |
| Config-protection: model cannot edit harness control files | ECC config-protection hook | ✅ (native) |
| Honest "sandbox ≠ security boundary" docs | hermes SECURITY.md | ✅ |
| Headless: ask → deny (no silent approvals) | claude code headless | ✅ |

## 5. Memory, skills, extensibility

| Capability | Source doctrine | Status |
|---|---|---|
| MARFIM.md / AGENTS.md / CLAUDE.md hierarchy (root→cwd walk) | codex AGENTS.md, claude code memory | ✅ |
| SKILL.md skills w/ YAML frontmatter, progressive disclosure | claude code / ECC / omp | ✅ |
| `.claude/skills` + `.claude/commands` compatibility dirs | omp cross-harness reuse | ✅ |
| Custom markdown commands with `$ARGUMENTS` (`!name` syntax) | claude code / omp commands | ✅ |
| Hooks: SessionStart/PreToolUse/PostToolUse/PreCompact/Stop; exit-2 veto | claude code hooks, ECC hook graph | ✅ |
| MCP server config in TOML + `marfim mcp add/test` | all | ✅ |
| Session fork/export/rm | opencode session UX | ✅ |
| Checkpoints + `/rewind` file restore | claude code checkpoints | ✅ |

## 6. UX & operations

| Capability | Source doctrine | Status |
|---|---|---|
| Interactive REPL, streaming markdown-lite rendering | all | ✅ |
| ~24 slash commands | claude code (~50), omp (~100) subset chosen deliberately | ✅ |
| Headless `-p` with text/json/stream-json + honest exit codes | claude code headless | ✅ |
| `marfim doctor` environment checks | hermes/claude code doctor | ✅ |
| `marfim models sync/free/list` | opencode models UX | ✅ |
| Guarded `/undo` (only Marfim-made commits) | aider | ✅ |
| Config layering: defaults < global TOML < project TOML < env < CLI | codex/opencode/aider | ✅ |

## 7. Deliberately deferred (documented in ROADMAP.md)

LSP/DAP integration (oh-my-pi), CodeMode tool-as-code interpreter (opencode v2),
durable inbox sessions w/ SQLite projections (opencode v2), Starlark execpolicy +
network enforcement proxy (codex), LLM permission judge (goose), learning loop +
instincts (hermes/ECC), proactive GitHub mandates (dsh-superagent), REA evidence
bridge, recipe/workflow system (goose), voice (aider/hermes), ACP surfaces (goose/hermes).
