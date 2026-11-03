# Marfim Architecture

## Design thesis

Ten harnesses converge on the same shape with different emphasis. Marfim keeps the
convergent core deliberately thin and pushes capability into five pluggable seams:

```
                     ┌──────────────────────────────────────────────┐
   config TOML ─────▶│  Config (layered: defaults<global<project<CLI)│
                     └──────────────┬───────────────────────────────┘
                                    │
        ┌───────────────┐   ┌───────▼────────┐   ┌──────────────────┐
        │ memory/skills │──▶│    Agent loop  │◀──│ PermissionEngine │
        │ (MARFIM.md,   │   │  (agent/loop)  │   │ (modes + rules + │
        │  SKILL.md)    │   └───┬───────┬────┘   │  config-protect) │
        └───────────────┘       │       │        └──────────────────┘
                                │       │
             ┌──────────────────▼─┐   ┌─▼────────────────────┐
             │ Provider adapters  │   │ Tool registry        │
             │ openai-compat      │   │ 13 built-in tools    │
             │ anthropic          │   │ + MCP bridge tools   │
             │ mock (tests)       │   │ + hooks (exit-2 veto)│
             └────────────────────┘   └──────────────────────┘
                                │       │
                     ┌──────────▼───────▼──────────┐
                     │ Session store (JSONL)       │
                     │ messages·usage·grants·      │
                     │ todos·snapshots(rewind)     │
                     └─────────────────────────────┘
                                │
                     ┌──────────▼──────────────────┐
                     │ Surfaces: REPL / headless / │
                     │ subagents (task tool)       │
                     └─────────────────────────────┘
```

## The five seams

1. **Provider seam** — `providers/base.py` defines the event protocol
   (`text_delta`, `tool_call`, `done`, `error`). Any wire protocol is an adapter.
   v0.1 ships openai-compat + anthropic + mock; the 45-protocol depth of opencode's
   `packages/ai` is the proven upgrade path.
2. **Tool seam** — `tools/base.py`: `Tool(name, description, parameters, readonly,
   handler)` + `ToolResult` with mandatory boundedness. Tool planning per turn is a
   pure function of config+context (codex `spec_plan` doctrine). MCP servers inject
   tools through the same seam — the model cannot tell a subprocess MCP tool from a
   native one (goose uniform-surface doctrine).
3. **Policy seam** — every mutation funnels through `permissions.check()` returning
   an explicit ruling. Hooks (exit-2) veto *before* the engine; the engine orders
   deny > config-protect > allow/grants > mode logic > ask-fallback. Nothing
   executes without a ruling — the goose effect-machine insight, simplified to a
   synchronous ruling object.
4. **State seam** — one JSONL file per session records everything (messages, usage,
   grants, todos, summaries, snapshots). Crash-safe append; resume is re-reading.
   Snapshots give file-level `/rewind` (claude code checkpoints).
5. **Surface seam** — REPL, headless, and subagents all drive the same
   `Agent.run_turn`. Headless adds no new semantics except ask→deny (documented,
   claude-code doctrine).

## Key decisions and their provenance

- **Synchronous stdlib-only core.** No asyncio, no third-party deps → installable
  anywhere, testable everywhere. Streaming is generator-based; SSE parsing is a
  simple line protocol. (Aider runs on the same philosophy of boring reliability.)
- **Byte-stable prompt assembly** (`agent/prompts.py`): identity → memory → skills
  index → discipline → environment, fixed order (hermes prompt-cache law).
- **GateGuard in the tool, not the prompt**: the edit/patch tools *structurally*
  refuse unread files instead of trusting a system-prompt rule (ECC's hook, made native).
- **Compaction as two layers**: deterministic pruning of old tool payloads first
  (hermes), LLM summarization second (goose/opencode), tail kept verbatim.
- **Steering queue**: input typed mid-turn is captured and delivered between tool
  batches as a `[mid-turn steering]` user message (opencode v2 inbox, reduced).
- **Cost honesty**: usage records live in the session file; `/usage` prices them via
  models.dev — estimates are labeled as estimates.

## Data formats

- Session record (one JSON per line): `{"t":"meta|message|todo|summary|usage|grant|snapshot", ...}`
- Config: TOML, keys documented in `examples/config.toml`.
- Skill: `<dir>/SKILL.md` with YAML frontmatter (`name`, `description`).
- Hook contract: JSON payload on stdin, `MARFIM_EVENT`/`MARFIM_TOOL` env, exit 2 = veto.

## Testing strategy

44 unittests in 4 suites: unit (config/permissions/tools/patch), integration
(agent loop against the mock provider — full turn lifecycle incl. permissions,
retries, checkpoints, subagents), contract (MCP JSON-RPC against a fake server
process; SSE parsers against a canned HTTP server). CI: GitHub Actions on 3.11/3.12.
