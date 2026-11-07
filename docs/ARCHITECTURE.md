# Marfim Architecture

## Rust workspace (1.0.0 — current)

Six crates: `marfim-core` (loop/providers/router/journal/permissions/
sandbox/faculty/session), `marfim-tools` (built-in tools), `marfim-mcp`,
`marfim-gateway` (serve/LSP/ACP), `marfim-brain` (the differentially
ported intelligence modules + LearningFaculty), `marfim` (CLI/REPL/TUI).
See docs/PORT-LEDGER.md for the honest port status.

Five crates, one binary, `#![forbid(unsafe_code)]` everywhere:

```
crates/
  marfim-core      events · journal · loop_ · providers (openai/anthropic/mock)
                   router · permissions · shellparse · hooks · sandbox · trust
                   skills · memory · guardian · evidence · board · usage · config
  marfim-tools     registry (ToolDef contract, deferred tools, ToolSearch) +
                   all 34 tools: fs ladder · patch · bash · web(SSRF) · repomap
                   compute · todo · notebook · monitors · binary inspection
  marfim-mcp       JSON-RPC 2.0 · client (stdio spawn, server__tool namespacing)
                   · server (expose the marfim catalog)
  marfim-gateway   serve (HTTP/1.1+SSE, hand-rolled) · acp (agent+client)
                   · lsp (Content-Length framing)
  marfim           CLI: headless text|json|stream-json · REPL + 67 slash
                   commands · serve/acp/lsp/mcp/doctor/agents/sessions
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

Binary size budget: <25 MB (actual 6.2 MB release). Core RAM budget:
<80 MB idle.
