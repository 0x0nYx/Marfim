# Changelog

## 1.0.0 — engineering round 2 (2026-11-06)

### Step 2: MCP wired end-to-end
- `McpClient` connects per config `mcpServers` — stdio AND streamable
  HTTP (JSON-RPC POSTs; plain-JSON and SSE-framed responses) — behind
  the trust gate at session start (config layers are trust-gated, so an
  untrusted cwd can never contribute servers). Servers that fail to
  connect are skipped with a warning, never silently.
- stdio MCP servers spawn with a SCRUBBED environment: an explicit
  keep-list (PATH/HOME/...) plus config-declared `env` entries —
  provider keys and ambient secrets never leak into tool servers.
- Tools register as `mcp__<server>__<tool>` (registry entries with the
  server's schema) and dispatch through the SAME PermissionEngine path
  as builtin tools (Execute-class: Ask verdicts surface as approvals).
- `marfim mcp` serves the builtin catalog and routes EVERY tools/call
  through the permission engine — the old "requires marfim run"
  read-only refusal is GONE. Ask verdicts fail closed (stdin is the
  protocol, so no interactive approver). Server surfaces never make
  outbound mcpServers connections (self-referencing configs would
  recurse); sessions compose namespaces.
- Fail-closed approvers (headless, ACP) now deny through the actor's
  authoritative Approve command — answering the forwarded job's
  tombstone responder was a silent no-op that hung every Ask-class tool
  call (caught by the new e2e).
- Tests: stub streamable-HTTP server on a loopback socket (connect →
  tools/list → tools/call), config-shape + env-scrub units, real-binary
  e2e: Bash denied (permission path), Read executed, and a session
  connecting a self-hosted `marfim mcp` child (denied without rule,
  full roundtrip with allow rule).

### Step 1: gateway hardening + SessionHost (security-blocking)
- `marfim serve` rewritten from a hand-rolled HTTP parser onto hyper 1.x
  (hyper-util auto connection builder, HTTP/1.1 + SSE). Every gate is
  fail-closed and covered by tests on a real socket: bearer token
  MANDATORY (`--token` or auto-generated 128-bit token printed once),
  constant-time comparison; Origin/Host validation (DNS-rebinding
  guard, 403 before any body read); 1 MiB header buffer + 1 MiB body
  cap (declared oversize → 413 unread); header-phase timeout
  (`header_read_timeout` + `timer`), time-boxed body collection, SSE
  pings + hard stream lifetime. Content-Length 10^12, slowloris
  dribbles, and malformed binary garbage are proven to kill only their
  own connection — the listener keeps serving.
- `SessionHost` added in `marfim-core`: owns every SessionActor
  (create/get/list/close, snapshots), construction injected by the
  binary crate. serve, acp, headless text/json/stream-json and tui all
  build their sessions through it. The REPL keeps direct actor
  ownership (documented decision: slash dispatch needs the concrete
  `ToolCtx`, which core's host cannot type without inverting the tools
  dependency).
- serve endpoints are REAL: `/v1/prompt` streams a real turn (SSE, ends
  with the actor's true StopReason), `/v1/approvals` + `POST
  /v1/approvals/:id` resolve REAL pending approvals (routed through the
  actor's authoritative Approve command; ApprovalRequested/Resolved are
  now published on the session bus), `/v1/sessions`, `/v1/sessions/:id`
  and `/v1/tools` (real registry catalog) are live.
- ACP: the `marfim received:` echo is DELETED. `marfim acp` runs real
  journaled turns on per-ACP-session SessionActors, streaming
  session/update notifications (agent_message_chunk, tool_call,
  tool_call_update) and answering with real stop reasons.
- e2e per surface: real-binary healthz + 401 assertions; gateway e2e
  drives a scripted mock-provider tool call through the permission
  engine → pending approval on /v1/approvals → HTTP resolution → tool
  executes → turn ends over SSE; a real-binary ACP stdio session runs a
  mock-provider turn end-to-end.
- `spawn_session` now returns `(SessionHandle, approval receiver)` so
  hosts own approval intake explicitly; SessionSnapshot gained
  denied_tools/executed_tools (D12 exit codes work through the actor).

### Step 0: baseline
- `rust-toolchain.toml` pins the workspace to 1.99.1 (clippy + rustfmt
  components included); CI installs exactly this toolchain for the main
  gate job.
- CI repaired and extended: the `push` trigger was `branches: ain]` (a
  malformed literal), so CI had never run on main pushes — now
  `branches: [main]`; new jobs: pinned-toolchain gate (ubuntu+macos:
  fmt, clippy -D warnings, tests, release size budget), MSRV(1.78)
  job (`cargo +1.78 test --all` + binary build), `cargo deny check`
  (deny.toml: licenses/bans/RustSec v2, yanked=deny), `cargo audit`.
- `crates/marfim-tools/src/patch.rs`: `find_offset` rewritten with
  `find_map`/`find` (clippy `manual_find`), behavior identical.
- Deleted `marfim-core::compact::summarize_via` and its private runtime
  bridge — no production callers, and the nested
  `block_on`-inside-runtime construction can panic under an outer
  runtime. (Compaction runs through the loop's own async path.)
- MSRV(1.78) made real: the lockfile was un-buildable on 1.78 (several
  transitive crates moved to edition 2024 / rustc 1.82-1.89). Pinned
  to the newest 1.78-compatible versions: clap 4.5.23, reqwest 0.12.9,
  quinn 0.11.3 (drops the cpufeatures 0.3 chain), rand 0.8.8, zeroize
  1.8.1, indexmap 2.7.1, toml 0.8.20, toml_edit 0.22.24, winnow
  0.7.11, serde 1.0.219, serde_json 1.0.140, proc-macro2 1.0.94,
  quote 1.0.40, hyper-util 0.1.10, hyper-rustls 0.27.5, instability
  0.3.7, tokio 1.43.1, tokio-util 0.7.13, unicode-segmentation 1.12.0,
  utf8_iter 1.0.4, uuid 1.11.0.
- `marfim-brain` code brought back onto 1.78: `LazyLock` ->
  `OnceLock` (confusables), `is_none_or` -> `match` (sketch, spellfix),
  `as_chunks::<4>` -> index math (dp SHA-256 message schedule). All
  semantics unchanged; differential fixtures still replay byte-exact.
- `clippy.toml` sets `msrv = "1.78"` for the toolchains that honor it.
- Decision (inputs): ORACLE_NODE = the Node marfim v1.2.0 tree,
  preserved locally (`preserved/oracle`, also fetchable as tag
  `archive/node-v1.2.0` from the preservation bundle in
  `0x0nYx/marfim-archive`). PY_INTEL = https://github.com/0x0nYx/caelestia-assistant
  (public; 478 Python files; `assistant/core/*.py` matches the
  `Python origin:` headers embedded in the oracle's `src/intel`).

### 1.0.0 baseline — one binary, one brain (2026-11-05)

The terminal 1.0.0: a single Rust workspace, a single static `marfim`
binary, and the intelligence modules ported differentially from the Node
oracle. 501 tests green across 6 crates. This hardening pass also made
the P1 gateway contract real (hyper + SessionHost + token auth), applied
config truth to the wire, and replaced doc claims with docs-as-tests. The
Python harness and its test suite are gone
(the porting reference lives in git history under the `archive/` tags
and in the preservation bundles).

### The brain (P4)
- `marfim-brain`: 18 intelligence modules ported 1:1 from the Node
  oracle (adwin, conformal, abers, conservative, ensemble, dp,
  confusables, dedupe, fts, phonetics; minhash, pooling, ranking,
  regret, sketch, spellfix, state; blake2b per RFC 7693) — every module
  validated by JSONL fixtures generated by RUNNING the oracle under
  node; byte-exact ints/strings, <=1 ulp floats. 17 differential suites.
- Faculty hooks in the loop: pre_model / pre_tool (may deny with a
  verbatim reason) / post_step (ADWIN drift + conformal risk + regret
  audit) / pre_compaction (bounded salience protection) / idle_propose
  (propose-approve — never silent execution). Every hook call is
  published as a faculty_hook event (the /why surface). Learning state
  is local, bounded, inspectable, and /forget-reversible.

### Session actor (P2)
- One task per session owns the harness; Prompt/Steer/Cancel/Approve/
  Undo/Rewind/Fork/Snapshot commands; journal-backed turn-level
  undo/rewind with audit tails; fork with provenance records on both
  journals; --resume rebuilds history from the journal.

### Provider mesh (P3)
- Gemini preset (OpenAI-compatible endpoint); 9 presets total.
- Offline models.dev catalog snapshot (versioned, committed; zero
  runtime network) pricing every known model honestly.
- UCB1 bandit routing (--routing bandit) over observed per-model
  outcomes; chain order remains the default.
- ACP handshake fixtures; fs errors name the path.

### Sandbox (P5)
- Landlock (Linux) fail-closed filesystem containment via the safe-API
  crate: reads+exec everywhere, writes only under configured roots; an
  unsupported kernel REFUSES to run the child (exit 84) instead of
  running it free. Seatbelt profile generator (macOS).
- Symlink-aware path containment; deny>ask>allow order-independence and
  per-shell-segment policy regression battery.

### TUI (P6)
- `marfim tui`: the session actor's first real UI — streaming
  transcript with tool cards, approval modal with risk badge, session/
  context meters, why panel, NO_COLOR, 12 TestBackend snapshots.

### Hygiene (P1 remainder, P7)
- Real Ctrl-C graceful cancel (the dropped-future deadlock found by the
  new e2e), per-step compaction semantics pinned by tests, TurnEnded
  exactly-once on every stop path.
- Workspace version 1.0.0, MIT everywhere, Python deleted, CI on
  ubuntu+macos (fmt, clippy -D warnings, tests, 25 MB size gate),
  release workflow ships static binaries + SHA256SUMS.


## 0.3.0 — the Rust wave (2026-11-05)

The Marfim core is rewritten in Rust: a Cargo workspace of five crates
(`marfim-core`, `marfim-tools`, `marfim-mcp`, `marfim-gateway`, `marfim`),
`#![forbid(unsafe_code)]` in every crate. **170 tests green** across the
workspace, including a differential suite replaying Node v1.2.0 wire
goldens (see `PARITY.md` for the item-by-item checkpoint). The Python
harness remains in this release and keeps working; deletion is gated on
the owner sign-off (Phase 5).

### marfim-core (Phase 1)
- Event bus (broadcast, JSON-serializable — drives stream-json + SSE).
- Append-only session journal: fsync per record, crash-recovery replay,
  rewind with `.pre-rewind` audit tail.
- Agent loop with the invariant set: steering drains only after a full
  tool-result batch (INV-02); verify fires only on no-tool-call steps
  after mutations with a feedback budget (INV-01); unparseable
  tool-call JSON is re-requested, never executed (INV-03); a single
  retry layer at the transport — 429/408/409/5xx + network errors,
  `Retry-After` honored, other 4xx never retried (INV-06); per-role
  router with failover, real-model usage ledger, unknown pricing fails
  closed (INV-05); compaction failure is an explicit stop (INV-07).
- Permission engine: deny > ask > allow then mode; argv-level shell
  analysis (quote-aware tokenizer; heredoc/`$()`/backticks/process
  substitution can NEVER ride an allow past ask; compound/`&&`/`;`/`|`
  segments checked individually); single serialized approval queue with
  fail-closed when no listener; env allowlist for subprocesses.
- Hooks: PreToolUse fail-closed (spawn error, non-zero exit, timeout and
  bad JSON all block the tool), PostToolUse best-effort.
- Sandbox: `unshare(1)` availability PROBED by running it (containers
  EPERM it), process-group spawn + TERM→KILL tree teardown, sanitized env.
- Strict config: unknown keys error with Levenshtein did-you-mean.
- Skills (SKILL.md frontmatter, `.marfim/skills` + `.claude/skills`),
  memory store whose entries can only TIGHTEN safety (relaxation
  attempts are stored AS constraints; deletion needs explicit confirm),
  Guardian rule engine with denial cap + kill-switch, evidence ledger,
  TTL message board, trust store (longest-prefix).

### marfim-tools (Phase 2)
All 34 Node v1.2.0 tool names (PARITY §4): Read/Write/Edit (exact →
line-anchored → whitespace-flexible ladder; identifier-fuzzy matching
rejected, bug-compat #3)/MultiEdit (all-or-nothing)/Glob/Grep/LS/Bash
(timeout, process-group kill, output cap)/BashOutput/KillShell/
ApplyPatch/ApplyDiff (unified diff parser with ±1/2/4/8 fuzz and
trim-matched fallback)/TodoWrite/Think/EnterPlanMode/ExitPlanMode/
NotebookEdit/RepoMap (heuristic symbol extractor: py/rs/go/js/ts/java/c/
cpp/rb; reference-count ranking)/Memory/Compute (recursive-descent
evaluator)/ToolSearch/Task/TaskOutput/WebFetch (SSRF guard: DNS resolve
→ block loopback/private/link-local/CGNAT/metadata IPs; https-only;
redirect re-validation; 2 MiB cap)/WebSearch (honest error without an
API key)/Monitor/MonitorKill/BinInfo/BinStrings/BinHexdump/BinSqlite/
AstQuery/Eval/JsModuleGraph.

### marfim-mcp (Phase 2)
MCP client (stdio, initialize handshake, tools/list → `server__tool`
namespacing, tools/call) and MCP server (exposes the marfim catalog;
read-only tools execute, execution-class tools refuse without the
interactive host).

### marfim-gateway (Phase 2)
- `serve`: hand-rolled HTTP/1.1 + SSE — /healthz, /v1/events,
  /v1/prompt, /v1/sessions, /v1/approvals/:id, /v1/tools.
- ACP agent over stdio (initialize, session/new, session/prompt with
  session/update notifications, session/set_mode, fs/read_file,
  fs/write_file) accepting both Content-Length and newline framing.
- ACP client + external agent catalog (`opencode` → `opencode acp`,
  `hermes` → `hermes-agent acp`): per the no-invention rule every
  profile ships `enabled=false verified=false` until the owner runs the
  binaries.
- LSP server (initialize, didOpen/didChange, completion with the full
  slash-command set, publishDiagnostics TODO/FIXME + long lines).

### marfim (CLI, Phase 2)
`marfim -p "..." --output-format text|json|stream-json` (claude-code
compatible contracts), interactive REPL with the full 65-command slash
surface + `q`/`?` aliases + `.marfim/commands/*.md` discovery, subcommands
`serve` `acp` `lsp` `mcp` `doctor` `agents` `sessions`, Node v1.2.0 flag
parity (`--model/-m --provider --resume --permission-mode --cwd/-C
--title --profile --budget --failover --glob --on-fail --debounce
--port --host --token --max-mode --var --days`).

### Performance
Release binary: **6.4 MB** (budget <25 MB). tokio runtime idles well
under the 80 MB core RAM budget.

### Dependencies (workspace) — justification per the minimal-deps rule
- `tokio` — async runtime: every surface (providers, gateway, sandbox,
  MCP) is IO-bound; the de-facto standard, no fork risk.
- `reqwest` (default-features off, `rustls-tls`) — provider HTTP +
  WebFetch; rustls avoids the OpenSSL toolchain dependency.
- `serde` + `serde_json` — wire/session/config serialization; non-negotiable.
- `clap` (derive) — CLI arg surface with Node-flag parity; hand-rolling
  an arg parser invites drift.
- `thiserror` — error enums without boilerplate; proc-macro only at
  compile time.
- `futures-util` — SSE byte-stream consumption (`bytes_stream`).
- `regex` — Grep tool + lint diagnostics + html stripping; a regex
  engine is part of the product surface (Node used ripgrep internally).
- `async-trait` — `Provider`/`ToolHost` async traits (pre-1.75-stable
  AFIT ergonomics; revisit when RPITIT covers object-safety needs).
- Tree-sitter was evaluated for RepoMap/AstQuery and DEFERRED: the
  grammars cost ~30 MB of binary size against the 25 MB budget; the
  heuristic extractor covers 9 languages and AstQuery stays honestly
  disabled (see PARITY §4).

### Intentional divergences (bug-compat rejected — PARITY §10)
1. Truncated tool-call JSON is never handed to tools (Node recorded a
   `{__parseError}` sentinel into transcripts; Rust re-requests).
2. Steer-handoff aborts journal the partial assistant message with
   `stop_reason: aborted` (Node dropped it; accounting stayed exact).
3. Edit ladder stops at line-anchored matching (Node's ApplyDiff could
   fuzzy-match across identifier casing; unsafe).


## 0.2.0 — the safety wave (2026-11-03)

Full 34-finding security & correctness review executed. Every finding has a
regression test (`python3 -m unittest discover -s tests`, 115+ tests).

### P0 — the safety system now works
1. **CLI bypass fixed**: `approve_all: False` no longer flips bypass on every
   invocation; `--mode` and `-y` are honored independently.
2. **Project-config trust gate** (`marfim trust add|list|revoke`): an
   untrusted repo's `.marfim/config.toml` may only TIGHTEN (deny/ask rules);
   its hooks, mcp_servers, providers, mode, allow, sandbox, protect_configs
   and project skill dirs are ignored until the user trusts the file
   (sha256-keyed trust store at `~/.marfim/trust.json`). Ported from the
   archived Node tree's `trust.js` design.
3. **patch/multi_edit under protection**: paths are parsed out of the patch
   DSL and every target is permission-checked; prompts show every path.
4. **Plan-mode bash is actually read-only**: real shell-lexer check, shell
   operators and substitutions rejected, per-command flag allowlists,
   `find`/`git branch`/`echo`/`python -c` removed.
5. **Background bash** goes through execpolicy + sandbox, is capped (8 jobs),
   and logs to 0600 files.
6. **"Always allow" grants are exact commands** (no prefix globs), compound
   commands must match EVERY segment, and interpreters/shells/sudo/curl/git
   are never auto-granted.
7. **grep option injection closed**: `rg -e <pattern> -- <path>`; rg errors
   (exit 2) surface instead of "(no matches)".
8. **bypass respects deny rules and config protection**.
9. **Path containment**: secret-like paths (.ssh, .env, .aws, .git/config,
   the Marfim home, the session dir) are denied in every mode; writes must
   stay inside the workspace; the global Marfim config is protected.
10. **Sandbox fails closed**: a requested-but-missing backend is a hard error
    (`sandbox_required=false` is the explicit opt-out); self-defense ro-binds
    for `.git/hooks`, `.git/config`, `.marfim/`, `MARFIM.md`; children run
    with a scrubbed environment; the `network` config key is wired.
11. **Malformed configs refuse to run** (loud error, exit 2) instead of
    silently dropping the safety settings.

### P1 — data loss, silent edits, loop & session
12. outdent strategy applies the replacement (re-based indentation).
13. the destructive `nearest` fuzzy strategy is REMOVED — near-misses are
    reported as hints, never applied; no fuzzy application with replace_all.
14. ambiguous `old_string` errors with the match count.
15. `write` validates arguments against the tool schema, requires read
    evidence for existing files, and is atomic (temp + os.replace, mode kept).
16. stale-read rejection is real: (mtime_ns, sha256) evidence per file.
17. encoding/newline round-trip: CRLF stays CRLF, latin-1 files survive
    byte-for-byte, mixed-endings files are refused.
18. glob/repomap prune ignored dirs during the walk (a project path
    containing `build` no longer returns "(no matches)").
19. tool-call JSON parses leniently (raw newlines, trailing commas); parse
    failures surface as parse errors.
20. interrupts leave NO dangling tool calls (synthetic results on every exit
    path).
21. compaction and /clear survive resume (single `compact` record, replayed).
22. compaction cuts on turn boundaries; failed/empty summaries keep history.
23. context windows are provider-qualified, cached, network-free in the loop.
24. provider layer: `reasoning_content` separated from answers, mid-stream
    errors raise, no-index tool-call deltas keyed by id, transport errors
    wrapped, Anthropic prompt-cache breakpoints + cache-write accounting,
    configurable max_tokens, volatile env moved out of the system prompt.
25. subagents: spawn cwd honored, worktrees live outside the repo and are
    removed with a mergeable diff, role prompts + memory injected,
    role-restricted permission engines (bypass cannot leak), shared
    stop_event.
26. todo state updates memory and file together.
27. repomap: no None-group crashes, indented symbols visible, .kt/.dart/
    .cc/.cxx/.hpp/.mjs supported.
28. REPL has ONE input owner; five prompts sent 1s apart in a real pty get
    five replies (regression-tested).
29. mid-turn steering and Ctrl-C actually work (turns run in a worker thread;
    SIGINT sets stop_event instead of killing the REPL).
30. `marfim mcp add` works (TOML-escaped, duplicate-safe, auto-trusts the
    user's own edit).
31. dead bits revived: SessionStart/Stop hooks fire, home MARFIM.md is used,
    --session-name honored, exit code 2 returned on permission denial,
    Tool.readonly drives the engine, /undo matches only Marfim-authored
    commits, /model only treats known providers as providers, session rm
    cleans snapshots, rewind handles absolute paths and DELETES files the
    agent created, session files are 0600.

### P2 — MCP, hooks, web
32. MCP: server-initiated requests answered (ping/roots/list), stderr drained
    in a thread, dead servers marked dead, pagination followed with a
    loop-guard, tools/list_changed honored, parallel startup, per-call
    timeouts, provider-safe tool names, structuredContent preserved, scrubbed
    child env, thread-safe pending map. **New: streamable-HTTP transport**
    with per-server auth headers.
33. PreToolUse hooks fail CLOSED (timeout ⇒ deny) and run with scrubbed env.
34. webfetch: SSRF guard (private/loopback/link-local/metadata ranges denied,
    re-checked per redirect hop, credentials in URLs refused).

### §4 — unified intelligence
- **Model router**: role → ordered provider/model candidates with 429/5xx
  failover, per-role dollar budgets, and a capabilities record (tools are
  never sent to a model that cannot take them).
- **MCP as universal adapter**: HTTP transport + readOnlyHint-driven
  permission tier.
- **External agents as backends**: `[external_agents]` runs opencode / codex
  / claude-code / hermes headlessly inside their own worktree with a
  stream-json normalizer; the Marfim loop stays the control plane.
- **Parallel subagents**: consecutive `task` calls run in a bounded thread
  pool with cancellation.
- **Verify tool**: `verify.commands` from the trusted config run in the
  sandbox; failures feed a reflection loop bounded by `reflection_cap`.

## 0.1.0 — initial clean-room release

Unified harness synthesized from forensic analysis of opencode v2, Codex,
Goose, Aider, Claude Code, Hermes, Oh My Pi, dsh-superagent, REA and ECC.
Python 3.11+, stdlib-only core.
