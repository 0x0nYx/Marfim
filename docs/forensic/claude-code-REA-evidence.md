# TARGET C — REA run log against the Claude Code installation (evidence → spec)

- **Date:** 2026-11-03. **Operator:** Marfim forensic sub-agent (Claude).
- **REA:** source checkout `/home/z/my-project/work/rea` at commit `9ebbe67e05af08b567e215340da993c08dae378f`
  (rea-agents 6.8.0), built from source in this run. No proprietary code was extracted from the
  Claude Code npm bundle; all recorded observations are **metadata only** (file names, counts,
  digests, identifiers, error contracts). Raw JSON artifacts are preserved under
  `analysis/evidence/`.
- **Target:** `/home/z/.npm-global/lib/node_modules/@anthropic-ai/claude-code`
  (`@anthropic-ai/claude-code` 2.1.317; `claude --version` → `2.1.317 (Claude Code)`).

## 1. What was run (exact commands, in order)

```text
# (1) Install from the locked dependency set — succeeded
cd /home/z/my-project/work/rea && npm ci --no-audit --no-fund --loglevel=error
  → "added 739 packages in 15s" (runs rea-agents@6.8.0 prepare hook)

# (2) Build the runtime + skill artifacts — succeeded
npm run build:cached
  → turbo: compile (tsc -p tsconfig.build.json + catalog digests + MCP catalog +
    bundle-runtime) and artifacts:skills; "Tasks: 3 successful, 3 total"; ~15s

# (3) Confirm CLI and enumerate the surface
node scripts/rea.mjs --version        → 6.8.0
node scripts/rea.mjs --help           → 180 command groups printed (full catalog)

# (4) Non-interactive MCP server check (stdio JSON-RPC), no MCP client library needed:
printf '%s\n%s\n%s\n' \
 '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"forensic-probe","version":"1.0"}}}' \
 '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
 '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
 | node scripts/rea.mjs --mcp
  → initialize result: serverInfo {name:"rea", version:"6.8.0"}, capabilities
    {completions, prompts{listChanged}, tools{listChanged}}, full instructions text
  → tools/list: 234 tools (names saved to analysis/evidence/rea-mcp-tool-names.json)

# (5) Readiness / catalog identity
node scripts/rea.mjs doctor --json  > analysis/evidence/doctor.json

# (6) MCP tool invocations (initialize → initialized → tools/call ×2), driven by a
#     20-line Node stdio client (sequential writes, read all responses):
tools/call binary_session            {detail:"summary"}
tools/call inspect_desktop_application {path:"<CC install dir>", max_entries:20000}
  → raw transcript: analysis/evidence/mcp-calls-raw.txt

# (7) Static analysis tools, CLI surface (equivalent operations, JSON evidence to files):
node scripts/rea.mjs inspect-desktop-application "$CC" --json
  > analysis/evidence/desktop-app.json
node scripts/rea.mjs inspect-artifact "$CC/bin/claude.exe" --json
  > analysis/evidence/artifact-claude-exe.json
node scripts/rea.mjs analyze-javascript-application "$CC" --json
  > analysis/evidence/js-analysis.json
# ($CC = /home/z/.npm-global/lib/node_modules/@anthropic-ai/claude-code)
```

- The `npm i -g rea-agents` / `npx rea-agents` fallbacks were **not needed**: `npm ci` and the
  source build succeeded on Node v24.22.2 / npm 11.19.1.
- Interactivity findings: the MCP server is fully non-interactive over stdio (initialize →
  tools/list → tools/call all answered on one pipe); no GUI agent, display, or native analysis
  engine (Hopper/Ghidra/IDA) was required for the static tools used here.

## 2. Tool catalog evidence (identity pinning)

From `doctor.json` (`identity` section):

- `cli_package_version`: 6.8.0; `expected_skill_version`: 59; MCP SDK 2.3.1.
- `catalog.counts`: `cli_commands` 180, `mcp_tools` 234, `mcp_prompts` 6.
- `catalog.digests`: tools_sha256 `89198e2a67efee6b6c0b166486cca2ebe03c41e292e0ce7949cf1fd541000c91`,
  prompts_sha256 `0a526588…`, combined_sha256 `ea74ffec…` — a client can pin this exact surface.
- `scope.mode`: audit-wide; `skill.state`: missing (expected: setup not run); overall
  `healthy: false` solely because no agent clients/providers are registered in this sandbox —
  the doctor's remediation text says to run `rea setup` (deliberately not done: read-only
  discipline; no configuration was modified anywhere).
- `binary_session` (MCP, `detail:"summary"`) confirmed at runtime: `open:false`,
  `investigation.evidence_records: 0`, `cleanup_pending:false`,
  `maximum_representation_bytes: 550,725,158`, provider capability rows for `native-macos`,
  `rea-artifact-graph`, `rea-dotnet-static`; `tool_availability` = 17 groups naming all 234
  tools, **117 available** in this environment (no native/ADR providers installed); reported
  `client_features` for the probe client: elicitation_form/url, roots, sampling all `false`;
  `server_identity` = rea-agents 6.8.0 at the source checkout.

## 3. Evidence artifacts obtained from the Claude Code installation

### 3.1 `inspect_desktop_application` on the npm package directory

- Via MCP `tools/call`: evidence_id `ev_7e33ff85db41da1fb7f854ab40bdeefc04ff0f6d2450b69c81404e844cd093b3`,
  coverage `{"status":"complete-within-view","examined":15,"total":15,"exhausted":true}`.
- Via CLI (complete record, `desktop-app.json`): evidence_id
  `ev_05c7c7b2cb00b942576944d8f107feda8a61bbd2d5884b2ba43561f23aac9803`; provider
  `rea-artifact-graph` v1; `authority: "shipped-artifact"`, `confidence: "observed"`.
- Directory SHA-256 (`9d9ed7d0…` — commits retained paths + omission kinds, not every byte).
- **File inventory (15 entries, paths only):** `LICENSE.md`, `README.md`, `bin/`,
  `bin/claude.exe`, `cli-wrapper.cjs`, `install.cjs`, `node_modules/`,
  `node_modules/@anthropic-ai/`, `node_modules/@anthropic-ai/claude-code-linux-x64/`,
  `…/claude-code-linux-x64/LICENSE.md`, `…/README.md`, `…/claude-code-linux-x64/claude`,
  `…/claude-code-linux-x64/package.json`, `package.json`, `sdk-tools.d.ts`.
- Coverage: 15 entries examined, 2 files probed, 152 bytes read, 0 symlinks, 0 omissions.
- Findings: **no Electron/Unity/.NET/Unreal/Godot/GameMaker runtime candidates detected** — the
  npm layout is a thin wrapper (JS install/wrapper scripts + a native platform binary), not an
  Electron app. Six recorded limitations (e.g. "Runtime candidates and component markers do not
  establish loaded code, execution, or build correspondence").

### 3.2 `inspect_artifact` on `bin/claude.exe` — a fail-closed capacity result

- Exit code 1 with the canonical REA error contract (`artifact-claude-exe.json`):
  - `code: "truncated"`, `category: "truncated"`, `boundary: "capacity"`, `retryable: false`.
  - Message: "Bun standalone readable entries declare **91,770,668,526** plaintext bytes,
    exceeding this graph's **91,733,049,000**-byte expansion budget. This release has no runtime
    override. Host memory and NODE_OPTIONS do not raise it."
  - Remediation: "Use an artifact within the capacity reported in details.detail. Changing
    output detail or retrying the unchanged input does not remove this boundary."
- Interpretation (evidence, not conjecture): REA identified `bin/claude.exe` as a **Bun
  single-file standalone executable** (consistent with the observed 331 MB ELF x86-64 with a
  hard-linked copy in the platform package) and measured its embedded, compressed module
  payload's declared expanded size before refusing to expand it, by design.
- What this demonstrates for Marfim: REA's guarantee that oversized analyses fail **closed** with
  a precise budget, an explicit boundary class, and a remediation — never a partial silent result.

### 3.3 `analyze_javascript_application` on the package directory (static, inert)

- evidence_id `ev_53fc0887714b6d6f4903ee84f348402618f1d1ca9a80196ee753f178dd225dcf`;
  subject digest (directory) `a5ed8e5c…`; provider `rea-javascript-application`.
- **Statistics** (`normalized_result.statistics`): relevant_files 5; parsed_javascript_files 3;
  skipped 0; parse_failures 0; visited_ast_nodes 7,478; findings 121; text_bytes_read 188,174;
  modules 0; nested_asar_containers 0.
- **Application graph:** 27 nodes (19 `javascript-module`, 3 `javascript-asset`, 3 `artifact`,
  2 `package`), 41 edges; semantic graph: 943 nodes, 674 relations, **524 explicit unknowns**,
  11 fingerprints; `coverage.status: "unknown"` with per-family partial declarations.
- **Route inventory sources:** `cli-wrapper.cjs`, `install.cjs`, `sdk-tools.d.ts` (identifiers
  only; no source content retained in this log).
- Recorded limitations (5): relationships recovered "from inert syntax"; "JavaScript and HTML
  were parsed as inert text; bundle bootstrap code was never executed"; static imports do not
  prove runtime execution; dynamic/obfuscated expressions may stay unresolved; bundler factories
  recovered from AST literals without invoking handlers.
- Electron surface summary all zeros (browser_windows, preload entrypoints, context_bridge APIs,
  IPC operations, native add-on bindings) — corroborating 3.1: no Electron app layer in the npm
  package.

## 4. Invocation ledger (tool → arguments → outcome summary)

| # | Surface | Tool | Arguments | Outcome (summary) |
|---|---|---|---|---|
| 1 | stdio MCP | `initialize`/`initialized` | protocolVersion 2025-06-18 | serverInfo rea 6.8.0; capabilities + instructions |
| 2 | stdio MCP | `tools/list` | `{}` | 234 tools with input/output schemas (~4 MB frame) |
| 3 | CLI | `doctor` | `--json` | catalog counts/digests; readiness audit (no writes) |
| 4 | stdio MCP | `tools/call binary_session` | `{detail:"summary"}` | session state, 17 availability groups / 234 names, 117 available; client_features; server_identity |
| 5 | stdio MCP | `tools/call inspect_desktop_application` | `{path, max_entries:20000}` | Evidence ev_7e33ff85…; 15/15 entries, complete-within-view |
| 6 | CLI | `inspect-desktop-application` | `<CC dir> --json` | Evidence ev_05c7c7b2…; 15-entry inventory; no app-framework candidates |
| 7 | CLI | `inspect-artifact` | `<CC>/bin/claude.exe --json` | fail-closed `truncated`/capacity error: Bun standalone, ~91.77 GB declared expansion |
| 8 | CLI | `analyze-javascript-application` | `<CC dir> --json` | Evidence ev_53fc0887…; 3 JS files parsed, 7,478 AST nodes, graph 27n/41e, 524 unknowns |

Notes:
- First MCP `tools/call` attempt (shell-printf driver) produced no response lines; re-driven with
  a sequential Node stdio client both calls succeeded — a useful reminder that REA requires
  newline-delimited JSON-RPC and tolerates slow clients, but a driver bug looks like a hang.
- No native analysis engine was needed; none of the runs executed, unpacked, or deobfuscated
  Claude Code's proprietary binary beyond header/range reads and inert JS parsing of the two
  small wrapper scripts.

## 5. CLEAN-ROOM SPEC — Claude Code core agent-loop behavior

Written entirely from public behavioral documentation (CLI --help, public repo README /
CHANGELOG / examples / mods / shipped `sdk-tools.d.ts`). Suitable as a starting point for
independent reimplementation; identifiers are interface facts, not copied code.

1. **Session loop.** A session is a persistent, resumable conversation bound to a working
   directory: interactive by default; `claude -p` runs one turn and exits; sessions have stable
   IDs, optional names, and can be continued (`--continue`), resumed by id/name (`--resume`),
   forked (`--fork-session`), or restored from checkpoints (`/rewind`).
2. **System prompt assembly.** The final system prompt is composed from a base prompt plus
   dynamic per-machine sections (cwd, environment, git status, memory paths); per-machine
   sections can be moved into the first user message for prompt-cache reuse
   (`--exclude-dynamic-system-prompt-sections`); the rendered prompt can be snapshotted and
   reused verbatim across requests/resumes until compaction (`--system-prompt-snapshot`).
3. **Context injection.** CLAUDE.md-family memory files are discovered up the directory tree
   (and in `--add-dir` directories), with AGENTS.md as a mod-configurable equivalent
   (default: AGENTS.md is read where a project has no CLAUDE.md); @-mentions and file-change
   notices attach as the session progresses.
4. **Tool surface.** A fixed core tool set (Bash, Read, Write, Edit, Glob, Grep, NotebookEdit,
   TodoWrite, WebFetch, WebSearch, Agent/Task, Task* family, AskUserQuestion, plan-mode
   enter/exit, MCP plumbing) plus namespaced `mcp__server__tool` additions; each tool's input is
   a published JSON Schema (`sdk-tools.d.ts`); availability is curatable via `--tools`,
   `--allowedTools`, `--disallowedTools`, and settings rules like `Bash(git *)`.
5. **Permission engine.** Every tool call passes a permission layer combining: mode
   (acceptEdits/auto/bypassPermissions/manual/dontAsk/plan), allow/ask/deny rules, optional Bash
   sandboxing (with network allowlist), `--restricted` mode (no code-running tools or WebFetch;
   file tools confined to working directories; settings writes need human approval), and managed
   policy that can remove user/project overrides.
6. **Plan mode.** A read-only planning state entered/exited via dedicated tools/commands; the
   plan is presented for approval before mutations resume (ExitPlanMode tool; plan restoration
   on resume).
7. **Hooks.** Deterministic lifecycle interception at events including PreToolUse/PostToolUse,
   UserPromptSubmit, Stop/SubagentStop/SubagentStart, SessionStart/SessionEnd, PreCompact,
   PermissionRequest/Denied; classic command hooks (exit code 2 blocks) and plugin function
   hooks (chained `($, e, next)` with tier ordering and deny/refuse outcomes) both exist; hook
   failures can block (`onFailure: "block"`).
8. **Compaction.** Long conversations auto-compact inside a configurable window
   (`--autocompact auto|100k–1M`); compaction must preserve prior summaries and resume state;
   memory-size changes are detected to avoid prompt-too-long errors after compaction.
9. **Streaming protocol.** Headless/SDK hosts consume newline-delimited stream-json messages
   (assistant/user/system/tool-result events, optional partial deltas, hook lifecycle events,
   replayed user messages) and can drive multi-turn input via `--input-format stream-json`.
10. **Session persistence.** Transcripts, task lists, file-history/checkpoint backups, and
    per-project state persist under the user's config root with `cleanupPeriodDays` pruning;
    `claude purge` removes a project's data; persistence can be disabled for ephemeral runs.
11. **Background execution.** Sessions can run detached (`--bg`) with an agent/list lifecycle
    (agents, attach, logs, stop/kill, rm, respawn); shell commands can run in background with
    output retrievable; parallel work is isolated in git worktrees (`--worktree`, `--tmux`).
12. **Model/effort routing.** Model aliases resolve per account/region with fallback chains
    (`--fallback-model`), effort levels (low→max) scale reasoning; subagents can run a different
    model/effort; `--max-budget-usd` caps spend; betas are client-controlled.
13. **MCP integration.** Servers register via settings/`.mcp.json`/`--mcp-config` across stdio/
    SSE/HTTP/WebSocket transports; project `.mcp.json` servers require explicit approval;
    OAuth login exists for HTTP/SSE servers; enterprise config can allow/deny servers wholesale.
14. **Extension loading.** Plugins (manifest + commands/agents/skills/hooks + optional bundled
    MCP servers) load from marketplaces or directories (`--plugin-dir/--plugin-url`), can be
    enabled/disabled/configured, and are statically validatable and testable; the same shape
    carries built-in "mods".
15. **Checkpointing.** File edits are checkpointed so conversations or files alone can be
    rewound (`/rewind`, `--rewind-files`), with bounded on-disk backups pruned over time.
16. **Trust & safety UX.** Workspace trust dialog (skipped in `-p`), permission prompts with
    "don't ask again" semantics, an email-privacy check before web/shell calls send account
    email, and telemetry/feedback opt-outs via settings/env.
17. **Exit discipline.** Headless runs exit non-zero on infrastructure failures (e.g. transcript
    or config writes failing), not just on model errors.
18. **Degradation etiquette.** Customization-free modes (`--safe-mode`, `--bare`,
    `--restricted`, `--setting-sources`) let an operator bisect a broken configuration without
    uninstalling; admin/policy settings still apply in safe mode.

**Correlation with the REA evidence (§3):** items a reimplementation can verify from shipped
artifacts alone — the Linux distribution is a compiled native executable (Bun standalone
per REA's format detection), wrapped by a thin npm installer (`install.cjs`, `cli-wrapper.cjs`),
with a published `sdk-tools.d.ts` tool-schema file and per-platform binary packages — match
public knowledge that current builds compile the CLI to a native binary per platform.
