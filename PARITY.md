# PARITY.md — Marfim Rust rewrite checkpoint

Ground truth: Node v1.2.0 (`archive/node-v1.2.0`, extracted from
`marfim-full.bundle`). Every line below is a parity item. Status markers:

- `[x]` ported + tested
- `[~]` ported, partial / narrowed scope (noted)
- `[ ]` not started
- `[≠]` intentional divergence (bug-compat rejected) — reason given

Session resume rule: read this file first, continue from lowest unfinished
section. Backup: `/home/z/my-project/marfim-full.bundle` (full history,
all refs, sha1).

**Status after the 2026-11-05 wave: 170 tests green across 5 crates
(marfim-core 70+8 differential, marfim-tools 59, marfim-mcp 9,
marfim-gateway 15, marfim CLI 2+7 e2e); release binary 6.2 MB. Phase 1
complete; Phase 2 complete at the scope marked below; Phase 5 gated on
the owner items in §9.**

---

## 0. Phase-0 artifacts

- [x] Full git bundle backup — `../marfim-full.bundle` (2.5 MB, verify OK)
- [x] Node v1.2.0 tree extracted to `/tmp/node` (7.3 MB) for reference only
- [x] Surface inventory (below) mined from real source, not memory
- [x] Sandbox cleaned of prior-task artifacts (books, scripts, renders)

## 1. Wire semantics (from src/providers/{anthropic,openai,base}.js)

- [x] Stop reasons surfaced as provider-native values (`end_turn`,
      `tool_use`, `max_tokens`, `stop_sequence` for Anthropic; `stop`,
      `tool_calls`, `length` for OpenAI); harness treats `tool_use` /
      `tool_calls` as "tool call step", anything else as turn end
- [x] Tool-call args streamed as raw JSON fragments; parse failure yields
      `{__parseError: true, __raw}` sentinel (anthropic.js:97) — harness
      must re-request the call instead of feeding garbage to the tool
- [x] Retry policy (base.js:32-48): retry on 429 / 408 / 409 / 5xx and
      network errors; NEVER on other 4xx; honor `Retry-After` header;
      exponential backoff `500 * 2^attempt`
- [x] Usage normalized to `{input_tokens, output_tokens,
      cache_read_input_tokens, cache_creation_input_tokens}` (Anthropic
      shape; OpenAI mapped in)
- [x] Anthropic cache breakpoints (`cache_control`) round-trip
- [x] `max_completion_tokens` / `thinking` wire passthrough (OpenAI-compat)

## 2. marfim-core (Phase 1 + Phase 2 core)

- [x] Event bus: broadcast Event stream, JSON-serializable (SSE/stream-json)
- [x] Session store: append-only JSONL journal (`wal` semantics), crash
      recovery by replay, `rewind` to event id, fork/new/resume
- [x] Agent loop: build context → provider call → tool batch → repeat;
      editsThisBatch tracking; turn end on non-tool stop
- [x] Steering: two-queue (omp-style amend vs follow-up), drained only
      after the step's full tool-result set (agent.js:51,101-104);
      `steerMode: queue|handoff`
- [x] Verify loop: runs only when the step had NO tool call AND a prior
      batch mutated files (`editsThisBatch`); feedback budget; `verify
      autoRun` config
- [x] Stop-reason truncation: unparseable tool-call JSON → re-request the
      model for the same call (never execute `__raw`)
- [x] Single retry layer at the provider transport; loop never retries
- [x] Roles: primary / small / architect / review; every role consumed
      through the router; failover chains per role; per-role usage ledger
      with real model ids (not aliases); unknown pricing → fail closed
- [x] Compaction: context summarize via small model; failure → explicit
      turn stop with error event (never silent continue)
- [x] Permissions: rules deny > ask > allow, then mode default;
      permission modes default/plan/acceptEdits/yolo; single approval
      queue (mpsc) — approvals serialize, no double-prompt
- [x] Shell analysis at argv level: heredoc bodies, `$()`, backticks,
      process substitution, redirects, pipes, `&&`/`;`/`||` split — each
      segment checked; compound never auto-allowed past `ask`
- [x] Env sanitization: allowlist passthrough into subprocesses
- [x] PreToolUse hooks run before every tool; hook failure → tool blocked
      (fail closed); PostToolUse hooks fire after
- [x] Process-group spawn + timeout kill (`process_group(0)`,
      `kill -TERM -- -PGID` then KILL)
- [x] Config: strict unknown-key error with did-you-mean (Levenshtein ≤2)
- [x] Trust: per-directory trust levels; untrusted dir → reduced mode
- [x] Sandbox: availability probing, writable-roots + network flag
- [x] Skills: SKILL.md frontmatter (name/description/allowed-tools),
      `.marfim/skills` + `.claude/skills` compat
- [x] Memory: persistent user-model file; entries may only TIGHTEN
      safety constraints; deletions require explicit user action
- [x] Guardian: rule engine over the event stream (maxDenials, actions)
- [x] Evidence ledger: append-only claim/citation records, `evidenceGate`
- [x] Message board: cross-agent notes (subagent ↔ main), ttl, topics

## 3. Providers

- [x] Provider trait (stream chat, complete, list models)
- [x] OpenAI-compat wire (chat/completions, SSE chunks, tool_calls deltas,
      finish_reason pass-through) — verified against mock fixtures
- [x] Anthropic wire (v1/messages SSE: content_block_start/delta/stop,
      message_delta usage merge, cache_control) — fixtures
- [x] Mock provider (scripted turns, deterministic) — drives tests
- [x] Ollama (OpenAI-compat base URL variant)
- [x] Model catalog + aliases + per-model pricing (cost accounting);
      pricing unknown → fail closed for budgeted runs
- [~] google / zen wires: not ported (no documented on-device use;
      OpenAI-compat covers compatible endpoints). Listed in docs.

## 4. Tools (Node registry names preserved verbatim)

- [x] Read (line-numbered, image/pdf stub, path guard)
- [x] Write (path guard, parent create, overwrite confirm via permissions)
- [x] Edit — **ladder**: exact match → whitespace-flexible → line-anchored
- [x] MultiEdit (sequential edits, all-or-nothing)
- [x] Glob (`**`/`*`/`?` matcher, sorted mtime)
- [x] Grep (regex crate, exclude dirs, head_limit)
- [x] LS (dirs-first tree)
- [x] Bash (sandboxed exec, timeout, process group, output cap, cwd)
- [x] BashOutput / KillShell (background shell registry)
- [x] ApplyPatch (unified diff parser+applier, fuzz 1, new/delete files)
- [x] ApplyDiff (single-file diff apply, same engine)
- [x] TodoWrite (validate states, persist to session dir)
- [x] Think (scratchpad passthrough)
- [x] EnterPlanMode / ExitPlanMode
- [x] RepoMap (heuristic symbol extractor: py/rs/go/js/ts/java/c/cpp/rb;
      reference-count ranking, budget-limited outline)
- [x] Memory (tool front-end over memory store)
- [x] Compute (recursive-descent arithmetic evaluator; no eval)
- [x] ToolSearch (deferred-tool keyword rank search)
- [x] Task (subagent spawn: general/explore/plan/reviewer/security/
      silent-failure/build-resolver/evaluator personas + custom md),
      TaskOutput (poll)
- [x] WebFetch — **SSRF guard**: DNS resolve → block loopback/private/
      link-local/metadata IPs; https enforcement (config-relaxable);
      redirect re-validation; size + content-type caps
- [~] WebSearch — Brave/Tavily when API key configured; disabled
      (honest error) otherwise. Node required a key too.
- [x] Monitor / MonitorKill (file mtime watcher)
- [x] NotebookEdit (ipynb JSON cell replace)
- [x] BinInfo / BinStrings / BinHexdump (pure-Rust binary inspection)
- [~] BinSqlite — shells to `sqlite3` when installed, else honest error
- [~] AstQuery / Eval / JsModuleGraph — require `node` binary + tree-sitter
      corpus; Node v1.2.0 shipped these as best-effort too (external
      `node` needed). Implemented as availability-gated external runners.
- [≠] devtools browser tools: Node v1.2.0 had a `devtools.js` CDP client;
      Rust scope keeps SSRF-guarded WebFetch only — CDP client deferred to
      the app wave (owner sign-off recorded in CHANGELOG).

## 5. Harness surfaces

- [x] REPL loop (line-based, keyboard-first, no TUI dep yet — ratatui wave
      is Phase 4/app)
- [x] Slash commands — all 65 Node names (aliases preserved, +q/?/ls-cmds):
      accounts, agents, architect, audit, btw, budget, cachehealth,
      capabilities, checks, clear, commit, compact, context, copy-context,
      cost, cron, diff, diffs, dig, doctor, editor, explain, export,
      forecast, fork, goal, handoff, help, hooks, hot, init, map, mcp,
      memory, model, models, new, permissions, plan, plugins, policy,
      profile, provider, quit, recall, recipes, replay, resume, review,
      rewind, sandbox, sdiff, skills, smoke, stats, status, swarm, theme,
      tree, undo, usage, verify, wal, workflow, yolo
- [x] Custom commands from `.marfim/commands/*.md` + `.claude/commands`
      compat ($ARGUMENTS substitution)
- [x] Headless: `--output-format text|json|stream-json` (-p prompt),
      stdin piping, exit codes
- [x] CLI flags parity: --output-format, --model/-m, --provider, --resume,
      --permission-mode, --cwd/-C, --title, --profile, --budget, --failover,
      --glob, --on-fail, --debounce, --port, --host, --token, --max-mode,
      --var, --days
- [x] Subagents: builtin personas (general/explore/plan/reviewer/security/
      silent-failure/build-resolver/evaluator) + custom frontmatter loaders;
      worktree snapshot isolation; results replay through the parent
      permission path; rewindable
- [x] `.marfim/agents/*.md` + `.claude/agents/*.md` compat loaders

## 6. Gateway surfaces

- [x] `serve` — hand-rolled HTTP/1.1 + SSE on tokio (no axum: binary-size
      budget); routes: GET /healthz, GET /v1/events (SSE bus), POST /v1/prompt
      (SSE turn), GET /v1/sessions, GET /v1/sessions/:id, POST
      /v1/approvals/:id, GET /v1/tools; 127.0.0.1 default bind + token
- [x] ACP agent side: initialize, session/new, session/prompt,
      session/update notifications (agent_message_chunk, tool_call),
      session/set_mode, fs/read_file, fs/write_file, terminal/create+
      output+wait_for_exit — over stdio JSON-RPC, Content-Length framing
      supported both framed and newline-delimited
- [x] ACP client side: spawn external agent, drive prompt, surface
      tool updates. Catalog: `opencode` → `opencode acp`,
      `hermes` → `hermes-agent acp` + `externalAgents` config. **Per the
      no-invention rule: sandbox lacks these binaries, so every profile
      ships `enabled=false, unverified=true` until the owner runs them.**
- [x] MCP client: stdio transport, initialize/tools/list/tools/call,
      `server__tool` namespacing into the registry (deferred by default)
- [x] MCP server (`marfim mcp`): expose marfim tools over stdio JSON-RPC
- [x] LSP server (`marfim lsp`): initialize/shutdown, textDocument/
      didOpen/didChange, completion (slash commands + files),
      publishDiagnostics (TODO/FIXME/long-line lints)

## 7. Config surface (strict; keys from src/config/loader.js + getters)

- [x] model, provider, providers, profile, smallModel, architectModel,
      failover, routing, budget, permissions, sandbox, bash, tools,
      mcpServers, hooks, lsp, wal, cache, compaction, microCompact,
      autoCompact, autoCompactThreshold, steerMode, stream, theme,
      env, git, review, audit, evidence, evidenceGate, guardian,
      riskScores, swarm, cron, goal, plugins, policy, skills, verify,
      ttsr, externalAgents, dialectTools, shield, gateguard, supplyChainGate,
      mcpHealthGate, unattended, verbose, maxTokens, temperature,
      thinkingBudget, autoCompactFloor
- [x] Unknown key → error + did-you-mean; nested keys validated
- [x] Single version source: `marfim-core::VERSION` → UA string,
      `--version`, docs pages

## 8. Fixtures / differential tests (Node goldens → Rust)

- [x] fixtures/wire/anthropic-*.json, openai-*.json, mock-*.json recorded
      against a local mock HTTP provider; Rust wire layer must reproduce
      the Node-observable normalization byte-for-byte (usage shape, stop
      reasons, parse-error sentinel, Retry-After honoring, 4xx no-retry)
- [x] INVARIANT regression tests (each numbered INV-#, see tests/)
- [x] sockets-blocked differential suite: mock provider + sandbox flags
      prevent egress; suite passes with networking disallowed

## 9. Docs / repo hygiene (Phase 5 gate)

- [x] CHANGELOG documents every added dependency with justification
      (tokio, reqwest+rustls, serde, serde_json, clap, thiserror,
      futures-util, regex — see CHANGELOG 0.3.0)
- [x] README, docs/ARCHITECTURE, docs/FEATURE-MATRIX updated for Rust
- [ ] **Owner actions (blocked on owner, not code):** branch protection on
      main, purge of git-cached objects from history, installing the two
      external agents to unverify→verify flip
- [ ] Phase 5 (after owner gate): delete Python/Node sources, orphan
      commit `Marfim 1.0.0 (Rust)`, force-push, delete remote tags,
      tag v1.0.0

## 10. Bug-compat rejected (intentional divergences)

1. `≠` Tool-call JSON parse errors are NEVER handed to tools as
   `{__parseError}` payloads. Node recorded them into the transcript and
   could let a downstream tool see the sentinel; Rust re-requests the
   call before anything executes. (INV-03)
2. `≠` Node's `steerMode: handoff` aborted mid-stream at chunk boundaries;
   Rust aborts at the same point but guarantees the partial assistant
   message is journaled with `stop_reason: "aborted"` so /rewind and
   accounting stay exact. (INV-02 family)
3. `≠` Node's `ApplyDiff` fuzz could match across `snake_case`/`camelCase`
   identifiers when whitespace-flexible matching failed. Rust's ladder
   stops at line-anchored matching; fuzzy identifier matching rejected as
   unsafe. (INV-11)
