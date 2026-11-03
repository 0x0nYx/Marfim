# Forensic Analysis: Oh My Pi (https://github.com/can1357/oh-my-pi)

**Analyzed commit:** `83de60ad992d12e572ec3ed6fe2dd9e160cdc4a` — "fix(coding-agent): keep composer prediction across silent advisor cards" (AuthorDate 2026-11-03, author "Brit"; repo is a shallow clone, full history not available). Repo remote verified as `https://github.com/can1357/oh-my-pi`; homepage `https://omp.sh/`; workspace version `18.16.0` (root `Cargo.toml`, `packages/coding-agent/package.json`).

**License:** MIT. Copyright lineage is unusual and confirms the fork claim: "Copyright (c) 2025 Mario Zechner" (badlogic, upstream Pi), "(c) 2025-2026 Can Bölük", "(c) 2026 Stencil Labs, Inc." (root `LICENSE`). README states it is a "Fork of Pi by @mariozechner"; `packages/coding-agent/src/extensibility/legacy-pi-*-shim.ts` files provide backward compatibility with the Pi surface.

**Primary languages:** TypeScript (~1.0M lines non-test under `packages/*/src`, dominated by coding-agent ≈530k, tui ≈199k, ai ≈140k) and Rust (≈433k lines non-vendor: pi-builtins ≈182k, pi-natives ≈109k, pi-shell ≈43k, pi-edit ≈26k, pi-vcs/pi-walker/pi-ast ≈12k each). Vendored crates under `crates/vendor/` excluded. Runtime is Bun ≥1.4.4 (`packages/coding-agent/package.json` engines); monorepo uses Bun workspaces plus Bazel `BUILD.bazel` files for Rust/native CI builds. README's "~80k lines of Rust core" understates reality — that figure covers only a subset.

**Scale markers (README.md):** "60+ providers · 31 built-in tools · 14 lsp ops · 28 dap ops". Verified in-repo: 81 catalog providers (`packages/catalog/src/models.json`), 30 visible + 3 hidden built-in tools (`packages/coding-agent/src/tools/builtin-names.ts`), 14 LSP actions (`docs/tools/lsp.md`), 28 DAP actions (`docs/tools/debug.md`).

## 1. Identity & Purpose

- Terminal-first coding agent CLI named `omp` ("A coding agent with the IDE wired in", README.md; `APP_NAME` in `packages/utils/src/dirs.ts`). Distributed as npm `@oh-my-pi/pi-coding-agent`, Homebrew tap, Nix flake, or SHA-256-checked prebuilt binaries with the Rust addon embedded (`scripts/install.sh`, `docs/natives-architecture.md`).
- The "IDE wired in" claim is literal: LSP servers, DAP debug adapters, hash-anchored editing with parse-repair, and a persistent shell are first-class subsystems of the agent loop rather than external helpers (`packages/coding-agent/src/lsp/`, `src/dap/`, `crates/pi-edit/`, `crates/pi-shell/`).
- It is an aggressively performance-obsessed fork: the Rust native layer (`crates/pi-natives`, `crates/pi-edit`, `crates/pi-iso`, `crates/pi-walker`, `crates/pi-shell` — a full bash interpreter via vendored brush) exists to move hot paths out of TypeScript (`docs/natives-architecture.md`, `docs/native-crates.md`).
- Descends from badlogic's pi coding agent (pi-mono); Pi compat is a supported discovery provider (`docs/config-usage.md` lists canonical roots `.omp`, `.claude`, `.codex`, `.gemini`).
- Distribution of concerns: `packages/agent` (agent loop core), `packages/ai` (provider transports), `packages/catalog` (model/provider catalog + compat rules), `packages/tui` (terminal UI toolkit), `packages/coding-agent` (the product), `packages/natives` (N-API bridge).

## 2. Architecture

**Package layout** (all under `packages/`, names from each `package.json`):

| Package | Role |
|---|---|
| `agent` (`@oh-my-pi/pi-agent-core`) | Provider-agnostic agent loop, steering, compaction engine (`packages/agent/src/agent-loop.ts`, `agent.ts`, `compaction/`) |
| `ai` (`pi-ai`) | 16 built-in API transports, OAuth, auth cascade, retries, dialect layer (`packages/ai/src/providers/`, `src/auth/`) |
| `catalog` (`pi-catalog`) | Model/provider catalog, pricing, per-provider compat rules as KDL (`packages/catalog/src/compat/rules/*.kdl`, 243 .kdl files) |
| `coding-agent` | The product: tools, sessions, TUI modes, config, extensions, MCP, tasks, memory, eval kernels |
| `tui` (`pi-tui`) | Terminal UI toolkit: components, overlays, themes, apps (`packages/tui/src/`) |
| `natives` (`pi-natives`) | Rust N-API addon loader + generated bindings (`packages/natives/native/index.d.ts`, `crates/pi-natives`) |
| `snapcompact` | Compaction strategy that archives history as dense bitmap images (`packages/snapcompact/src/snapcompact.ts`) |
| `mnemopi` | Local SQLite vector memory backend (`packages/coding-agent/src/mnemopi/`) |
| `wire` (`pi-wire`) | Collab protocol + Tern Surface Protocol wire types (`packages/wire/src/index.ts`, `tsp.ts`) |
| `metaharness` | Benchmark harness/dashboard for edit/compaction evals (`packages/metaharness/README.md`) |
| `browser-relay`, `collab-web`, `stats`, `omptype`, `utils` | Browser relay service, web collab client, usage stats, schema-builder shims, shared utilities |

**Rust crates** (`crates/`): `pi-natives` (N-API surface: edit/AST/search/shell/PTY/iso/desktop), `pi-edit` (hashline edit engine + grammars), `pi-builtins` (a shell-builtin library incl. a full rsync implementation), `pi-shell` (brush-based bash with command-output minimizer defs in `src/minimizer/defs/*.toml`), `pi-iso` (CoW isolation: apfs/btrfs/zfs/overlayfs/projfs/rcopy backends), `pi-walker` (parallel file walk/grep), `pi-ast` (tree-sitter ast-grep ops), `pi-vcs` (git), `pi-predict` (n-gram + SmolLM composer text prediction), `pi-voice`, `pi-vfs`, `pi-diff`.

**Entry points:** `packages/coding-agent/src/cli.ts` (bin `omp`) → `main.ts` (arg parsing, ~2.9k lines) → `sdk.ts` (`createAgentSession`, the shared runtime, ~530k-line package's hub) → mode runners in `src/modes/` (`interactive-mode.ts`, `print-mode.ts`, `rpc/rpc-mode.ts`, `acp/acp-mode.ts`).

**Data flow (text diagram):**

```
 user input (TUI composer / RPC / ACP / SDK / collab guest)
        │
        ▼
 AgentSession ── queues: steer() / followUp() / aside   (src/session/agent-session.ts)
        │ transcript (JSONL session file + entries)
        ▼
 AgentLoop (packages/agent/src/agent-loop.ts)
   ├── system prompt blocks + tool schemas   (src/system-prompt.ts, src/tools/index.ts)
   ├── pi-ai stream: 81 providers via 16 API transports (packages/ai/src/)
   ├── tool dispatch → approval gate → tool execute (src/tools/approval.ts)
   │      ├── built-ins (read/bash/edit/lsp/debug/task/eval/...)
   │      ├── MCP tools (src/mcp/tool-bridge.ts)
   │      └── extension/custom tools (src/extensibility/extensions/wrapper.ts)
   ├── live steering + speculative execution (packages/agent/src/live-steering.ts,
   │                                          speculative-execution.ts)
   └── maintenance: compaction / shake / snapcompact (packages/agent/src/compaction/)
        │
        ▼
 events → TUI renderers / RPC frames / ACP mapper / collab relay
```

**PAL:** the "platform abstraction layer" named in docs is the native bridge: `@oh-my-pi/pi-natives` exposes `EditSession`, `EditStore`, ast-grep, search, shell/PTY, and isolation primitives to TS (`docs/natives-architecture.md`, `docs/natives-binding-contract.md`); `crates/pi-iso` is the isolation PAL behind `isoResolve/isoStart/isoStop` (docs/tools/task.md). The addon ships embedded in release binaries with a version sentinel guarding against mismatched addon/JS pairs (`packages/natives/native/embedded-addon.js`, `version-sentinel.js`, `docs/natives-addon-loader-runtime.md`).

**Agent core (`packages/agent`) internals:** the loop is driven off `AgentContext` + `AgentLoopConfig` (`packages/agent/src/types.ts`, 1,317 lines of contracts incl. `BeforeToolCallResult`, `SteeringQueueState`, `AsideMessage`); `agent.ts` (2,347 lines) wraps the loop into the `Agent` object with follow-up/steer queues and event emission. Supporting modules: `output-budget.ts` (token budgeting incl. image tokens via `image-tokens.ts`), `tokenizer.ts`, `thinking.ts` (reasoning-effort levels), `append-only-context.ts` (a mode where context only grows), `run-collector.ts` (per-run coverage + `ToolCallBlockedError`), `proxy.ts`, `telemetry.ts` (OpenTelemetry spans: chat/execute-tool/invoke-agent), `sent-tool-definitions.ts` (tracks what schema the provider last saw), `pause.ts` (global pause gate).

**`packages/ai` internals:** streaming with first-event/idle timeouts per provider (`src/providers/register-builtins.ts`), `error/` taxonomy (`isContextOverflow` etc.), `oneshot-retry.ts`, provider session state for resumable streams (`src/provider-session-state.ts`), dialect layer for in-band tool calling (`src/dialect/`), embeddings/rerank/speech/transcription/video sub-surfaces (`src/embeddings/`, `src/rerank/`, `src/speech/`, `src/transcription/`, `src/video/`), and a "judgment" module for LLM-as-judge (`src/judgment/`). Message transforms (`src/providers/transform-messages.ts`) handle tool-call-id de-duplication, assistant survival rules, and provider-specific sanitization.

## 3. Agent Loop

- Core: `packages/agent/src/agent-loop.ts` (4,119 lines) — single loop over `AgentMessage`s, converting to provider `Message[]` only at the LLM boundary. Turn structure: dequeue steering → optional pre-model transforms (`agent_pre_model_call`) → provider stream → tool-call batch execution → repeat until stop boundary.
- **Turn lifecycle detail:** the outer loop re-checks steering at every start (messages may arrive while waiting on the provider, `agent-loop.ts` ~1250); replayed/pending tool results from a previous interrupted run are executed before the first model call so steering stays parked until the batch finishes (~1277). A stop boundary only continues the turn for live user steering; asides fold into the next turn alongside it (~1793-1804). A recovered message (harmony/DSML leak recovery) completes the turn and resets the abort-retry counter (~1499).
- **Aside messages:** transient context (advisor cards, hidden notices) ride `AsideMessage`s with explicit COMMIT/DISCARD contracts (`packages/agent/src/types.ts` `ASIDE_MESSAGE_COMMIT`/`ASIDE_MESSAGE_DISCARD`); silent advisor cards must not reset composer prediction state (subject of the analyzed head commit).
- **Steering** is two-queue: `steer()` (interrupting) and `followUp()` (turn boundary), each with `"all" | "one-at-a-time"` modes (`packages/coding-agent/src/session/agent-session.ts` getters near line 7536). Live steering can be injected into the provider's *in-flight* response via `LiveSteeringChannel` and `offerSteeringToProvider` (`packages/agent/src/live-steering.ts`, `agent-loop.ts` ~line 1900) — latency-sensitive claims convert only the steering batch, not the transcript.
- Tool batches get three abort controllers: user abort, steering interrupt (`steeringAbortController`), and a *soft* steering signal tools may opt into (e.g. auto-backgroundable bash) (`agent-loop.ts` ~3288-3304). Pure-wait tools (`wait`, vibe) are interruptible; partially streamed results are preserved.
- **Retries:** classification/backoff/credential rotation/model fallback in `packages/coding-agent/src/session/turn-recovery.ts`, orchestrated from `agent_end` (`docs/non-compaction-retry-policy.md`). Context-overflow errors are deliberately excluded from retry and routed to compaction.
- **Compaction** is a method *order*, not one algorithm (`compaction.methodOrder`): provider-native compaction (Anthropic, OpenAI Codex/OpenAI, Bedrock — `packages/agent/src/compaction/{anthropic,openai,bedrock,azure-openai-endpoint}.ts`), snapcompact (history → bitmap images), summarization, handoff, plus `shake` — mechanical elision of heavy tool-result text with a protected tail (`packages/agent/src/compaction/shake.ts`, `DEFAULT_SHAKE_CONFIG` keeps 16k tokens, min 4k savings). Mid-turn threshold checks run before each provider request (`docs/compaction.md` §"Mid-turn threshold maintenance"); subagents always pin mid-turn on (`packages/coding-agent/src/session/session-context.ts` per docs).
- **Speculative execution:** `SpeculativeOperationCoordinator` (`packages/agent/src/speculative-execution.ts`, 968 lines) and task-side `speculative-launch.ts` — the loop pre-launches work on predicted next tool calls (composer prediction is the visible tip; the last commit fixes exactly this interplay with advisor cards).
- Tool arguments are streamed and validated incrementally (`AgentToolArgStream` in `packages/agent/src/types.ts`); schema descriptions can be stripped provider-side (`stripSchemaDescriptions` import in agent-loop.ts).
- Provider-leak hardening is in-loop: harmony-XML/DSML tool-markup leak detection and recovery with audit events and retry escalation (`agent-loop.ts` imports from `pi-ai/utils/harmony-leak`, `dsml-leak`; `docs/ERRATA-GPT5-HARMONY.md`).

## 4. Tool Set

Canonical registry: `BUILTIN_TOOL_NAMES` + `HIDDEN_TOOL_NAMES` in `packages/coding-agent/src/tools/builtin-names.ts`; factories in `packages/coding-agent/src/tools/index.ts` (`BUILTIN_TOOLS` / `HIDDEN_TOOLS`, lines ~569-606). README's "31 built-in tools" ≈ the 30 visible names plus `think`. TTS and image-gen ship as *bundled custom tools* instead (`packages/coding-agent/src/sdk.ts` lines ~3872-3874; `src/tools/tts.ts`, `src/tools/image-gen.ts`). Browser automation is an eval *prelude facade*, not an AgentTool (`docs/tools/browser.md`: "It is not an AgentTool").

| Tool | File (under `packages/coding-agent/src/`) | Notes |
|---|---|---|
| `read` | `tools/read.ts` | Files, dirs, archives, SQLite, PDFs, images, URLs, `ssh://`/`xd://`/`skill://`/`memory://` internal URLs; hashline display |
| `write` | `tools/write.ts` | + `tools/write-content.ts`, `file-write-fallback.ts` |
| `edit` | `edit/index.ts` | Modes: hashline (default), apply_patch, patch, replace, sloppy (`edit/schemas.ts`, `crates/pi-edit/`) |
| `bash` | `tools/bash.ts` | Native shell runtime (`src/exec/`, `crates/pi-shell`), output minimizer, PTY selection, auto-background |
| `eval` | `tools/eval.ts` | Persistent Python + JS kernels (`src/eval/py/`, `src/eval/js/`), tool/subagent bridges |
| `task` | `task/index.ts` | Subagents, batch shape, isolation, async jobs (`src/task/*.ts`) |
| `grep` / `glob` / `find` | `tools/grep.ts`, `glob.ts`, `jfind/` | Native walker-backed search (`crates/pi-walker`) |
| `ast_grep` | `tools/ast-grep.ts` | Structural search (tree-sitter, `crates/pi-ast`) |
| `ast_edit` | `tools/ast-edit.ts` | Structural rewrite via native ast-grep (`crates/pi-natives/src/ast.rs`) |
| `lsp` | `lsp/tool.ts` | 14 actions: diagnostics/definition/references/hover/symbols/rename/rename_file/code_actions/type_definition/implementation/status/reload/capabilities/request |
| `debug` | `tools/debug.ts` | DAP; 28 actions (launch/attach/breakpoints/stepping/evaluate/memory/...); `src/dap/` |
| `github` | `tools/gh.ts` | gh CLI wrapper: repos, files, PRs, search, Actions run watch; per-repo write lock (`src/utils/repo-lock.ts`) |
| `web_search` | `web/search/index.ts` | ~30 search providers (`src/web/search/providers/*.ts`: brave, exa, kagi, perplexity, tavily, searxng, duckduckgo, jina, ...) |
| `ask` | `tools/ask.ts` | Structured user questions with pickers/previews/timeouts |
| `todo` | `tools/todo.ts` | Session todo phases (`src/session/todo-tracker.ts`) |
| `checkpoint` / `rewind` | `tools/checkpoint.ts` | Mark state / collapse exploratory context into a report; safety-paired auto-include |
| `context_notes` / `new_context` | `tools/context-notes.ts` | Experimental context management notes + fresh-context |
| `security_scan` | `tools/security-scan.ts` | SARIF-producing scanner (`src/security/`) |
| `memory_edit` / `retain` / `recall` / `reflect` | `tools/memory-*.ts` | Memory backend tool family; gated by `memory.backend` |
| `learn` / `manage_skill` | `tools/learn.ts`, `manage-skill.ts` | Capture lessons / author skills at runtime (`src/autolearn/`) |
| `ida` | `tools/ida.ts` | IDA Pro integration (install-gated, `src/ida/install.ts`) |
| `wait` | `tools/wait.ts` | Block until background job / peer message / steering interrupt |
| `think` (hidden) | `tools/think.ts` | External thinking scratchpad for models that support it |
| `yield` (hidden) | `tools/yield.ts` | Subagent work-pool yield protocol (`src/task/workpool-yield.ts`) |
| `goal` (hidden) | `goals/tools/goal-tool.ts` | Goal-mode persistence tool |
| `tts` (custom) | `tools/tts.ts` | Speech synthesis via `speech` role |
| `generate_image` (custom) | `tools/image-gen.ts` | Hosted image-generation relay |
| `browser` (eval prelude) | `tools/browser.ts` + `tools/browser/` | CDP/Puppeteer tab supervisor, relay/cmux/Tern backends, stealth patches (`patches/puppeteer-core@25.3.0.patch`) |
| `computer` (eval prelude) | `tools/computer.ts` + `tools/computer/` | Desktop computer-use with safety checks |
| `xdev` devices | `tools/xdev.ts` | Demotes discoverable tools into `xd://` devices driven through read/write |

Other docs/tools entries: `docs/tools/{wait,computer,generate_image,tts,new-context,context-notes}.md`. Every tool doc pairs an entry file with a model-facing prompt in `src/prompts/tools/*.md`.

**Tool infrastructure around the dispatch table:**
- Per-tool timeout clamps (`src/tools/tool-timeouts.ts`), approval-cache target reporting (`approval-targets.ts`), and an approval resolution pipeline that fails closed to `always-ask` without grants (`tools/approval.ts`).
- A session-wide file-mutation queue serializes conflicting writes and detects races (`src/tools/file-mutation-queue.ts`, `conflict-detect.ts`, `racy-stamp.ts`); conflict results are addressable via `cfg://` URIs (`conflict-uri.ts`).
- Oversized outputs spill into session artifacts with `agent://` addresses and truncation metadata (`src/task/output-manager.ts`, `src/session/artifacts.ts`, `tools/output-meta.ts`).
- Tool-choice directives can be queued per turn (`src/session/tool-choice-queue.ts`) — used by loop-redirect prompts (`src/prompts/tool-call-loop-redirect.md`) that push the model off repeating failed calls.
- Streaming tool-argument validation runs while the model is still emitting (`packages/agent/src/tool-arguments.ts`), enabling speculative execution of already-validated prefixes.

## 5. Model/Provider Layer

- **Catalog:** 81 providers in `packages/catalog/src/models.json` (anthropic, openai, openai-codex, google, google-vertex, google-gemini-cli, google-antigravity, xai, xai-oauth, moonshot, zai, zhipu-coding-plan, minimax(+cn/code), deepseek, qwen-portal, groq, mistral, together, fireworks, openrouter, github-copilot, gitlab-duo(+agent), cursor, devin, amazon-bedrock, bedrock-mantle, azure, ollama-cloud, local, venice, nanogpt, ...). Compat and policy rules are KDL files under `packages/catalog/src/compat/rules/`.
- **16 wire transports** (`packages/ai/src/api-registry.ts` `BUILTIN_API_IDS`): openai-completions, openai-responses, openai-codex-responses, azure-openai-responses, openrouter, anthropic-messages, bedrock-converse-stream, google-generative-ai, google-gemini-cli, google-vertex, ollama-chat, cursor-agent, factory-droid-agent, gitlab-duo-agent, devin-agent, apple-foundation-models. Extensions can register custom APIs (`registerCustomApi`).
- **Model roles** (`packages/coding-agent/src/config/model-roles.ts`, IDs in `packages/tui/src/overlays/model-browser.ts`): chat roles `default, smol, slow, vision, plan, commit, tiny, memory, task, advisor, widget`; kind roles `image, web, speech, dictation, judge`. Roles are resolved through selectors with `@` prefix (legacy `pi/` prefix accepted) and stored per-scope (`modelRoleStorage: project` writes to project config — docs/settings.md).
- **Provider ordering:** `modelProviderOrder` setting ranks providers when resolving ambiguous model ids (`packages/coding-agent/src/config/model-settings.ts` line 133; usage in `config/model-resolver.ts` ~line 2147); plus `enabledModels`, `enabledProviders`/`disabledProviders`, `cycleOrder`, `modelTags`.
- **BYOK/OAuth/local:** API keys via env, `.env`, `models.yml` (`docs/models.md` provider fields: baseUrl/apiKey/api/headers/discovery); OAuth flows in `packages/ai/src/registry/oauth/` (anthropic, openai-codex, google*, github-copilot, gitlab-duo, cursor, kimi, kilo, perplexity, device-code.ts); credential cascade/pools/rotation/health in `packages/ai/src/auth/` (`cascade.ts`, `pool.ts`, `rotation.ts`, `affinity.ts`); subscription plans (ChatGPT Codex backend, Claude, Gemini CLI, Antigravity, coding plans) are first-class providers. Local backends: ollama discovery (`docs/models.md` discovery.type ollama), a `local` catalog provider, and on-device inference (`src/tiny/local-inference-api.ts`, MLX runtime; `crates/pi-predict` GGUF SmolLM for composer prediction).
- Quota/reset tracking per subscription provider (`packages/ai/src/usage/openai-codex.ts`, reset management, `claude-auto-reset.ts`/`codex-auto-reset.ts` in `src/session/`).
- **Team credential infrastructure:** `omp auth-broker serve` centralizes the SQLite credential vault + OAuth refresh; `omp auth-gateway serve` is a forward proxy so clients (containerized agents, usage widgets) never see access tokens (`docs/auth-broker-gateway.md`; `packages/ai/src/auth-broker/`, `src/auth-gateway/`).
- **Cursor bridge:** a `cursor-agent` transport plus in-process tool handoff lets omp run behind Cursor's agent infrastructure (`packages/ai/src/providers/cursor.ts`, `src/cursor-bridge-tools.ts`, `docs/provider-quirks.md`).
- Model metadata: per-model tokenizer, thinking-effort clamping (`clampThinkingLevelForModel` in `packages/catalog/src/model-thinking.ts`), pricing/status (`packages/catalog/src/pricing.ts`), and measured performance samples feeding the model browser (`packages/tui/src/overlays/model-browser.ts` `ModelBrowserPerf`).

## 6. Permissions & Safety

- **Three-tier tool declaration:** every tool declares `approval: read | write | exec`; unknown/custom tools default to `exec`; MCP tools are forced `write` (`docs/approval-mode.md`; enforcement in `packages/coding-agent/src/tools/approval.ts`).
- **Modes** via `tools.approvalMode`: `always-ask`, `always-ask-cached` (grants files/folders, persisted to `approval-cache.db` via `tools/approval-cache.ts`), `write`, and default `yolo`. CLI `--approval-mode`, `--yolo` (`docs/approval-mode.md`).
- **Argument-dependent policy:** tools may return `policy: allow|deny|prompt` with `override` and `policyKey` (per sub-tool dispatch); user overrides `tools.approval.<tool>` can tighten but not bypass denies (`docs/approval-mode.md`, `tools/approval-targets.ts`, `search-approval-targets.ts`).
- **Restricted folders:** `tools.restrictedFolders` forces a prompt for anything inside listed paths regardless of mode, resolved through symlinks/case-folding (`docs/approval-mode.md` §Restricted folders).
- **Hook-based confirmation:** extension `tool_call` / before-tool events can block with a reason (`packages/coding-agent/src/extensibility/hooks/types.ts`, wrapper `extensions/wrapper.ts`); the `pi.on("tool_call", ...)` block contract is shown in docs/hooks.md.
- **Isolation/sandbox:** subagent CoW clones (see §10), read-only plans (`src/task/read-only-policy.ts`), plan-mode tool gating (`src/tools/plan-mode-guard.ts`), bash git-guard (`crates/pi-shell/src/git/guard.rs`), per-repo write locks, restricted `ulimit` isolation tests (`crates/pi-shell/tests/ulimit_isolation.rs`).
- **Secret obfuscation:** reversible `$$HASH$$` redaction of secrets in provider-visible text, restored in tool arguments; env-pattern + credential-shape regexes (`docs/secrets.md`, `src/memory-backend/redact.ts` for memory paths).
- Browser/computer exec-tier approval including inspection helpers, and provider safety checks surfaced into the approval pipeline (`docs/tools/browser.md`; `ComputerSafetyCheck` import in agent-loop.ts).

## 7. Sessions, State & Context

- **Storage:** one JSONL file per session at `<agentDir>/sessions/<timestamp>_<id>.jsonl`, branched children nest as `<parent>/<agentId>.jsonl` (`packages/coding-agent/src/session/session-manager.ts` lines ~380-393, ~2501). Secondary stores: `agent.db` SQLite (`src/session/agent-storage.ts`, `sql-session-storage.ts`, `indexed-session-storage.ts`), optional Redis (`redis-session-storage.ts`), artifact blob store (`src/session/blob-store.ts`, `artifacts.ts`; `docs/blob-artifact-architecture.md`).
- **Entries, not just messages:** typed session entries (compaction entries, custom messages, model usage, checkpoints) with branch trees, pins, export/share/fork/resume operations (`src/session/session-entries.ts`, `docs/session.md`, `docs/session-operations-export-share-fork-resume.md`). Foreign-session import for Claude/Codex (`foreign-session-*.ts`).
- **Compaction:** see §3; the session manager owns I/O, pure engine in `packages/agent/src/compaction/` (7.6k lines). Branch summaries capture abandoned `/tree` branches (`compaction/branch-summarization.ts`).
- **Steering queue state machine** in the loop: queued → classified (user vs system steering) → live-taken vs boundary-taken; on external abort, un-consumed steering is preserved (`agent-loop.ts` ~1770-1822, `steeringQueueState` ~1884).
- **Memory:** five backends — `off`, `local` (project summaries/lessons), `hindsight` (remote), `mnemopi` (local SQLite + embeddings, `src/mnemopi/`), `sharpshooter` (friction-gated decision files) — surfaced via `retain/recall/reflect/learn/memory_edit` tools and `memory://` URLs (`docs/memory.md`, `src/memory-backend/resolve.ts`).
- **Context files:** AGENTS.md/RULES.md/SYSTEM.md discovery across `.omp/.claude/.codex/.gemini` roots with `@import` support (`src/discovery/at-imports.ts`, `docs/context-files.md`); workspace tree snapshot injected into prompt footer and shared with subagents (`src/workspace-tree.ts`, ToolSession.workspaceTree).
- **Prompt cache engineering:** the system prompt is an ordered block array with a static prefix; the cache breakpoint lands before the first cwd-derived block so subagents in different worktrees share the cached prefix (`docs/system-prompt-customization.md` §Block layout; `src/system-prompt.ts`). Subagents inherit a parent `getPromptCacheKey` for sibling prompt-cache sharing (`src/tools/index.ts` ToolSession).
- **Session lifecycle services:** idle maintenance, cost gates bounding session-tree spend (`src/session/cost-gate.ts`), usage limits, provider boundary pinning, credential affinity seeding for child sessions, and exit diagnostics (`src/session/idle-maintenance.ts`, `usage-limit.ts`, `exit-diagnostics.ts`).
- **Turn recovery & continuity:** persisted turn state lets a crashed/interrupted turn resume (`src/session/turn-persistence.ts`, `turn-recovery.ts`); unexpected-stop detection classifies dead ends and injects retry contracts (`src/session/unexpected-stop-classifier.ts`, `src/prompts/unexpected-stop-retry.md`).
- **Internal URL space:** a router maps `ssh://`, `xd://`, `skill://`, `memory://`, `history://`, `artifact://`, `agent://`, `cfg://`, `rule://` onto typed resources readable by tools (`src/internal-urls/`, incl. `router.ts`, `skill-protocol.ts`, `history-protocol.ts` per docs/tools/read.md).

## 8. UI/UX

- **TUI:** custom toolkit (`packages/tui`): component renderer with diff-based redraw (`docs/tui-core-renderer.md`), vim mode, kill-ring, bracketed paste, kitty graphics/sixel images, mouse, keybindings + chord system (`src/keybindings.ts`, `docs/keybindings.md`), autocomplete, LaTeX-to-unicode preview, spelling, emoji autocomplete.
- **Composer:** inline text prediction via local n-gram/SmolLM (`crates/pi-predict`, `src/predict/`), composer shape system (`tui/src/overlays/composer-shape-*.ts`), magic keywords with animated highlights (`src/modes/magic-keywords.ts`: `ultrathink`, `orchestrate`, `workflowz`, `jevify` — each injects a hidden per-turn contract notice; `docs/magic-keywords.md`).
- **Overlays/apps:** model browser with measured tps/ttft perf (`tui/src/overlays/model-browser.ts`), agent hub / activity visualizers (`overlays/agent-hub.ts`, `apps/live-visualizer.ts`), context viewer, jobs panel, plan review, debug log/SSE viewers (`apps/debug/`).
- **Slash commands:** large builtin registry (`src/slash-commands/builtin-registry.ts` pulling modes/session/lifecycle/marketplace/skills/control groups; ~100 names incl. `/compact`, `/shake`, `/fork`, `/seance`, `/collab`, `/vibe`, `/goal`, `/hub`, `/wt`), markdown commands from `commands/` dirs and TS custom commands (`src/extensibility/custom-commands/`), provider-priority dedup (`docs/slash-command-internals.md`).
- **Themes:** two-slot `theme.dark` / `theme.light` with auto-switching by terminal luminance; flat `theme:` strings migrate into slots (`packages/coding-agent/src/config/settings.ts` ~line 2691; `packages/tui/src/theme/{dark,light}.json`, `theme-schema.json`).
- **Beyond one terminal:** multi-root workspaces, tmux/multiplexer integration (`tui/src/tmux.ts`), desktop notifications, OSC 8 hyperlinks, Tern Surface Protocol for native terminal UI (`packages/wire/src/tsp.ts` — Stencil's own Tern terminal), live collab sharing with native guest rendering + browser join (`docs/collab.md`, `src/collab/`), IRC inter-agent messaging (`src/irc/`).
- **Modes:** interactive (default), print/headless, RPC, RPC-UI, ACP, plus in-session behavioral modes — plan mode with review overlays and yolo handoff (`src/prompts/plan-mode-*.md`), goal mode (`src/goals/`), vibe mode, prewalk hand-off (`src/session/prewalk.ts`), auto-research experiment loops on a git branch (`src/autoresearch/`), and BTW side-questions (`src/session/btw-turn.ts`).
- **Voice:** dictation (STT) and speech (TTS) wired to `dictation`/`speech` roles; native voice crate `crates/pi-voice` (`src/stt/`, `src/tts/`).
- **Status line & HUD:** configurable statusLine settings, progress HUD, footer showing cwd/model/context %/cost for hosts (`src/modes/status-line-host.ts`, `progress-hud.ts`).

## 9. Extensibility

- **Extensions (current runtime):** TS modules default-exporting a factory `(pi: ExtensionAPI) => void`; 45 typed events (`session_*`, `before/after_provider_request`, `tool_call`, `before_subagent_spawn`, `turn_start/complete`, ...) plus `registerTool`, `registerCommand`, `registerProvider`, UI dialogs, link schemes (`packages/coding-agent/src/extensibility/extensions/types.ts` lines 1795-2330; `docs/extensions.md`, `docs/extension-loading.md`).
- **Hooks (legacy but supported):** `.omp/hooks/pre/*.ts`-style TS modules are loaded as extensions; `--hook` aliases `--extension`; pre/post tool interception contracts in `extensibility/hooks/{types,runner,tool-wrapper}.ts` (`docs/hooks.md`).
- **Skills:** `<root>/skills/<name>/SKILL.md` with frontmatter (`name/description/globs/alwaysApply/hide/disableModelInvocation`), exposed as prompt metadata + `skill://` reads + `/skill:<name>` commands; multi-provider discovery incl. Claude/Codex/Agent Skills/plugins and marketplaces (`docs/skills.md`, `src/extensibility/skills.ts`, `docs/marketplace.md`).
- **Custom commands:** markdown prompt-expansion commands and TS executable commands with full host API (`src/extensibility/custom-commands/types.ts`).
- **Custom tools:** factory modules returning TypeBox/ArkType/Zod-schema'd tools; SDK-provided and file-discovered paths; bundled examples are `tts` and `generate_image` (`docs/custom-tools.md`, `src/extensibility/custom-tools/`).
- **MCP:** full manager with stdio/HTTP transports, OAuth (incl. Smithery registry), tool caching, notifications, `mcp__<server>_<tool>` naming (`src/mcp/manager.ts`, `docs/mcp-*.md`).
- **Agents-as-config:** markdown+frontmatter agent definitions (bundled: scout, reviewer, security-reviewer, seance, init, model-mention, task — `src/prompts/agents/`), user/project/plugin discovery with model policy, tool subsets, isolation per agent (`docs/task-agent-discovery.md`, `src/task/discovery.ts`).
- **Plugins & marketplaces:** omp plugin packages bundle skills/commands/extensions with manifest discovery, marketplace install/uninstall, and per-plugin settings UI (`src/discovery/omp-plugins.ts`, `docs/marketplace.md`, `tui/src/overlays/plugin-selector.ts`, `docs/skills/authoring-marketplaces.md`). A "Skillshare" registry installs root-`SKILL.md` packages (`docs/skills.md`).
- **Failure isolation:** extension load errors are collected and surfaced without killing the session; `disabledExtensions` can omit individual items (`<name>:<id>`), and `--no-extensions`/`--trusted-extension <path>` scope ambient discovery (`docs/extension-loading.md` §Failure handling; README.md trust-model section).
- **Rulebook/TTSR:** tool-triggered sticky rules (`tool:edit(*.ts)`-style matchers checked against streamed edit payloads with per-file digests) re-inject warnings when violated (`src/session/ttsr-coordinator.ts`, `docs/ttsr-injection-lifecycle.md`, `docs/rulebook-matching-pipeline.md`).
- **Claude/Codex/Gemini compatibility:** discovery providers read foreign skills, commands, rules, and settings read-only; `enabledProviders` gates foreign user bases (`docs/config-usage.md`, `src/discovery/claude.ts`, `codex.ts`, `opencode.ts`).
- **Surfaces:** in-process SDK (`docs/sdk.md`, `createAgentSession` in `src/sdk.ts`), JSONL RPC protocol with extension-UI sub-protocol and host-tool round-trips (`src/modes/rpc/rpc-mode.ts`, `docs/rpc.md`), ACP (`src/modes/acp/`), collab guests, and a headless print mode; extension/Hook/CustomCommand/CustomTool hosts all receive an `api.pi` handle resolving the host SDK lazily (`docs/sdk.md` §entry points).
- **Provider registration at runtime:** `pi.registerProvider(name, config)` for extension-defined transports (`extensions/types.ts` ~2122-2146).

## 10. Configuration

- **Files:** global `~/.omp/agent/config.yml` (profiles: `~/.omp/profiles/<name>/agent/`), project `<cwd>/.omp/config.yml`, `--config` overlays, runtime overrides (`docs/settings.md` §Where settings live). Canonical roots also scan `.claude`, `.codex`, `.gemini` (`docs/config-usage.md`). ~250 documented env vars (`docs/environment-variables.md`).
- **Every key is a registered setting** with type/default/UI metadata colocated with its feature (`register({...})` pattern; e.g. `src/tools/settings.ts`, collected by `src/config/all-settings.ts`; `omp config list/get/set/reset` CLI).
- **Task isolation keys** (`src/task/settings.ts`): `task.isolation.enabled` (default false; protocolDefault for rpc/acp), `isolation.backend` enum `auto|apfs|btrfs|zfs|reflink|overlayfs|projfs|block-clone|rcopy` (auto picks best PAL backend; APFS clonefile, btrfs/ZFS snapshots, Linux FICLONE, overlayfs with fuse fallback, Windows ProjFS, block clone, raw recursive copy), plus `task.isolation.apply`, `task.isolation.merge`, `task.isolation.commits`, `task.worktreeDir` (default `~/.omp/wt`), and `task.batch` shape switch.
- **Other notable keys:** `tools.approvalMode`/`tools.approval.*`/`tools.restrictedFolders`, `compaction.methodOrder`, `steeringMode` (renamed from `queueMode`), `edit.mode`, `memory.backend`, `advisor.*`, `modelRoles`/`modelProviderOrder`, `theme.dark/light`, `secrets.enabled`, `snapcompact.*`, `providers.*`.
- **Illustrative minimal `config.yml`** (keys verified against `docs/settings.md`, `src/task/settings.ts`, `docs/memory.md`):

```yaml
theme:
   dark: omp-dark
   light: omp-light
tools:
   approvalMode: always-ask-cached
   restrictedFolders: [~/secrets]
modelRoles:
   default: anthropic/claude-sonnet-4
   smol: openai/gpt-5-mini
advisor:
   enabled: true
memory:
   backend: mnemopi
task:
   isolation:
      enabled: true
      backend: auto
compaction:
   methodOrder: [native, snapcompact, summarize, handoff]
```

(The `compaction.methodOrder` value names are illustrative of the mechanism, not a verified enum; the order itself is read from `packages/coding-agent/src/session/context-settings.ts` — unclear.)

- **Migration (docs/settings.md §Legacy migration):** startup merges legacy `settings.json` + `agent.db` settings into `config.yml` then renames the JSON to `settings.json.bak`; field-level rewrites include `queueMode→steeringMode`, flat `theme→theme.dark/light` (luminance slotting), `task.isolation.mode: none|<backend>→task.isolation.enabled` + backend, legacy backends `worktree|fuse-overlay|fuse-projfs→rcopy|overlayfs|projfs`, `edit.mode: atom|vim→hashline`, `compaction.strategy|remoteEnabled→compaction.methodOrder`, `serviceTier*→tier.*`, `readHashLines` removed. JSON→YAML migration for other config files (e.g. `models.json→models.yml`) is automatic (`docs/models.md`).
- **Layering semantics:** arrays replace (never append) across project/global layers; broken YAML is moved to a `.broken-*` backup at startup; unknown keys are never silently promoted to settings (`docs/settings.md` §Troubleshooting).

## 11. Standout Features

1. **Hashline editing** — line-anchored patches keyed by a 4-hex content snapshot tag per file, with native parse/repair, block ops, cut/paste registers, and strict unseen-line rejection (`packages/coding-agent/src/edit/hashline-compact.md`; `crates/pi-edit/src/modes/hashline/`). The model literally cannot edit against stale reads.
2. **CoW subagent isolation** — per-task filesystem clones via APFS clonefile / btrfs / ZFS / reflink / overlayfs / ProjFS / block-clone / rcopy with capture-and-merge semantics (`crates/pi-iso/`, `src/task/worktree.ts`, `src/task/isolation-runner.ts`).
3. **Advisor/"WATCHDOG" reviewers** — auxiliary reviewer models watching the transcript, tool-equipped but isolated, injecting advice cards; rostered via `WATCHDOG.yml` (`src/advisor/runtime.ts`, `watchdog.ts`; `docs/advisor-watchdog.md`).
4. **Live steering** — user messages can land inside the provider's in-flight response via a dedicated channel with latency-based conversion (`packages/agent/src/live-steering.ts`; `agent-loop.ts` ~1900).
5. **Provider-native compaction menu** — method *order* spanning Anthropic/Codex/OpenAI/Bedrock native compaction, snapcompact bitmap archives, summarization, handoff, and mechanical `shake` elision (`packages/agent/src/compaction/`, `packages/snapcompact/`).
6. **LSP as a loop citizen** — 14-action tool plus writethrough: edits are fed to language servers and *deferred diagnostics* are injected back into later turns with staleness tracking by file-mutation counters (`src/lsp/deferred-diagnostics.ts`, `src/edit/index.ts`, `ToolSession.queueDeferredDiagnostics` in `src/tools/index.ts`).
7. **Persistent eval kernels** — one-call-one-cell Python (runner.py kernel) and JS (worker) runtimes with retained state, streaming, tool bridges, budget/judgment/workpool bridges (`src/eval/`, `docs/eval.md`, `docs/python-repl.md`).
8. **Kernel-native orchestration contracts** — magic keywords (`orchestrate`, `workflowz`, `jevify`) inject contracts that drive `agent()/completion()/judge()/workpool()` kernel helpers for fan-out research/audit workloads (`src/modes/magic-keywords.ts`, `src/eval/*-bridge.ts`).
9. **Vibe mode (director/worker split)** — the main session becomes a read-only director steering persistent worker sessions, verifying their claims itself (`docs/vibe-mode.md`, `src/vibe/`).
10. **xdev tool virtualization** — discoverable tools are unmounted from the request schema and remounted as `xd://` devices behind `read`/`write`, keeping top-level tool lists small (`src/tools/xdev.ts`, `src/tools/essential-tools.ts`).
11. **Seance (session séances)** — consult/revive past sessions as read-only advisors with fork-from-history and isolation policy (`src/task/seance.ts`, `src/slash-commands/seance.ts`).
12. **Per-provider compat as data** — 243 KDL rule files encoding quirks (auth, endpoints, streaming) instead of ad-hoc branches (`packages/catalog/src/compat/rules/`, `docs/provider-quirks.md`).
13. **Native-first performance** — N-API addon with embedded binaries: edit engine, walker/grep, AST ops, bash runtime with output minimizer trained from real corpora (`scripts/session-stats/`, `crates/pi-shell/src/minimizer/`).
14. **Local composer prediction** — n-gram + web-prior + GGUF SmolLM predicting the user's next input client-side (`crates/pi-predict/`, `data/web-prior.bin.zst`).
15. **Credential ops as a subsystem** — pools, rotation, health, affinity, resets, per-family service tiers (`packages/ai/src/auth/`, `src/account-pools.ts` in config).
16. **Collab & surfaces everywhere** — same session natively rendered for guests over a relay, browser join links, TSP terminal protocol, IRC agent-to-agent messaging (`src/collab/`, `packages/wire/src/tsp.ts`, `src/irc/`).
17. **Deep IDE ops** — 28-action DAP driver with stop-state caching and awaited outcomes (`src/dap/`, `docs/tools/debug.md`), plus IDA tool for reverse engineering.
18. **Self-measuring harness** — metaharness benchmarks (edit benchmarks, compaction research with dozens of experiments in `packages/snapcompact/research/`, session-stat analysis scripts in `scripts/session-stats/`) — the product is continuously tuned against its own evals (`packages/metaharness/README.md`).

## 12. Lessons for Marfim

**Adopt:**
- **Hash/snapshot-anchored edits**: binding every edit to a content digest of what the model last saw eliminates stale-edit failures structurally instead of via prompt pleading (`crates/pi-edit`); cheap to reimplement, huge reliability win.
- **Compaction as an ordered strategy list**, mixing provider-native compaction, mechanical elision (shake), and summarization with a protected tail — never one monolithic summarizer (`packages/agent/src/compaction/`).
- **Two-queue steering with mode knobs** (interrupt vs follow-up; all vs one-at-a-time) and a *soft* abort signal tools can honor — clean semantics for "user typed while agent works" (`agent-session.ts`, `agent-loop.ts`).
- **Deferred LSP diagnostics with staleness counters** — post-edit diagnostics injected at the next yield, dropped if the file changed again (`src/lsp/deferred-diagnostics.ts`). This is the cheapest high-value "IDE wired in" feature.
- **Approval tiers declared by tools** (`read|write|exec`) with fail-closed defaults for unknown tools, plus argument-level policy overrides (`docs/approval-mode.md`) — small surface, composable.
- **Settings-as-registered-schema** colocated with features, with automatic legacy migrations on load (`docs/settings.md`) — keeps config honest at 18 major versions.
- **Ephemeral subagent isolation via CoW** with auto backend selection; plain `git worktree` is the fallback, not the centerpiece (`crates/pi-iso`).
- **Per-provider quirks as data files** rather than scattered conditionals (`packages/catalog/src/compat/rules/*.kdl`).

**Avoid:**
- **Scale without product focus**: ~1.0M lines of TS and a 180k-line shell-builtin crate (incl. a full rsync) is far beyond what a clean-room reimplementation needs; Marfim should pick the loop/edit/session core and skip `pi-builtins`, `pi-voice`, IDA, IRC, collab, and the Tern protocol.
- **Deep native coupling early**: the N-API addon buys speed but triples build complexity (Bazel + bun + vendored crates); a pure-TS Marfim with selective native hot paths later is more maintainable.
- **Hidden magic**: magic keywords and dozens of hidden notice/retry prompts (`src/prompts/`) make behavior hard to predict and audit; keep contracts explicit.
- **Yolo-by-default** (`tools.approvalMode` default `yolo`, `docs/approval-mode.md`) — reasonable for omp's expert audience, risky as a general default.
- **Fork-lineage debt**: legacy Pi shims, dual hook/extension subsystems, and four foreign config roots (`.claude/.codex/.gemini`) are migration artifacts, not design (`docs/hooks.md` explicitly calls hooks legacy).

**Open questions / unclear:**
- Exact upstream divergence point and whether any code is still mechanically synced from pi-mono (shallow clone; unclear).
- `pi-wire` `INTENT_FIELD = "i"` compaction of collab frames is documented only tersely (`packages/wire/src/index.ts` line 404); full collab wire semantics live in protocol docs not examined here.
- Build parity: whether all platforms ship the embedded addon or fall back to pure TS paths (`docs/porting-to-natives.md` exists but was not fully reviewed).
