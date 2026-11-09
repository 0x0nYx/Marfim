# Marfim Architecture

## Rust workspace (1.0.0 — current)

Twelve crates: `marfim-core` (loop/providers/router/journal/permissions/
sandbox/faculty/session/execpolicy/urlpolicy/oauth/mandates/cronjobs/…),
`marfim-app` (the embeddable application layer), `marfim-tools` (built-in
tools), `marfim-term` (PTY sessions), `marfim-state` (durable sqlite state),
`marfim-mcp`, `marfim-gateway` (serve/LSP/ACP/protocol v1), `marfim-brain`
(the differentially ported intelligence modules + LearningFaculty),
`marfim-exec` (standalone sandbox exec-helper), `marfim-ffi` (C ABI),
`marfim-testkit` (shared test scaffolding), `marfim` (CLI/REPL/TUI).
See docs/PORT-LEDGER.md for the honest port status.

Twelve crates, one binary, `#![forbid(unsafe_code)]` everywhere:

```
crates/
  marfim-core      events · journal · loop_ · providers (openai/anthropic/
                   responses/gemini/bedrock + mock) · router · permissions ·
                   shellparse · hooks · sandbox · trust · catalog · oauth ·
                   execpolicy · netproxy · urlpolicy · smart_approval ·
                   mandates · cronjobs · sources · sessiontree · trajectory ·
                   skills · memory · memcaps · memvault · instincts · curator ·
                   guardian · rea · hashline · iso · git · platform · privacy
  marfim-app       the embeddable application layer (W1.1): settings,
                   session factory, platform trait, credential chain
  marfim-brain     the differentially ported intelligence modules +
                   LearningFaculty (fixture-verified; see docs/PORT-LEDGER.md)
  marfim-tools     registry (ToolDef contract, deferred tools, ToolSearch) +
                   all 41 tools: fs ladder · patch · bash · web(SSRF) ·
                   repomap · compute · todo · notebook · terminal · monitors
                   · CodeMode · subagents · binary inspection
  marfim-term      durable PTY sessions over a real terminal screen model (W3)
  marfim-state     durable sqlite state (feature `sqlite`): WAL policy, journal
                   projections, durable inbox, session tree, trajectory store
  marfim-mcp       JSON-RPC 2.0 · client (stdio spawn, server__tool namespacing)
                   · server (expose the marfim catalog)
  marfim-gateway   serve (HTTP/1.1+SSE) · protocol v1 (seq + Last-Event-ID ring)
                   · acp (agent+client) · lsp (Content-Length framing)
  marfim-exec      standalone sandbox exec-helper binary (read-only mounts)
  marfim-ffi       the C ABI boundary owning the tokio runtime (mobile)
  marfim-testkit   race-free test scaffolding shared by the workspace
  marfim           CLI: headless text|json|stream-json · REPL + 67 slash
                   commands · serve/acp/lsp/mcp/doctor/agents/sessions/tui
```

Data flow for one turn:

```
user → REPL/headless → Harness.run_turn
         │ journal.append(user)            (fsync, append-only)
         ├─ Router.resolve(Role::Primary)  (failover chain, priced-only under budget)
         ├─ Provider.chat_step             (THE retry layer: 429/408/409/5xx,
         │                                  Retry-After, never other 4xx)
         ├─ unparseable tool args? ──▶ re-request (never executed)
         ├─ PermissionEngine.check         (deny>ask>allow>mode; argv-level
         │                                  shell analysis; single queue)
         ├─ Hooks.pre_tool_use             (fail closed)
         ├─ ToolHost.execute               (sandbox: clean env, process group,
         │                                  timeout→TERM→KILL the group)
         ├─ journal.append(tool results)   (is_error round-trips on the wire)
         └─ steering drains HERE           (after the full tool-result set)
```

Verification (INV-01) fires only on a no-tool-call step after a mutating
batch, with a feedback budget. Compaction failure stops the turn
explicitly (INV-07). Every event flows onto the broadcast bus, so the
REPL, `stream-json` headless mode and the SSE gateway see identical data.

Binary size budget: <25 MB (CI-enforced; actual ≈14.1 MB release build).
Core RAM budget: <80 MB idle.
