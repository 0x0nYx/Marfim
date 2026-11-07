# Marfim Roadmap

v0.1.0 ships the convergent core. This roadmap sequences the remaining high-value
capabilities surfaced by the forensic analysis, each with its source doctrine.

## v0.2 — intelligence & context

> **Status (v0.2.0):** model-role auto-routing SHIPPED (router with failover + budgets +
> capabilities); MCP HTTP/SSE transport SHIPPED; external agents as worktree-isolated
> backends SHIPPED; parallel subagents SHIPPED; verify-driven reflection SHIPPED;
> personalized-PageRank repo map SHIPPED (file-level reference graph, token-budget
> binary search, deterministic); micro-compaction SHIPPED (tool-result elision with
> protected tail). Remaining below: tree-sitter symbol frontends (the extractor is
> line-precise today: modifier-aware Rust, Go receivers, TS interface/type, comment
> skipping; tree-sitter grammars would raise per-language precision further) and
> CodeMode.
- **Tree-sitter symbol frontends for the repo map**: the PageRank ranking and
  token-budget binary search are live; tree-sitter grammars remain the planned
  precision upgrade over the (already comment- and modifier-aware) line extractor
  (aider `repomap.py`).
- **CodeMode**: expose namespaced tool catalogs to the model as callable code executed
  in a restricted interpreter (opencode v2 `packages/codemode`) — collapses N tool
  schemas into one code surface.
- **Model-role auto-routing**: weak model for titles/summaries (aider), fast model for
  plan, vision role for images (omp).
- **Micro-compaction**: tool-result elision with protected tail before summarization
  (hermes `micro_compaction.py`).

## v0.3 — hard isolation & policy
- **Durable inbox sessions**: SQLite projection over the JSONL log with crash-recovery
  settlement of stale tool calls (opencode v2 `specs/v2/session.md`).
- **Starlark execpolicy**: allow/prompt/forbidden prefix rules with self-tests
  (codex `execpolicy`), replacing the deny-list.
- **Network enforcement proxy**: local HTTP/SOCKS allowlist proxy bound to bash runs
  (codex network proxy).
- **LLM read-only permission judge** with cached decisions + deterministic inspector
  chain (goose smart-approve).

## v0.4 — learning & memory
- **Learning loop**: background warm-cache review forks that distill sessions into
  skills (hermes `background_review.py`).
- **Instincts**: atomic confidence-scored behaviors injected at session start,
  evolved into skills (ECC continuous-learning-v2).
- **Session FTS search**: sqlite FTS5 over past sessions (hermes `session_search`).
- **Memory vault**: portable memory format across harnesses (ECC `ecc.memory.v1`).

## v0.5 — autonomy (the dsh layer)
- **Mandates**: versioned, revocable authorization objects for autonomous work;
  deterministic gate + LLM assessment split (dsh-superagent `policy.ts`).
- **Durable effect queue + outbox**: store-first, act-later execution with receipts
  (dsh `effects.ts`/`outbox.ts`).
- **Source plugins**: GitHub/Feishu triggers → router → per-ticket worker sessions
  with a monitoring panel (dsh architecture).
- **Scheduled jobs**: cron-like recurring agent runs (opencode schedule, hermes cron).

## v0.6 — surfaces
- **LSP integration**: diagnostics re-injected with staleness counters, 14 actions
  (omp `src/lsp`).
- **DAP debugging**: breakpoint/stepping/evaluate tool (omp `src/dap`).
- **ACP server**: editor integration protocol (goose/hermes acp adapters).
- **TUI upgrade**: alternate screen, themes with auto dark/light (omp renderer).
- **Recipe/workflow system**: parametric YAML task templates with subrecipes
  (goose recipes).

## v0.7 — evidence pipeline (the REA layer)
- **Evidence records**: every tool result becomes a canonical evidence object with
  confidence/coverage and a retained-view pager (rea contracts).
- **Obligation ledger**: reconstruction/verification checklists as a definition-of-done
  gate for refactors (rea `verify_reconstruction`).
- **Unknowns ledger**: explicit `record_unknown`/`verify_unknown_resolution` workflow
  for investigations (rea).

## Continuous
- Expand wire adapters toward opencode's 45-protocol breadth (Bedrock, Vertex, Mistral
  native, Responses API).
- Prompt-cache warming pings (opencode) and cache-aware compaction boundaries.
- Skills marketplace with compat gates (ECC `skill-host-compat`).
