# Forensic Analysis: opencode (https://github.com/sst/opencode)

- **Analyzed commit:** `1bf08ae46a91f7793c756e4ee521e37b6b554064` (2026-11-02, "feat(app): tweak the left sidebar and align window controls with its icons (#61289)"), branch `dev` (default branch per root `AGENTS.md`). The clone is shallow and contains **no git tags**, so tag-based version checks were impossible; version evidence below comes from `package.json` and `CHANGELOG.md`.
- **License:** MIT (root `LICENSE`, "Copyright (c) 2025 opencode"). Several sub-packages carry their own MIT LICENSE files (e.g. `packages/http-recorder/LICENSE`, `packages/ui/LICENSE`).
- **Primary languages:** TypeScript overwhelmingly (~1.03M raw `.ts`/`.tsx` lines repo-wide excluding `node_modules`/`.git`; ~617k lines when counting only `.ts/.tsx/.js/.css` under `packages/ services/ sdks/ script/` and excluding `node_modules`, `dist`, `.d.ts`, and `*/generated/*`). Secondary: **Rust** — 36 files / ~13.9k lines in `packages/process-daemon` ("opencode-pty" PTY daemon). **Go: 0 files** — the historically documented Go TUI no longer exists anywhere in the tree.
- **Approx LOC by package** (`.ts/.tsx` only, excluding node_modules/dist/`.d.ts`/generated; includes tests):
  `packages/app` ~219k, `packages/core` ~205k, `packages/tui` ~115k, `packages/ui` ~48k, `packages/cli` ~26k, `packages/desktop` ~17k, `packages/client` ~15k, `packages/server` ~12k, `packages/protocol` ~5.3k, `packages/sdk` ~2.8k.

## 1. Identity & Purpose

- OpenCode describes itself as "The open source AI coding agent" (root `README.md`) — a terminal-first AI coding agent with a client/server core, plus web, desktop, and IDE surfaces. Install channels: `curl -fsSL https://opencode.ai/v2/install | bash`, npm `@opencode/cli`, Homebrew (`anomalyco/tap/opencode-v2` and the slower `opencode` formula), Arch/AUR, mise, Nix (`README.md`).
- Maintainer: the OpenCode team. The git remote is `github.com/sst/opencode` (the project originated in the SST org), but every internal `repository` field, CI badge, and release URL now points at **github.com/anomalyco/opencode** (`package.json`, `README.md`, `CHANGELOG.md` PR links) — the project operates under the anomalyco org at this commit.
- **Current major version line: v2.** Root `package.json` says `2.2.5`. `CHANGELOG.md` opens with "Release notes for OpenCode V2" and its newest entry is `v2.2.5 — 2026-10-30`; the oldest visible entry is `v2.0.26 — 2026-10-08` (1,330-line file covering only the v2 line). Docs are published under `https://opencode.ai/v2/docs` (`README.md`). So yes, **a v2 major version exists and is the current line**.
- v2 is a near-rewrite relative to v1: a dedicated migration guide (`services/www/src/docs/content/migrate-v1.mdx`, 633 lines) documents three intentional breaking changes (new plugin API, new server API/clients, `cli.json` replacing layered `tui.json`), a v1 compatibility layer (`packages/core/src/v1/`), and v1 database migration (`packages/core/src/database/v1-migration.ts`).
- First-party engineering specs live in-tree: `specs/v2/README.md` (authority map), `session.md`, `tools.md`, `event-stream-architecture.md`, `provider-policy.md`, `catalog-config-plugin-lifecycle.md`, `claude-agent-provider.md`. Contributor rules live in root `AGENTS.md`.
- **Verification of the supplied known-context claims against this tree:**
  - "TypeScript monorepo" — **true** (Bun workspaces + turbo, root `package.json`).
  - "Core agent under `packages/opencode/src`" — **false here**: no `packages/opencode` exists; the core is `packages/core` (`@opencode/core`).
  - "Go-based TUI under `packages/tui`" — **false**: 0 Go files; TUI is TypeScript/SolidJS on OpenTUI (§8).
  - "models.dev catalog" — **true** (`packages/core/src/models-dev.ts`).
  - "Client/server, sessions, subagents, MCP, plugins, permissions, auth login" — **all true**, with v2-specific shapes (§2–§6, §9).
  - "Subagents (build/plan/general)" — **partially true**: `build` and `plan` are *primary* agents; `general` and `explore` are the *subagents*, plus hidden `title`, `summary`, `compaction` agents (`packages/core/src/plugin/agent.ts`, `plugin/plan.ts`).
  - LSP integration — **removed in v2**: "V2 accepts and preserves `lsp` configuration, but it does not run language servers, expose LSP tools, or produce LSP diagnostics" (`services/www/src/docs/content/migrate-v1.mdx`).
  - Share feature — **effectively gone in v2**: the config key `share: manual|auto|disabled` survives (`packages/schema/src/config/config.ts`), legacy `autoshare` is normalized (`packages/core/src/config/normalize.ts`), but no share implementation exists in core/server/protocol and no docs page remains. Exact intent unclear.

## 2. Architecture

- **Monorepo**: Bun workspaces + Turborepo (`workspaces.packages = ["packages/*", "packages/plugins/*", "services/*", "packages/console/*", "packages/stats/*"]`, `packageManager: bun@1.4.4`; `typecheck` runs `bun turbo`). The codebase is written in **Effect-TS** style throughout (`effect` 4.1.0 in the dependency catalog; `Effect.gen`, `Layer`, `Scope` everywhere in `packages/core/src`).
- **Package inventory** (all `@opencode/*` scoped; names verified in each `package.json`):

| Package | Role |
|---|---|
| `core` | Agent runtime: sessions, tool registry, permissions, providers, MCP, plugins, config, database, skills, commands, worktrees, snapshots, schedules |
| `ai` | In-house LLM client/SDK: provider modules + wire protocols (§5) |
| `schema` | Public domain shapes and durable-event schemas (`packages/schema/src/`) |
| `protocol` | Typed HTTP API definitions; committed `openapi.json` (`packages/protocol/openapi.json`) |
| `server` | HTTP server assembling domain handlers into the API (`packages/server/src/api.ts`, `handlers/`) |
| `client` | Generated clients in `promise/`, `effect/`, `solid/` flavors (`packages/client/src/`) |
| `plugin` | Public plugin API: `effect/` capability domains, `gui/` UI extension points, TUI plugin host |
| `codemode` | The CodeMode interpreter and tool bridge (§4/§11) |
| `tui` | Terminal client (SolidJS + OpenTUI), incl. the alternative "mini" UI (`packages/tui/src/mini/`) |
| `app` | Web client (Vite + Solid), also embedded by the Electron desktop shell |
| `desktop` | Electron packaging, native menus, updater glue (`packages/desktop/package.json`) |
| `ui`, `theme` | Shared Solid component library and theme engine (v2 token system) |
| `session-ui` | Session-UI pieces shared between clients |
| `console/app` | Cloud console web app (billing, providers, workspaces) |
| `web` | Astro marketing/docs site |
| `sdk` | Embeddable SDK composing Client+Core+Server (`packages/sdk/src/index.ts`) |
| `simulation`, `http-recorder` | Record/replay test infrastructure for LLM/HTTP traffic |
| `process-daemon` | Rust PTY daemon ("opencode-pty") |
| `stories` | TUI/web storybook harness |
| misc | `identity`, `function`, `latex`, `merman`, `patchmark`, `gui-plugins`, `httpapi-codegen`, `effect-drizzle-sqlite`, `script`, `util`, `stats` |

- Cloudflare Worker services: `services/update` (release-channel artifacts in D1, `services/update/src/index.ts`), `services/files`, `services/changelog`, `services/www` (docs). Also in-tree: `sdks/vscode` (VS Code extension), `infra/` (SST infra), `nix/`, `perf/`, `specs/v2/`, `salesforce/` (CRM metadata — unrelated to the product runtime).
- **Testing infrastructure is first-class**: `packages/http-recorder` records/replays HTTP and WebSocket traffic as versioned cassettes with secret redaction (`packages/http-recorder/README.md`, `src/redaction/`), `packages/simulation` builds a scripted openai-compatible backend for end-to-end agent tests (`packages/simulation/src/backend/simulated-provider.ts`), and core supports `#v1-migration` conditional imports (`packages/core/package.json`).
- **Layering rule** (enforced, root `AGENTS.md` + `specs/v2/README.md`): HTTP operations = Protocol; public shapes/durable events = Schema; runtime/persistence = Core; Server composes Core; Clients depend only on Schema + Protocol (never Core/Server); `sdk` composes Client+Core+Server; generated clients are regenerated via `bun run generate` after Protocol changes and must not be hand-edited.
- **Entry points**: `packages/cli/src/index.ts` (the `opencode` binary; command spec in `packages/cli/src/commands/commands.ts`), `packages/server/src/api.ts` (HTTP server), `packages/tui/src/index.tsx`, `packages/cli/src/acp/` (Agent Client Protocol server), `sdks/vscode/src/extension.ts`, Electron main `packages/desktop`.
- **Local server security**: HTTP basic-style password; HMAC-signed 30-day session tokens accepted anywhere the password is; pairing codes (5-minute TTL) issue tokens for browser pairing (`packages/server/src/auth.ts`, `packages/server/src/pairing.ts`).
- **Data flow (text diagram):**

```
 TUI (Solid/OpenTUI)     Web app (Vite/Solid)    Desktop (Electron)     VS Code ext     ACP editor
        \______________________ @opencode/client (promise | effect | solid) ______________/
                                       |  HTTP + SSE + WebSocket
                                       v
                  packages/server  (typed HttpApi; handlers/<domain>.ts)
                                       v
        packages/core "Location" services (Effect layer graph, one per working directory)
   SessionRunner.drain -> SessionModelRequest -> @opencode/ai LLMClient -> provider protocol module
                                       |
                    Tool registry (built-ins | user plugins | MCP | CodeMode execute)
                                       |
              SQLite via drizzle (durable facts + projections) -> Bus -> SSE EventFeed
```

- A "Location" is one working directory scope: plugins, MCP servers, tools, and sessions are instantiated per Location (`packages/core/src/location.ts`, `instance.ts`; "Location-scoped" appears throughout `packages/core/src/tool/AGENTS.md`). `workerd` conditional imports show Core can also run on Cloudflare Workers (`packages/core/package.json` `imports` map: `#sqlite`, `#pty`, `#fff`, `#photon-wasm`).

## 3. Agent Loop

- **Turn admission precedes execution** (`specs/v2/session.md`; `packages/core/src/session/inbox.ts`): every prompt publishes a durable `session.inbox.enqueued` fact projected into a `session_inbox` row; a `session.inbox.delivered` projection atomically consumes it into a visible message. `resume` controls scheduling, not durability. Delivery modes: `steer` (default; delivered in enqueue order at the next Safe Step Boundary) and `queue` (waits for an idle boundary; steers still jump ahead). Manual compaction and session "move" are inbox control items that form delivery boundaries.
- **Turn loop** — `SessionRunnerLLM.drain` in `packages/core/src/session/runner/llm.ts`: wake (by input, continuation, or schedule) → `settleStaleCompactions` + `settleStaleToolCalls` (crash recovery: publish failures for orphaned `running` compactions and streaming/running tools) → `advanceToStep` loop (promote next inbox item; auto-title via the hidden `title` agent; manual compaction when requested) → `runStep`, repeated while continuation is needed.
- **Context preparation per step** (`runStep` + `packages/core/src/session/context.ts`): await plugin activation → `context.select` (session, agent, instruction sources, tool snapshot) → model resolve (`session/runner/model.ts`) → `SessionCompaction.estimatePrompt` → **auto-compaction gate** → `context.load`.
- **System prompt assembly**: base template `packages/core/src/session/runner/prompt/system.txt` rendered by `packages/core/src/session/system-prompt.ts`, which substitutes `${OPENCODE_TOOL_GUIDANCE}` with tool-specific guidance derived from the tool list (shell/edit/write advice). Model-family prompt plugins then override or append: GPT vs "Astra" (`plugin/system-prompt/gpt.txt`, `gpt-astra.txt`), Anthropic (append mode, `anthropic.txt`), Kimi (`kimi.txt`), Arcee Trinity (`trinity.txt`), Meta Muse (`meta.txt` with `{{MODEL_NAME}}`) — selection in `packages/core/src/plugin/optimize.ts`. Instruction sources (date, `<env>` block with working dir/platform/tmpdir preference, discovered `AGENTS.md` files, CodeMode catalog, MCP instructions, skills) are injected as separate rendered blocks (`packages/core/src/instructions/builtins.ts`, `config/plugin/instruction.ts`, `codemode/instructions.ts`, `mcp/instructions.ts`).
- **Request building**: `SessionModelRequest.primary` (`packages/core/src/session/model-request.ts`) assembles the provider request. When the agent's `steps` limit is reached, an assistant `MAX_STEPS_PROMPT` message is appended and `toolChoice` is forced to `"none"` so the model must answer in text (`session/runner/llm.ts`, `session/runner/max-steps.ts`).
- **Streaming + tool dispatch** (`packages/core/src/session/runner/step.ts`): the provider stream is consumed event-by-event through a durable-event publisher (`session/runner/publish-llm-event.ts`); each non-provider-executed `tool-call` is executed in its **own forked fiber**, so multiple tool calls run in parallel; local execution starts only after the `Tool.Called` publication completes; a start snapshot capture is forked concurrently with the provider request; tool outputs are truncated by the generic bounder (`packages/core/src/tool-output.ts`: 2,000 lines / 50 KiB / 7-day retention, full output kept in a backing file).
- **Step outcomes**: `Completed | Retry | Continue | RecoverFull | Compacted | OutputLimit` (`step.ts` `Outcome`). Notable recovery paths: provider context-overflow rejection before any output triggers compaction then a retry (`Outcome.Compacted`); an interrupted/partial stream yields a synthetic continuation message ("The previous response was interrupted. Continue from where you left off…"); `finish === "length"` gets up to two output-limit continuations before failing (`llm.ts` inspects the last three user/assistant rows).
- **Retries**: pluggable policy chain — `packages/core/src/model-retry.ts` decides attempt/delay, the plugin `session.retry` hook can override, `SessionEvent.RetryScheduled` is published and the runner sleeps the remaining delay (`session/runner/retry.ts`).
- **Compaction** (`packages/core/src/session/compaction.ts`, ~1.2k lines): triggers `auto` (threshold), `overflow` (provider rejected the context), `manual` (`/compact`); settings `buffer` (tokens or percent string like `"20%"`; default keeps 15% and ≥16k tokens free — CHANGELOG v2.2.5), `keep` (recent tail kept verbatim beside the summary), `precompute` (background summary started near the threshold and swapped in when compaction fires). Summary prompts around lines 195–217 produce a structured summary referencing the kept tail via `metadata.keepID`.
- **Snapshots per step**: filesystem state is captured content-addressed before/after work; `session.step.ended` events carry usage, snapshot id, and changed files (`packages/core/src/snapshot.ts`, `step.ts`).
- **Auxiliary generation**: untitled sessions get a background title job (hidden `title` agent) that must not delay model execution (`llm.ts` `titles` FiberMap; `session/title.ts`); the hidden `summary` agent writes PR-style session summaries (`plugin/agent.ts` `PROMPT_SUMMARY`); `compaction` is itself a hidden primary agent so the summary request looks like a normal step (`plugin/agent.ts`).
- **Durable session event vocabulary** (sampled from `packages/schema/src/session-event.ts`): `session.inbox.enqueued/delivered/cancelled`, `session.execution.started/succeeded/failed/interrupted`, `session.step.started/streamed/ended/failed`, `session.text.delta`, `session.reasoning.started/delta/ended`, `session.compaction.started/delta/ended/failed`, `session.retry.scheduled`, `session.agent.selected`, `session.model.selected`, `session.revert.staged/committed/cleared`, `session.shell.started/ended`, `session.skill.activated`, `session.forked`, `session.moved`, `session.synthetic`, `session.permissions`, `session.archived`, `session.deleted`.

## 4. Tool Set

Registry semantics in `packages/core/src/tool.ts` (Location-scoped service: registration, effective lookup, execution) and `packages/core/src/tool/AGENTS.md` + `specs/v2/tools.md`. One structural tool value (`Tool.make`) carries schemas + execute; every invocation receives durable context `{sessionID, agent, messageID, callID, progress}`. Tools default into **CodeMode** (`codemode: true`) unless they opt out with `codemode: false` — all core built-ins opt out; MCP and the `opencode`/`artifact`/`schedule` namespaces opt in. Permission actions attach per tool; `edit`/`write`/`patch` share the `edit` action (`tool/AGENTS.md`; per-tool resources documented in `services/www/src/docs/content/tools.mdx`).

| Tool | Purpose | Key params | Defining file |
|---|---|---|---|
| `read` | Read a file or directory listing; images/PDFs passed to the model; 2,000-line/50 KiB page cap | `path`, `offset` (1-based), `limit` | `packages/core/src/tool/plugin/read.ts` |
| `write` | Create/overwrite a file, creating parent dirs | `path`, `content` | `tool/plugin/write.ts` |
| `edit` | Exact-string replace; unique-match enforced unless `replaceAll`; CRLF-aware; typographic-variant fuzzy retry | `path`, `oldString`, `newString`, `replaceAll` | `tool/plugin/edit.ts` |
| `patch` | Structured multi-hunk patch | hunks of type `add`/`update`/`delete` | `tool/plugin/patch.ts` |
| `glob` | File-path glob search | `pattern`, `path?`, `hidden?`, `limit?` (default 100, 30 s timeout) | `tool/plugin/glob.ts` |
| `grep` | ripgrep-syntax content search | `pattern`, `path?`, `include?`, `literal?`, `caseSensitive?`, `limit?` | `tool/plugin/grep.ts` |
| `shell` | Shell execution; foreground timeout or background; combined output stored in a backing file with a bounded tail returned | `command`, `workdir?`, `timeout?`, `background?` | `tool/plugin/shell.ts`; permission resource is the scanner-parsed command (`tools.mdx`) |
| `todowrite` | Maintain the agent's task list | items with `status` pending/in_progress/completed/cancelled | `tool/plugin/todo.ts` |
| `question` | Ask the user structured questions (multiple choice, multi-select) | `questions[]` | `tool/plugin/question.ts` |
| `webfetch` | Fetch a URL to text/markdown/html | `url`, `format?` (default markdown), `timeout?` | `tool/plugin/webfetch.ts` |
| `websearch` | Web search through the configured provider (Console-managed or external) | query/provider options | `tool/plugin/websearch.ts`, `packages/core/src/websearch.ts` |
| `subagent` | Spawn or continue a child-session agent | `agent`, `description`, `prompt`, `model?`, `sessionID?`, `background?` | `tool/plugin/subagent.ts` |
| `skill` | Load a named skill's instructions on demand | `name` | `tool/plugin/skill.ts` |
| `experimental_image_generate` | Generate/edit images with image models as a background job | `prompt`, `images?` (≤8), `model?` | `tool/plugin/image-generate.ts` |
| `session_rename`, `session_move` | Manage the session from inside the loop | name / target | `tool/plugin/opencode.ts` |
| `worktree_create/list/remove` | Git worktree management | `name`, base branch | `tool/plugin/opencode.ts` |
| `models` | Look up available models for routing/choice | filters | `tool/plugin/opencode.ts` |
| `list_mcp_resources`, `read_mcp_resource` | Enumerate/read MCP server resources | `server`, `uri` | `tool/plugin/mcp-resource.ts` |
| `artifact_create/read/update/delete` | Publish and serve HTML/static artifacts (behind `experimental.artifacts`) | name, content | `tool/plugin/artifact.ts` |
| `schedule_create/list/pause/resume/delete` | Manage recurring scheduled agent runs | cadence + prompt | `tool/plugin/schedule.ts` |
| `<server>_<tool>` | MCP server tools, namespaced per server | JSON Schema defined by the server | `packages/core/src/tool/mcp.ts` (`namespace()`, `name()`) |
| `execute` | CodeMode entry point: run a JS-like program whose only I/O is tools | program source | `packages/codemode/src/tool.ts`; docs `packages/codemode/README.md` |

Notes: a model calling a tool named `bash` is transparently routed to `shell` (CHANGELOG v2.2.5); `grep`/`glob` are deleted from GPT and Claude native tool lists by `packages/core/src/plugin/optimize.ts` (`OpenAIToolsPlugin`/`AnthropicToolsPlugin`), whose comment says the heavy grep/glob UX via native tools is unsettled; subagent nesting is capped by `experimental.subagent_depth` (default 1) with parent-chain depth checks (`tool/plugin/subagent.ts`).

- **Registration semantics** (`tool/AGENTS.md`, `specs/v2/tools.md`): built-ins, plugins, and MCP all install through `Tool.Service.transform`; the latest valid registration for an effective name wins, disposal reveals the previous definition, and each model request captures the tool definitions/executors it advertises (later reloads only affect later requests). MCP owns one stable transform whose refresh preserves the precedence of later plugin overrides. One tool response carries up to three values: schema-validated `output` (ephemeral machine value consumed by Code Mode), `content` (model-facing, durably stored), and optional `metadata` (compact JSON for UI).

## 5. Model/Provider Layer

- **Catalog**: models.dev is fetched at runtime, decoded per-model defensively (unknown models/API entries are skipped rather than failing the catalog), priced (incl. cache read/write and TTL-keyed multipliers), and cached in KV; a bundled snapshot (`packages/core/src/models-dev/snapshot.txt`) is the offline fallback; v1 catalogs are upgraded via `packages/core/src/models-dev/v1.ts` (`packages/core/src/models-dev.ts`). Only model types `chat`, `decision`, `image` are kept (`TYPES` in that file). A script refreshes the snapshot (`packages/core/script/update-models-snapshot.ts`).
- **In-house provider SDK** `packages/ai` (`@opencode/ai`): 101 `./providers/*` export entries in `packages/ai/package.json` and ~45 hand-written wire protocols in `packages/ai/src/protocols/` — openai-chat/-responses/-images/-speech/-transcription, openai-compatible chat/responses, anthropic-messages, gemini + google images/interactions/speech/transcription/video, bedrock-converse + event-stream + mantle, alibaba chat/messages/responses/images, meta messages/responses/images, mistral-chat, cohere-chat, xai responses/images/video, venice-chat, zai-chat, plus speech/transcription for elevenlabs/deepgram/assemblyai/cartesia and runway video. Core resolves catalog entries to these packages through a lazy-import table (`packages/core/src/provider.ts`).
- **Provider plugins** (behavior + auth per provider): `packages/core/src/plugin/provider/` — amazon-bedrock, azure, chatgpt, claude-agent, cloudflare-ai-gateway, cloudflare-workers-ai, configured/dynamic/local, databricks, digitalocean, github-copilot, gitlab, google-vertex, kilo, llmgateway, lmstudio, modal, neon, nvidia, ollama, openai, opencode (Console), openrouter, poe, sap-ai-core, snowflake-cortex, vllm, xai, zenmux, plus cross-cutting `refresh.ts`, `prompt-cache-key.ts`, `openai-context-caps.ts`, `sdk-factory.ts`.
- **Auth methods per provider** (verified `Integration.MethodID.make` registrations):

| Provider plugin | Method(s) |
|---|---|
| `github-copilot.ts` | `device` |
| `xai.ts` | `browser` + `device` |
| `opencode.ts` (Console) | `device` |
| `digitalocean.ts`, `poe.ts`, `snowflake-cortex.ts` | `browser` (local OAuth callback page, `packages/core/src/oauth/page.ts`) |
| `gitlab.ts` | `pkce` |
| `azure.ts` | `azure-cli` |
| `openai.ts` | `chatgpt-browser`, `chatgpt-headless` |
| `chatgpt.ts` | `chatgpt-token-sharing` (consumes browser/headless credentials) |
| others | API keys via env/config or managed credentials |
- **Auth flows** (invoked by `opencode auth login`, handler `packages/cli/src/commands/handlers/auth/login.ts`; registry `packages/core/src/integration.ts` + `integration/connection.ts`):
  - OAuth **device flow**: GitHub Copilot, x.ai, and the OpenCode Console (`plugin/provider/github-copilot.ts`, `xai.ts`, `opencode.ts`).
  - OAuth **browser flow** with a locally served callback page (`packages/core/src/oauth/page.ts`): DigitalOcean, Poe, Snowflake Cortex.
  - OAuth **PKCE**: GitLab (`plugin/provider/gitlab.ts`).
  - **azure-cli** credential method (`plugin/provider/azure.ts`).
  - **ChatGPT account sharing**: `chatgpt-browser` and `chatgpt-headless` methods plus `chatgpt-token-sharing` (`plugin/provider/openai.ts`, `chatgpt.ts`).
  - **Claude Agent**: guided setup that copies `claude auth login`, installs Claude Code, and verifies the connection before model selection (CHANGELOG v2.2.4; `plugin/provider/claude-agent/`, spec `specs/v2/claude-agent-provider.md`).
  - **Well-known remote manifests**: origins can declare an auth command and remote config that injects providers (`packages/core/src/wellknown.ts`).
  - Credentials are stored, listable, switchable, and exportable/importable as JSON (`auth export|import|switch|list`, `packages/core/src/credential.ts`).
- **Console provider** (`plugin/provider/opencode.ts`): device-flow sign-in to the OpenCode Console; syncs org/workspace "Library" instructions and skills, org policies, and web-search entitlements; handles `SsoRequired` responses for SSO-enforced orgs.
- **Free models**: a model is free when every cost tier has input price 0 (`isFreeModel`, `packages/tui/src/component/model-options.tsx`); the server orders free models first for signed-out users (comment at the top of that file), and a dedicated picker for provider-less users exists under the "new-designs" experiment (`packages/tui/src/component/dialog-model-free.tsx`). Web search is likewise entitlement-gated through the Console or an external configured provider (`packages/core/src/websearch.ts`, docs `websearch.mdx`).
- **Custom base URLs and overlays**: `providers.<id>.settings.baseURL` keeps catalog metadata while redirecting traffic (docs `services/www/src/docs/content/providers.mdx`); per-provider/model `headers` and `body` overlays, plus compatibility knobs (`reasoning_field`, `max_tokens_field`, `media_input`) in `packages/schema/src/model-compatibility.ts`; region/resource-based providers (Azure, Bedrock VPC endpoints) build their own URLs when `baseURL` is unset (`BUILDS_OWN_URL` set in `models-dev.ts`).

## 6. Permissions & Safety

- **Rule shape**: ordered `{action, resource, effect: allow | deny | ask}` rulesets (`packages/schema/src/permission.ts`: `Rule`, `Ruleset`; replies `once | always | reject`; events `permission.asked`/`permission.replied`).
- **Evaluation** (`packages/core/src/permission.ts`): rules evaluate first-match with explicit deny handling; a request that no rule matches defaults to `ask` (`evaluate` returns `{effect:"ask"}` as the fallback); `assert` either allows, denies, or asks the connected client. A plain decline produces `Permission.DeclinedError` (tool call fails as "The user declined this tool call"); a decline with feedback (`Permission.CorrectedError`) is converted into a typed tool failure the model can act on (`packages/core/src/tool/AGENTS.md`, `session/runner/step.ts`).
- **Persistence**: "always" replies are saved per action/resource via `packages/core/src/permission/saved.ts` (+ `sql.ts`); `save: ["*"]` or per-resource patterns are passed by tools at call sites (e.g. `tool/plugin/read.ts`, `write.ts`).
- **Where enforcement happens**: inside each tool's execute against the captured `Permission.Service`; the registry itself performs no authorization — tool filtering is catalog visibility, not execution authorization (`packages/core/src/tool/AGENTS.md`). Permission requests take requester fields from the canonical invocation context via `Permission.invocation` (`permission.ts`).
- **Action/resource conventions** (`services/www/src/docs/content/tools.mdx`): `read` → normalized path; `glob` → pattern; `grep` → pattern (not the path); `edit` (covers edit/write/patch) → target path; `shell` → each scanner-produced command; `websearch` → query; `subagent` → agent id; `question` → `*`; `skill` → skill id; `experimental_image_generate` → selected model.
- **Agent defaults**: built-in `explore`, `title`, `summary` agents deny all actions with narrow allowlists (`packages/core/src/plugin/agent.ts`); `general` denies `question` and nested `subagent`; Plan mode denies `edit` everywhere except `~/.opencode/plan/*` and re-injects Plan-mode reminders when compaction strips them (`packages/core/src/plugin/plan.ts`).
- **Unattended contexts**: scheduled jobs evaluate a separate `Permission.Unattended` ruleset; anything not explicitly allowed is denied, configured denies still win (`packages/core/src/permission.ts`).
- **CLI overrides**: `--auto` (approve everything not explicitly denied) and hidden `--yolo` / `--dangerously-skip-permissions` (`packages/cli/src/commands/commands.ts`).
- **Enterprise**: managed settings (macOS managed plists, system config dirs) have the highest precedence and compose with `policies` that users cannot override (`services/www/src/docs/content/config.mdx`, `policies.mdx`; `packages/core/src/managed-policy.ts`, `packages/schema/src/config/policy.ts`).

## 7. Sessions, State & Context

- **Storage**: SQLite with WAL, `synchronous=NORMAL`, busy timeout, and passive checkpointing (`packages/core/src/database/database.ts`); schema and queries via drizzle-orm (`packages/core/src/database/drizzle/`, `sql.ts`); the DB lives under the XDG data root `~/.local/share/opencode` (`packages/util/src/global-roots.ts`; path helpers `packages/core/src/database/path.ts`). Data is modeled as durable facts + projections (inbox rows, `SessionMessageTable` with a monotonic `seq`; `specs/v2/session.md`); migrations in `packages/core/src/database/migration/` (e.g. `20260312043431_session_message_cursor.ts`, `20260323234822_events.ts`).
- **Session operations** are ID-bound façades delegating to `session/session.ts` (`prompt`, `synthetic`, compaction, shell, revert, rename, agent/model switch, fork); prompts are idempotent by inbox item ID (`specs/v2/session.md`).
- **Resume/continue**: CLI `--continue` resumes the last session, `--session <id>` resumes or creates, `--fork` forks before continuing (`packages/cli/src/commands/commands.ts`, `run`/`mini` params); TUI session list dialog and fork dialog (`packages/tui/src/component/dialog-session-list.tsx`, `routes/session/dialog-fork.tsx`).
- **Export/import**: sessions can be exported/imported as files (`packages/cli/src/commands/handlers/session/export.ts`, `import.ts`; server-side `session-import.test.ts`).
- **Steering**: mid-turn inputs deliver at Safe Step Boundaries; queued prompts deliver at idle boundaries; delivering new input resets the agent's step allowance (`specs/v2/session.md`).
- **Compaction/summarization**: §3; v1's `compaction.tail_turns`/`prune` were replaced by `compaction.keep.tokens` and checkpoint-based compaction (`services/www/src/docs/content/migrate-v1.mdx`).
- **Revert/snapshots**: content-addressed captures with diff/restore APIs (`packages/core/src/snapshot.ts`); user-facing revert is staged/committed/cleared via events (`session.revert.*`); a docs page exists (`services/www/src/docs/content/snapshots.mdx`).
- **Token accounting**: each step records provider usage + cost (`SessionUsage.record(record.finish.usage, model.cost)`, `session/runner/step.ts`); aggregates and provider rate-limit state in `packages/core/src/session/usage.ts` and `packages/core/src/usage-limits.ts`, surfaced as TUI usage toasts with `/models` shortcut (`packages/tui/src/component/usage.tsx`).
- **Workspaces/worktrees**: sessions attach to projects and workspaces (`packages/core/src/project.ts`, `workspace/`, migrations `20260225215848_workspace.ts`, `20260228203230_blue_harpoon.ts`); worktree strategies and directory logic in `packages/core/src/worktree/{strategies,directory,git}.ts` with `opencode_*` tools to manage them from inside the loop; a session can be "moved" between locations via inbox control items (`session.moved`, `specs/v2/session.md`).
- **Background work & jobs**: background subagents and background shell commands report through a shared job system (`packages/core/src/job.ts`, `session/background.ts`); the runner checks outstanding background work at idle boundaries and exposes it to the `experimental.idle` plugin hook (`session/runner/llm.ts`).
- **Share**: config key retained but no v2 implementation found (§1; unclear).

## 8. UI/UX

- **TUI implementation language**: TypeScript — SolidJS components compiled to the terminal via **OpenTUI** (`@opentui/core`, `@opentui/solid`, `@opentui/keymap` 0.8.1; `packages/tui/package.json`). The v1 Go TUI is gone (0 `.go` files repo-wide).
- **Structure**: route-based (`packages/tui/src/routes/home.tsx`, `routes/session/` with composer, sidebar, permission pane, timeline, grouping); a command palette; ~30 dialog components (`packages/tui/src/component/dialog-*.tsx`: model, model-free, agents, variants, themes, sessions, rename, MCP, skills, stash, status, update, pair, workspaces, worktree name, shell output, image preview, error details, highlights, experiments, integration, config, debug…); feature-plugin slots for prompt/sidebar/home/system (`packages/tui/src/feature-plugins/`); markdown streaming renderer; diff viewer with file tree and image diffs; inline terminal panes backed by persistent PTYs; attention sounds (`attention-sounds.*.ts`); i18n dictionaries; TUI plugin/keymap/markdown extension points (`test/plugin-*.test.tsx`); a storybook harness (`packages/stories`, `OPENCODE_STORY` workflow in root `AGENTS.md`).
- **Slash commands**: TUI built-ins are keymap commands carrying `slash` metadata — `/new`, `/sessions` (aliases resume/continue), `/open`, `/clear`, `/models`, `/agents`, `/variants`, `/mcps`, `/connect`, `/usage`, `/highlights`, `/settings`, `/status`, `/update`, `/pair` (alias `web`), `/restart`, `/plugins`, `/diff`, `/stats`, `/btw` (`packages/tui/src/app.tsx`, `feature-plugins/*`). Server-defined custom commands (markdown/JSON) are merged into autocomplete from `data.location.command.list` (`packages/tui/src/component/prompt/autocomplete.tsx`, `parseSlashHead` in `packages/tui/src/prompt/parse.ts`). The separate "mini" interface has `/new`, `/compact`, `/help` (`packages/tui/src/mini/footer.prompt.tsx`, `mini/demo.ts`).
- **Custom commands**: `.opencode/commands/<name>.md` with `$ARGUMENTS` substitution and optional frontmatter (`description`, `agent`, `model provider/model#variant`); nested paths become nested command names; JSON `commands` config also supported (`services/www/src/docs/content/commands.mdx`).
- **Themes**: ~35 bundled JSON themes in `packages/ui/src/theme/themes/` (catppuccin family, gruvbox, flexoki, nord, tokyonight, vesper, "oc-2", …) with a v2 token system (`packages/ui/src/theme/v2/`: primitives, mapping, foreground resolution) and TUI-side discovery/derivation (`packages/tui/src/theme/`, docs `themes.mdx`).
- **Keybinds**: leader-key scheme (default `ctrl+x`, configurable) with named commands — e.g. `app.exit`, `session.new`, `session.list`, `session.sidebar.toggle`, `terminal.toggle`, `prompt.editor` (`services/www/src/docs/content/cli/keybinds.mdx`; implementation `packages/tui/src/context/keymap.tsx`).
- **CLI surface** (`packages/cli/src/commands/commands.ts`): bare `opencode [directory]` starts the TUI (flags `--continue/-c`, `--session/-s`, `--prompt`, `--print-logs`, `--standalone`, `--server`, `--auto`/`--yolo`); subcommands: `run` (one-shot), `serve`, `api` (call any OpenAPI operation by operation-ID or `METHOD path`), `auth login|logout|list|switch|export|import`, `mcp list|add|auth|logout`, `plugin list|add|check|update|remove`, `models`, `stats` (shareable usage stats with models/tools/cost breakdowns), `session list|export|import|delete`, `upgrade`, `uninstall`, `service start|stop|status|get|set|unset|restart` (background daemon), `acp` (Agent Client Protocol server, docs `cli/acp.mdx`), `pair`, `debug agents|config|heap-dump|paths`.
- **Other clients**: web app `packages/app` (Vite + Solid, also embedded by the Electron `packages/desktop` shell; Playwright e2e suites), VS Code extension `sdks/vscode`, Zed via ACP/editor integration (`packages/tui/src/editor-zed.ts`).
- **Accessibility/i18n**: UI strings localized in `packages/ui/src/i18n/*.ts` (60+ locales including en, de, ja, zh, ar, he); the repo also ships translated READMEs (`README.de.md`, `README.ja.md`, etc.).
- **Usage stats UX**: `opencode stats` renders shareable usage summaries (days/year/lifetime, model and tool breakdowns, cost) (`packages/cli/src/commands/commands.ts`); TUI feature plugins add stats and diff-viewer surfaces (`packages/tui/src/feature-plugins/system/`).

## 9. Extensibility

- **MCP depth**: stdio servers (`mcp/stdio.ts`) and remote servers with a socket client (`connection.ts`, `socket-client.ts`); a full OAuth client incl. authorization-code + refresh-token rotation, concurrent-refresh dedup, and per-server credential rows (`mcp/oauth.ts`, 539 lines); resource enumeration/reading tools; server instructions injected into prompts (`mcp/instructions.ts`); >10 MiB stdio frames supported (CHANGELOG v2.2.5); MCP tools are registered through **one stable transform** whose refresh preserves later plugin override precedence (`packages/core/src/mcp/index.ts`, `tool/AGENTS.md`); management via `opencode mcp` and the TUI MCP dialog (`dialog-mcp.tsx`). MCP tools default into CodeMode (`packages/core/src/plugin/mcp-codemode-defaults.ts`).
- **Plugins (v2 Effect API)**: `define({id, effect})` modules receiving typed capability contexts — `ctx.tool` (register tools, `execute.after` hook), `ctx.agent` (transform agent definitions), `ctx.session` (`context`/`compaction`/`generate`/`retry` hooks, synthetic messages), `ctx.event.subscribe`, plus permission, provider, integration, skill, command, VCS, worktree, and websearch domains (`packages/plugin/src/effect/*.ts`). Live examples: Plan-mode reminders (`packages/core/src/plugin/plan.ts`), model prompt/tool curation (`plugin/optimize.ts`), provider auth (`plugin/provider/*`).
- **Plugin loading & surfaces**: npm/Git packages with semver (`plugins` config; `packages/schema/src/config/plugin.ts`), local directories, and `.opencode/plugins/` dirs (docs `plugins.mdx`; loaders `packages/plugin/src/source*.ts`); managed via `opencode plugin`. GUI plugins extend the web/desktop UI (`packages/plugin/src/gui/`, docs `build/plugins/gui/index.mdx`); plugins can also ship TUI feature plugins and keymaps (`packages/plugin/src/tui/`, `packages/tui/test/plugin-keymap.test.tsx`) and talk over an RPC surface (`packages/plugin/src/rpc.ts`).
- **Custom agents**: `.opencode/agents/<name>.md` (legacy `agent(s)/`, `mode(s)/` dirs still discovered) with V2 frontmatter (`mode: primary|subagent`, `permissions`, `model`, `system` body, `disabled`) (`services/www/src/docs/content/migrate-v1.mdx`, `agents.mdx`; resolution through the agent editor transform in `packages/core/src/plugin/agent.ts`).
- **Custom commands**: markdown/JSON templates (§8) executed as prompts with `agent`/`model` overrides.
- **Skills**: `SKILL.md` directories with frontmatter description; advertised when relevant, loaded on demand via the `skill` tool with supporting files beside it; global/project/served-library discovery (`services/www/src/docs/content/skills.mdx`; `packages/core/src/skill/discovery.ts`; built-ins `packages/core/src/plugin/skill/{opencode.md, report.md}`).
- **Formatters**: `formatter` config plus built-in runners (prettier, gofmt, mix, …) with per-language executable discovery (`packages/schema/src/config/formatter.ts`, `packages/core/src/formatter/builtins.ts`, `formatter.ts`, docs `formatters.mdx`).
- **LSP**: not implemented in v2 — config accepted-but-ignored (`migrate-v1.mdx`); no LSP code in `packages/core/src` (only schema/legacy event types remain, e.g. `packages/schema/src/lsp-event.ts`).
- **Server/SDK API**: typed HttpApi with committed OpenAPI (`packages/protocol/openapi.json`, `script/generate-openapi.ts`), generated clients in promise/effect/solid flavors (`packages/client/src/{promise,effect,solid}/`), SSE event feed with a bounded 4,096-capacity subscriber queue (`packages/server/src/event-feed.ts`), WebSocket transports (`packages/tui/src/mini/stream-v2.transport.ts`), the embeddable `packages/sdk` (re-exports schema contracts + client), and `opencode api` for scripted access to any operation.
- **ACP**: an Agent Client Protocol server implementation for editor integrations (`packages/cli/src/acp/` — connection, capabilities, directories, compaction, content).

## 10. Configuration

- **Files & precedence** (`services/www/src/docs/content/config.mdx`): managed/system directory (macOS `/Library/Application Support/opencode`, Linux `/etc/opencode`, Windows `%ProgramData%\opencode`, plus macOS managed plists) → global `~/.config/opencode/opencode.json(c)` → project `opencode.json(c)` / `.opencode/opencode.json(c)`. Files merge lowest→highest; conflicting keys resolved by precedence; list-valued keys (plugins, permissions, instructions) concatenate across files; managed settings cannot be overridden by users. Schema published at `https://opencode.ai/config.json`.
- **Discovered definitions** load from the same directories: `agents/`, `commands/`, `skills/`, `plugins/` (system + global + project) (`config.mdx`; `packages/core/src/config/discovery.ts`).
- **Terminal client config**: one global `cli.json` in v2 (v1 layered `tui.json`/`tui.jsonc` auto-migrated) (`services/www/src/docs/content/migrate-v1.mdx`).
- **Notable top-level keys** (`packages/schema/src/config/config.ts` and `config/*`): `model` (`provider/model#variant`), `share` (legacy), `permissions`, `policies`, `agents`, `commands`, `mcp` (`packages/schema/src/config/mcp.ts`), `providers` (settings/headers/body overlays; `config/provider.ts`), `formatter`, `lsp` (inert), `media`, `tool_output` (bounds), `compaction` (`config/compaction.ts` incl. `buffer` percent), `skills`, `instructions` (extra files/URLs), `references`, `websearch`, `plugins`, `worktree`, `warming` (prompt-cache keep-alive: `prompt`, `interval` default "4 minutes"/"auto" = 90% of cache TTL, `duration` default "30 minutes"; `packages/schema/src/config/warming.ts`), `snapshots`, `watcher`, `shell`, `default_agent`, `update` (disable/notify/auto), `enterprise`, `experimental` (e.g. `subagent_depth`, `artifacts`).
- **Env & misc**: `OPENCODE_LOG_LEVEL`, `OPENCODE_DISABLE_PROJECT_CONFIG`, `OPENCODE_TUI_CHANNEL`, XDG base-dir overrides (`packages/util/src/global-roots.ts`; `migrate-v1.mdx`). JSONC supported; variable interpolation (`packages/core/src/config/variable.ts`); hot reload via watcher (`config/watch.ts`); v1 fields normalized with diagnostics (`config/normalize.ts`); accepted-but-unsupported v1 fields (`logLevel`, `server`, `subagent_depth` top-level, `compaction.tail_turns/prune`, provider whitelists) are ignored with warnings (`migrate-v1.mdx`).

## 11. Standout Features

1. **CodeMode** — a from-scratch interpreter for a JS-like language where the model calls tools as code; no network/fs/process access, REPL semantics, and a `search()` catalog for large tool surfaces (`packages/codemode/README.md`, `packages/core/src/codemode/instructions.ts`).
2. **In-house multi-protocol LLM client** — ~45 wire protocols implemented directly in `packages/ai/src/protocols/`, mapped from the models.dev catalog (`packages/core/src/provider.ts`).
3. **models.dev catalog + bundled snapshot fallback** with per-entry defensive decoding (`packages/core/src/models-dev.ts`).
4. **Durable, event-sourced sessions** — inbox facts + projections, idempotent prompt admission, and crash recovery that settles stale compactions/tool calls on wake (`specs/v2/session.md`, `session/runner/llm.ts`).
5. **First-class mid-turn steering** — steer vs queue delivery at safe step boundaries with control items as boundaries (`specs/v2/session.md`).
6. **Three-trigger compaction** (auto/overflow/manual) with %-of-window buffer, verbatim tail keep, and background pre-computed summaries (`session/compaction.ts`).
7. **Per-step content-addressed snapshots** with changed-file attribution and revert (`packages/core/src/snapshot.ts`).
8. **Plan mode as data** — an agent plus a re-injected `<system-reminder>` protocol and a plan-file-only edit exception (`packages/core/src/plugin/plan.ts`).
9. **Ordered permission rulesets** with saved "always" replies, unattended rulesets, and per-agent deny-by-default profiles (`packages/core/src/permission.ts`, `plugin/agent.ts`).
10. **Location-scoped Effect architecture** — per-directory layer graphs with workerd-portable conditional imports, enabling multi-root workspaces and cloud execution (`packages/core/package.json` imports map).
11. **One server API, five clients** — TUI, web, desktop, VS Code, ACP on generated typed clients with an SSE feed (`packages/protocol`, `packages/client`, `packages/server/src/event-feed.ts`).
12. **Provider OAuth matrix** — device flow, browser callback, PKCE, azure-cli, ChatGPT account sharing, SSO-aware Console flows (`packages/core/src/plugin/provider/`).
13. **MCP with OAuth and CodeMode-by-default** tool exposure preserving plugin precedence across refreshes (`packages/core/src/mcp/oauth.ts`, `plugin/mcp-codemode-defaults.ts`, `tool/AGENTS.md`).
14. **Prompt-cache warming** — idle keep-alive prompts aligned to model cache TTLs (`packages/schema/src/config/warming.ts`).
15. **Rust PTY daemon** — threaded terminal service with libghostty state, authenticated local sockets/pipes, and owner-handoff tickets (`packages/process-daemon/README.md`).
16. **Scheduled unattended agent jobs** with their own permission semantics (`tool/plugin/schedule.ts`, `Permission.Unattended`).

## 12. Lessons for Marfim

- **Adopt catalog+snapshot**: fetch a community model catalog, decode per-model defensively (skip unknowns), cache, and bundle a snapshot for offline first-run — this removes the biggest cold-start/fragility risk (`packages/core/src/models-dev.ts`).
- **Adopt ordered permission rules (allow/deny/ask) with saved replies and an unattended ruleset**: one evaluation function, one assert chokepoint in tools, easy to explain to users (`packages/core/src/permission.ts`).
- **Adopt the durable-inbox turn model**: enqueue durably before executing, deliver steers at step boundaries, and make compaction/move control items explicit boundaries — the cleanest steering design observed in any surveyed agent (`specs/v2/session.md`).
- **Adopt registry semantics for tools**: namespacing, latest-wins registration with disposal, per-request captured snapshots, and a single tool shape shared by built-ins, plugins, and MCP (`packages/core/src/tool/AGENTS.md`).
- **Adopt generic output bounding** (lines/bytes caps, backing file, tail preview, retention) instead of per-tool truncation logic (`packages/core/src/tool-output.ts`).
- **Adopt data-driven prompt assembly**: base template + per-model-family overlays + orthogonal instruction sources (env, repo instructions, tool catalog, skills), so prompts are testable artifacts (`session/system-prompt.ts`, `plugin/optimize.ts`).
- **Adopt crash-consistency**: settle stale tool calls/compactions on wake rather than trusting volatile state — cheap to implement, huge reliability win (`session/runner/llm.ts`).
- **Be cautious with a CodeMode bet**: it cuts native tool-schema overhead, but drags in an interpreter, a catalog, `search`, and extra instructions; adopt only if Marfim's tool count makes native calling measurably expensive (`packages/codemode/`).
- **Avoid per-vendor tool-list hacks**: opencode deletes grep/glob from GPT/Claude tool lists and admits in-code that the UX is unsettled (`plugin/optimize.ts`); keep one consistent search UX instead.
- **Budget for feature removal**: LSP (shipped in v1, silently disabled in v2 with config ignored) and share (orphaned config key) show the maintenance cost of ambient features; if Marfim ships them, plan explicit removal paths and migration docs like opencode's (`migrate-v1.mdx`).
- **Weigh the Effect-TS trade-off**: the concurrency/cancellation guarantees (fiber-per-tool, uninterruptible final settlement, scoped registration) are excellent, but readability cost is high; Marfim can replicate the guarantees on a smaller concurrency core.
- **Record traffic in tests**: cassette-based HTTP/WS record-replay with redaction plus a scripted provider backend made deep agent-loop tests practical here; Marfim should budget for this early (`packages/http-recorder`, `packages/simulation`).
- **Ship client/server with generated clients from day one**: one typed protocol fed TUI, web, desktop, VS Code, ACP, and scripting (`packages/protocol`, `packages/client`) — the highest-leverage structural decision in the codebase.
