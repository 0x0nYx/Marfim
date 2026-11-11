# Forensic Analysis: Goose (https://github.com/block/goose)

- **Analyzed commit:** `cfb67d72f6129ba3c9f68d4c225d8dfcaba18a87` ("refactor(desktop): drop tool-specific tool call titles (#13107)", dated 2026-11-02).
- **Provenance note:** `git remote` is still `block/goose`, but README badges, CI URLs and release downloads point at the `aaif-goose/goose` org — goose moved to the Agentic AI Foundation (AAIF) at the Linux Foundation (`README.md`, `documentation/blog/2026-04-07-goose-moves-to-aaif/index.md`). Workspace version 1.57.0, rust-version 1.94.1 (`Cargo.toml`).
- **License:** Apache-2.0 (`LICENSE`; `Cargo.toml` `workspace.package.license`).
- **Primary languages:** Rust ≈304k LOC under `crates/` (incl. tests; `crates/goose` alone ≈192k, of which ≈17k is `crates/goose/tests/`); TypeScript/TSX ≈94k in `ui/desktop`; Python ≈5k (`crates/goose-sdk/python/`, examples). Counts via `rg --files | xargs wc -l`; vendored/deliberate vendored assets (`crates/goose-mcp/src/autovisualiser/templates/assets/`) are negligible.
- **Crate inventory (`ls crates/`):** goose, goose-agent, goose-acp-macros, goose-cli, goose-context-management, goose-download-manager, goose-local-inference, goose-mcp, goose-provider-types, goose-providers, goose-roaming, goose-sdk, goose-sdk-types, goose-test, goose-test-support.
- **Structural correction to the known context:** there is **no `goose-server` crate and no `goose-recipe` crate** — server functionality is the ACP server inside `crates/goose/src/acp/` (plus `goose serve`), and recipes live in `crates/goose/src/recipe/`.
- **Workspace feature flags** referenced across crates: `scheduler`, `telemetry`, `acp-http`, `roaming`, `tree-sitter`, `platform-apps`, `chat-recall`, `code-mode`, `local-inference` (see `#[cfg(feature …)]` guards in `crates/goose/src/agents/platform_extensions/mod.rs` and `crates/goose-cli/src/cli.rs`).

## 1. Identity & Purpose

- Goose is a general-purpose AI agent — "desktop app, CLI, and API — for code, workflows, and everything in between" (`README.md`). It is explicitly *not* only a coding agent; docs tutorials cover research, data analysis, and automation.
- MCP-first design: capabilities arrive as external "extensions" (MCP servers), "builtin" bundled MCP servers, or in-process "platform extensions" (`crates/goose/src/agents/extension.rs`, `agents/platform_extensions/mod.rs`). README claims 15+ providers and 70+ extensions.
- One agent core, four surfaces: interactive CLI (`goose-cli`), headless runs (`goose run`), an ACP agent server for editors and the desktop app (`goose acp` / `goose serve`, `crates/goose/src/acp/`), and an Electron desktop app (`ui/desktop`) driving the same core over ACP.
- Authored by AAIF (`Cargo.toml` `authors = ["AAIF <ai-oss-tools@block.xyz>"]`); system prompt self-identifies as "created by AAIF (Agentic AI Foundation)" (`crates/goose/src/prompts/system.md`).
- Repo also carries non-agent auxiliaries to exclude from any LOC/feature accounting: docusaurus docs + blog (`documentation/`), a Discord bot (`services/ask-ai-bot/`), an eval harness (`evals/harbor/`), and `oidc-proxy/`.

## 2. Architecture (crates layout, CLI/server/desktop split, data flow)

- `crates/goose-agent` — framework only:
  - generic effect-sourcing state machine (`src/machine.rs`);
  - `Operation`/`Inference` traits, `ConversationEffect`, `Emitter` (`src/operation.rs`);
  - reusable inference runner and skill roots (`src/inference.rs`, `src/skills/`).
  - Knows nothing about goose specifics; `wasm32`-aware (`#[cfg(target_arch = "wasm32")]` in `machine.rs`).
- `crates/goose` — the actual agent core (~175k src LOC):
  - `src/agents/`: extension manager + leases, tool execution, platform extensions, pipeline ops, state machine, provider manager, prompt manager, moim turn-context;
  - `src/session/`: SQLite session manager, naming, chat-history search, export/import;
  - `src/config/`, `src/permission/`, `src/security/`, `src/recipe/`, `src/scheduler/`, `src/gateway/` (Telegram), `src/acp/` (server + transports), `src/skills/`, `src/plugins/` + `src/hooks/`, `src/goose_apps/`, `src/live_voice/`, `src/dictation/`, `src/prompts/`.
- `crates/goose-cli` — clap CLI (`src/cli.rs`, ~3.3k lines) and the interactive REPL (`src/session/`: input, completion, editor, elicitation, paste, streaming buffer, thinking display, task-execution display).
- `crates/goose-provider-types` — shared protocol layer:
  - conversation/messages/tool requests with careful serde (`src/conversation/`, `conversation/tool_result_serde.rs`);
  - `ModelConfig` (`src/model.rs`), provider errors + retry (`src/errors.rs`, `src/retry.rs`);
  - wire formats (`src/formats/`: openai, openai_responses, anthropic, google, ollama, snowflake, databricks);
  - thinking effort (`src/thinking.rs`), context-limit resolver (`src/context_limit.rs`), canonical model catalog ingested from models.dev (`src/canonical/models_dev.rs`, snapshots in `src/canonical/data/`).
- `crates/goose-providers` — provider implementations (`src/anthropic.rs`, `openai.rs`, `google.rs`, `bedrock.rs`, `azure.rs`, `ollama.rs`, `openrouter.rs`, `databricks.rs`, `snowflake.rs`, declarative engine `src/declarative/`), plus live-voice realtime providers (`src/live.rs`, `openai_live.rs`).
- `crates/goose` provider facade — `src/providers/` holds registry/init, OAuth flows, toolshim, ACP-bridges and the `Provider` trait (`base.rs`); three-layer split (types crate / impl crate / facade) is deliberate.
- `crates/goose-mcp` — four bundled MCP servers: computercontroller, memory, tutorial, autovisualiser (`src/mcp_server_runner.rs`).
- `crates/goose-context-management` — standalone compaction library (`summarize`, `structured` summaries, templates); exported to Python/Kotlin via goose-sdk (`src/lib.rs` doc comment).
- `crates/goose-local-inference` — GGUF inference via llama.cpp (`llama-cpp-2`) with optional MLX on Apple silicon; HF-hub discovery, CUDA/Vulkan backends (`README.md`).
- `crates/goose-roaming` — iroh-based p2p: persisted ed25519 node key, connection cards, TrustBook allowlist, `goose-acp/1` ALPN (`src/lib.rs`).
- `crates/goose-sdk` / `goose-sdk-types` — uniffi bindings (Python, Kotlin/Maven) and DTOs for ACP custom methods (preferences, provider setup, sources, export formats — `goose-sdk-types/src/custom_requests.rs`).
- `ui/` — `desktop` (Electron via electron-forge + React + Vite + Playwright), `goose-acp` (npm launcher resolving platform binaries), `goose-acp-client` (TS ACP client), `goose-binary/*` (per-platform npm binary packages) (`ui/package.json`).

```
 CLI (goose-cli REPL, in-process)  Desktop (Electron; gooseServe.ts spawns binary)  Editor (Zed etc.)
        |                              acp/acpConnection.ts over stdio/HTTP              |
        +-----------------+------------------------+----------------------------------+
                          v  Agent Client Protocol: stdio | HTTP+WS `goose serve` (default 127.0.0.1:3284) | iroh p2p `goose roam`
             crates/goose/src/acp/server/*  (sessions, permissions, tools, prompts, recipes, providers, apps)
                          v
             StateMachineServices -> state-machine pipeline (crates/goose/src/agents/)
             SessionManager (SQLite sessions.db)   ExtensionManager (MCP clients)   ProviderManager
                          |                                  |                                  v
                          v                                  v                            LLM HTTPS APIs
             data dir (sessions.db)     stdio | streamable_http | builtin(duplex) | platform(in-proc)
```

- The desktop app is a *thin client*: it spawns the Rust binary (`ui/desktop/src/gooseServe.ts`), leases ports (`gooseServeLeaseRegistry.ts`), and speaks ACP custom methods for config/providers/permissions (`ui/desktop/src/acp/*`).
- Sessions, permissions, and recipes are stateful on disk; extensions and providers are stateless processes or in-process clients the agent orchestrates per turn.

## 3. Agent Loop (turn lifecycle, tool dispatch, retries, compaction)

- **Core loop** (`crates/goose-agent/src/machine.rs`):
  - `StateMachine` holds ordered `Step`s (each an `Operation`, plus one final `Inference` step);
  - `step()` runs operations in order; `NotApplicable` falls through; the first `Applied` returns effects and ends the scan; falling through everything yields;
  - `run()` = load session → step → apply effects → repeat until `Yielded`/`Cancelled`;
  - cancellation is a `CancellationToken` racing each step via biased `tokio::select!`;
  - `finalize()` synthesizes error tool-responses for unanswered tool requests (`interrupted_response`) so transcripts stay provider-valid after interrupts.
- **Pipeline assembly** (`crates/goose/src/agents/state_machine_builder.rs`): SlashCommand → ToolApproval → Skill → Recipe → ToolExecution → RemainingToolCalls → Subagent (concurrency `GOOSE_MAX_CONCURRENT_SUBAGENTS`, default 5) → Steer → BangShell → [Compaction unless provider `manages_own_context`] → ToolPairCompaction → Doctor → Project → Retry → EmptyResponse → StopHook → ExitOnError → MaxTurns → Inference.
- **Operations also serve slash commands**: `ops_slash_command.rs` dispatches `/…` commands to every registered op's `run_command`.
- **Effects** (`state_machine/effects.rs`): `GooseEffect` = Conversation (append/replace/patch tool-request meta/message visibility), `CompactConversation`, `SetRecipe`, `SetExtensionState`, `RecordUsage`; applied through `SessionManager` in `state_machine/session.rs`.
- **Turn context**: before inference a `<turn-context>` system block carries current time, working dir, extension context, and `<turn-budget>` once ≥50% of `max_turns` is used (`agents/moim.rs`; `ops_maxturns.rs::turn_budget_part`; `MIN_CONTEXT_FOR_MOIM = 32_000`).
- **Inference** (`goose-agent/src/inference.rs` + `state_machine/ops_llm.rs`): tools + prompt parts collected from *all* operations (`collect_inference_input`; duplicate tool names are a hard error); streaming; usage recorded; unclaimed tool errors enriched with advertised-tool list (`enrich_unclaimed_tool_errors`).
- **Tool approval flow** (`ops_tool_approval.rs`): `ActionRequiredData::ToolConfirmation` / `ToolConfirmationResponse` messages; approvals expire when the extension lease is gone (`EXPIRED_APPROVAL_RESPONSE`); declines yield `DECLINED_RESPONSE`; Chat mode yields `CHAT_MODE_TOOL_SKIPPED_RESPONSE` (`agents/tool_execution.rs`).
- **Tool dispatch** (`ops_toolcalling.rs`):
  - pending `ToolRequest`s routed to the owning extension via `ExtensionManager::dispatch_tool_call`;
  - per-tool `execute_tool` tracing span with gen_ai operation attributes (`tool_span`);
  - hooks run pre/post; results may carry a `platform_notification` meta key re-emitted as a server notification;
  - results categorized Shell/Read/Write/Other (`categorize_tool`) for streaming priorities;
  - mangled tool names recovered (`functions.` prefixes, dot-for-underscore — `extension_manager/mod.rs::recover_mangled_tool_name`).
- **Unclaimed requests** (`ops_remaining_tool_calls.rs`): answers any tool request no other op handled ("This tool call was not handled."), tags `goose.unclaimed_tool`, and yields while approvals are pending.
- **Retries — two layers**:
  - provider-level `retry_operation`: 3 attempts, 1s initial, ×2 backoff, 30s cap, transient-only (rate limit / server / network) — `goose-provider-types/src/retry.rs`;
  - recipe-level `RetryConfig`: `max_retries`, shell `SuccessCheck`s, `on_failure` cleanup; 300s/600s default timeouts — `agents/types.rs`, enforced by `ops_retry.rs`.
  - Error-as-prompt philosophy documented in `documentation/docs/goose-architecture/error-handling.md` (tool errors become tool results the model corrects).
- **Compaction**:
  - `ops_compaction.rs` triggers when `tokens/context_limit > threshold` (default 0.8, `GOOSE_AUTO_COMPACT_THRESHOLD`) capped by `DEFAULT_AUTO_COMPACT_TOKEN_LIMIT = 225_000` — headroom below OpenAI's >272k long-context price tier (`context_mgmt/mod.rs` doc comment);
  - context-window errors retry compaction up to `MAX_CONTEXT_ERROR_COMPACTIONS = 2`;
  - unreported tool tokens after the last tool call are counted (`unreported_tool_tokens`);
  - `ToolPairCompactionOperation` (`ops_tool_pair_compaction.rs`) summarizes old tool call/result pairs in batches of 10 (`TOOLCALL_SUMMARIZATION_BATCH_SIZE`);
  - summarization engine: `goose-context-management/src/summarize.rs` + structured summaries (`structured.rs`) + templates (`templates.rs`, registered in `prompt_template.rs`).
- **Misc guards**: large tool responses above `GOOSE_MAX_TOOL_RESPONSE_SIZE` (default 200_000) handled by `agents/large_response_handler.rs`; `ops_empty_response.rs` handles blank model outputs; `ops_stop_hook.rs` lets Stop hooks block turn end up to `stop_hook_block_cap`; `ops_exit_on_error.rs` terminates on fatal errors; MaxTurns default 1000 (`goose-cli/src/cli.rs` line ~137).

## 4. Tool Set (built-in tools + bundled MCP extensions)

**Platform extensions (in-process; registry in `crates/goose/src/agents/platform_extensions/mod.rs`):**

| Extension (default state) | Tools / purpose | File |
|---|---|---|
| developer (on, unprefixed) | `write`, `edit`, `shell`, `tree`, `read_image` — tool list asserted in unit test | `platform_extensions/developer/mod.rs` (+ `shell.rs`, `shell_process.rs`, `shell_output_streaming.rs`, `edit.rs`, `image.rs`, `tree.rs`) |
| analyze (on; feature `tree-sitter`) | directory overviews, file details, symbol call graphs | `platform_extensions/analyze/{mod,parser,graph,languages,format}.rs` |
| extensionmanager (on) | discover/enable/disable extensions, search registry | `platform_extensions/ext_manager.rs` |
| summon (on, unprefixed) | delegate tasks to subagents; load knowledge sources (subrecipes/recipes/agents) | `platform_extensions/summon.rs` |
| skills (on) | discover skills; `load_skill` with supporting-file access | `crates/goose/src/skills/{mod,client}.rs` |
| tom "Top Of Mind" (on) | inject GOOSE_MOIM_MESSAGE_TEXT/FILE context every turn | `platform_extensions/tom.rs`, `agents/moim.rs` |
| todo (off) | persistent todo list for the agent | `platform_extensions/todo.rs` |
| apps (feature `platform-apps`) | create/manage HTML/CSS/JS apps in sandboxed windows | `platform_extensions/apps.rs`, `crates/goose/src/goose_apps/` |
| chatrecall (feature `chat-recall`) | search past conversations / load summaries | `platform_extensions/chatrecall.rs` |
| scheduler (hidden; feature `scheduler`) | `manage_schedule` cron CRUD for recipes | `agents/platform_tools.rs`, `platform_extensions/scheduler.rs`, `src/scheduler/full.rs` |
| summarize (off) | LLM summary of files/dirs in one call | `platform_extensions/summarize.rs` |
| code_execution (feature `code-mode`) | "Code Mode": call extensions through generated code to save tokens | `platform_extensions/{code_execution,javascript}.rs` |

**Bundled MCP servers (`crates/goose-mcp`; run as stdio servers via `goose mcp <name>`, `src/mcp_server_runner.rs`):**

| Server | Tools | File |
|---|---|---|
| computercontroller | `xlsx_tool` (read/modify spreadsheets), `docx_tool` (extract/create docs), `pdf_tool` (text+images), macOS-only `computer_control` (external Peekaboo CLI: see → click → type UI automation) | `src/computercontroller/mod.rs`, `xlsx_tool.rs`, `docx_tool.rs`, `pdf_tool.rs`, `peekaboo/mod.rs` |
| memory | `remember_memory`, `retrieve_memories`, `remove_memory_category`, `remove_specific_memory`; global vs working-dir scope | `src/memory/mod.rs` |
| tutorial | serves markdown tutorials (`build-mcp-extension`, `first-game`) | `src/tutorial/mod.rs`, `tutorials/` |
| autovisualiser | HTML visual generation: chart, sankey, radar, donut, treemap, chord, interactive map (Leaflet), mermaid | `src/autovisualiser/mod.rs`, `templates/` (incl. `assets/mcp-app-bridge.js`) |

- Desktop default set: `ui/desktop/src/built-in-extensions.json` (developer on; computercontroller, autovisualiser, memory, tutorial off).
- Naming: external tools are prefixed `ext__tool` unless the extension is first-class (`unprefixed_tools`; developer/analyze/summon/skills are flat) — `extension_manager/mod.rs` ~lines 343–363.
- MCP `ToolAnnotations` (e.g. `read_only_hint`) drive read-only detection (`permission_inspector.rs::apply_tool_annotations`).
- Historical note: the old `text_editor` tool is gone (developer exposes `write`/`edit`); computercontroller's cross-platform screenshot/click tools are gone — only macOS/Peekaboo `computer_control` remains.

## 5. Model/Provider Layer

- **Registered providers** (`crates/goose/src/providers/init.rs`, lines ~64–183):
  - first-party APIs: anthropic, openai, google, azure, azure-foundry, bedrock, gcp-vertex-ai, xai (+ xai-oauth), databricks (+ databricks-v2), snowflake, githubcopilot, huggingface;
  - local/self-hosted: ollama, local-inference (llama.cpp/MLX), litellm, sagemaker-tgi, gondola, avian, nanogpt;
  - aggregators/routers: openrouter, tetrate;
  - subscription/agent bridges: claude-acp, claude-code, codex, chatgpt-codex, codex-acp, copilot-acp, cursor-agent, amp-acp, pi-acp, kimicode, muse-code, gemini-cli, gemini-oauth.
- **ACP bridges** are registered with `runs_own_tool_loop = true` (e.g. `register::<ClaudeCodeProvider>(true)`, `init.rs` lines 90/99) and are exempt from host compaction via `manages_own_context` (`state_machine_builder.rs` lines 70/99–106).
- **53 declarative providers**: JSON definitions in `crates/goose-providers/src/declarative/definitions/*.json` — groq, mistral, together, deepseek, moonshot, cerebras, perplexity, fireworks, nvidia, lmstudio, ollama_cloud, vercel_ai_gateway, ovhcloud, scaleway, alibaba, zai, zhipu, zai_coding_plan, minimax, and many community routers; engine in `declarative.rs` + `macros.rs`, registered by `crates/goose/src/config/declarative_providers.rs`.
- **Auth**:
  - secrets resolve env (uppercase key) → OS keyring (service `goose`, username `secrets`; `GOOSE_DISABLE_KEYRING`) → `secrets.yaml` in config dir with 0600 tightening (`config/base.rs` lines 32–35, tests lines 2400–2432);
  - OAuth: Google (`providers/gcpauth.rs`, `oauth_device_flow.rs`), Azure (`azureauth.rs`), xAI (`xai_oauth.rs`), Gemini (`gemini_oauth.rs`), HuggingFace (`huggingface_auth.rs`), Databricks (`goose-providers/src/databricks_auth.rs`), Tetrate;
  - user-defined auth shell commands (`providers/command_auth.rs`; test `tests/custom_provider_command_auth_test.rs`).
- **Model parameters** (`goose-provider-types/src/model.rs`): model_name, context_limit, temperature, max_tokens, toolshim + toolshim_model (interpreter model for non-tool-calling models — `crates/goose/src/providers/toolshim.rs`), request_params (e.g. `anthropic_beta`), reasoning, supports_vision, per-request headers. Reasoning is format-specific (`formats/openai_responses.rs` for Responses-API models; `thinking.rs` `ThinkingEffort`).
- **Context limits** (`goose-provider-types/src/context_limit.rs`): explicit override → per-model configured limit → canonical catalog → `DEFAULT_CONTEXT_LIMIT = 128_000` (`model.rs`). Catalog ingests models.dev (`canonical/models_dev.rs::from_models_dev`) plus bundled snapshots (`canonical/data/canonical_models.json`, `provider_metadata.json`); costs in `crates/goose/src/providers/canonical_cost.rs`.
- **Lead/worker model routing: not found.** No `GOOSE_LEAD*`/`GOOSE_WORKER*`/lead-worker code exists anywhere under `crates/` (grep verified). Multi-model is achieved by:
  - per-session `/model` switching;
  - per-subagent provider+model+temperature+max_turns in `summon::DelegateParams` (`platform_extensions/summon.rs`);
  - recipe `Settings { goose_provider, goose_model, temperature, max_turns }` (`recipe/mod.rs`);
  - toolshim interpreter model;
  - subagents cannot spawn further subagents (`prompts/subagent_system.md`).
- **Token budgets / usage accounting**: `usage_ledger` SQLite table (per-entry model, cache read/write tokens, cost, `cost_source`, `is_compaction`) plus accumulated columns on the session row (`session/session_manager.rs` schema); `GOOSE_MAX_TOKENS` / `GOOSE_CONTEXT_LIMIT` getters (`config/base.rs`); provider session-id reuse for stateful backends (`agents/mod.rs::latest_provider_session_id`).

## 6. Permissions & Safety

- **Modes** (`goose-provider-types/src/goose_mode.rs`):
  - `Auto` — automatically approve tool calls (default);
  - `Approve` — ask before every tool call;
  - `SmartApprove` — ask only for sensitive tool calls;
  - `Chat` — chat only, no tool calls.
  - Persisted per session (`sessions.goose_mode` column, default `auto`), switchable mid-session via `/mode` (`goose-cli/src/session/completion.rs`).
- **Per-tool allowlist store** (`crates/goose/src/config/permission.rs`): `permission.yaml` in config dir; sections `user` and `smart_approve` map tool → `AlwaysAllow` / `AskBefore` / `NeverAllow`; managed by a global `PermissionManager` singleton.
- **LLM read-only judge** (`permission/permission_judge.rs`): declares `platform__tool_by_tool_permission` and asks the session's provider to classify pending requests as read-only vs write (SELECT vs INSERT examples baked into the tool description); `PermissionInspector` demotes any non-read-only tool to `AskBefore` under smart approve and caches the decision (`permission_inspector.rs::cache_non_readonly_decision`).
- **Inspector pipeline** (`agents/services.rs` lines ~555–572 + `tool_inspection.rs` trait; order = registration order):
  1. SecurityInspector — model-based prompt-injection scanner with env override `SECURITY_PROMPT_ENABLED_OVERRIDE` (`security/scanner.rs`, `classification_client.rs`, `security/mod.rs`);
  2. EgressInspector — exfiltration guarding (`security/egress_inspector.rs`);
  3. AdversaryInspector — user rules over shell calls; default rules BLOCK exfiltration/destructive/privilege-escalation, ALLOW normal dev work (`security/adversary_inspector.rs`);
  4. PermissionInspector — mode + permission.yaml + LLM judge;
  5. RepetitionInspector — consecutive identical name+params calls (`tool_monitor.rs`; CLI `--max-tool-repetitions`).
  - Non-permission inspectors override baseline decisions; anything undecidable defaults to needs-approval (`permission_inspector.rs::process_inspection_results`).
- **Tool allowlists per extension**: `ExtensionConfig.available_tools` restricts exposed tools (`agents/extension.rs`); extension `instructions` are injected into the system prompt (`prompt_manager.rs::with_extension`, `prompts/system.md` Extensions section).
- **Dangerous-command pattern library**: curated regex families (remote code execution, data exfiltration, system modification, network access, process manipulation, privilege escalation, obfuscation) — `security/patterns.rs`; git-specific checks `crates/goose/tests/git_command_security.rs`; schedule-tool security `tests/schedule_tool_security.rs`.
- **Extension screening**: malware check before load (`agents/extension_malware_check.rs`); explicit `goose validate-extensions` command (`goose-cli/src/cli.rs` line ~1206).
- **Hooks as guardrails**: command hooks can veto tools (PreToolUse) and block stopping (`ops_stop_hook.rs` with `stop_hook_block_cap`); events: PreToolUse, PreToolUseResult, PostToolUse, PostToolUseFailure, SessionStart, SessionEnd, Stop (`hooks/mod.rs`); 30s default timeout; JSON event on stdin.
- **Platform-specific commands**: developer `shell` picks the login shell per OS (PowerShell/Windows branch — `shell.rs::windows_shell`), spawns through the Flatpak host when sandboxed (`shell.rs::flatpak_spawn_command`), and runs child cleanup helpers (`tests/developer_shell_cleanup.rs`); `computer_control` is macOS-only via Peekaboo; builtin extensions can run inside Docker (`agents/container.rs`, `extension_manager/builtin.rs` `docker_exec`).
- **Server / distribution safety**: `goose serve` requires `GOOSE_SERVER__SECRET_KEY` unless `--dangerously-unauthenticated`; exact-origin CORS allowlist; optional TLS (`goose-cli/src/cli.rs` Serve variant, `acp/transport/auth.rs`, `transport/tls.rs`); roaming grants access only via mutual ed25519 key acceptance with revocations (`goose-roaming/src/lib.rs`, `trust.rs`); `goose update` refuses archives without valid Sigstore attestations (`goose-cli/src/commands/update.rs` lines ~249–325).

## 7. Sessions, State & Context

- **Storage**: SQLite `sessions.db` in the data dir (`session/session_manager.rs`: `SESSIONS_FOLDER`, `DB_NAME`, global `SESSION_STORAGE` rooted at `Paths::data_dir()`).
- **Schema highlights** (`session_manager.rs` CREATE TABLE block, lines ~1164–1240):
  - `sessions`: working_dir, name/user_set_name, session_type, goose_mode, recipe_json + user_recipe_values_json, provider_name + model_config_json, parent_session_id, container_id (Docker), system_prompt_override + system_prompt_extras_json, project_id, archived_at, accumulated token/cost columns;
  - `messages`: one row per message with content_json + metadata_json;
  - `usage_ledger`: per-inference usage entries (model, cache tokens, cost, `cost_source`, `is_compaction`).
- **Locations** (`config/paths.rs`): etcetera app strategy with legacy vendor `Block`/`goose` retained for compat — macOS `~/Library/Application Support/Block/goose`, Linux `~/.config/goose`; separate config/data/state dirs; plugins `~/.agents/plugins`, agents `~/.agents/agents`, skills `~/.agents/skills`; `GOOSE_PATH_ROOT` relocates the tree (must be absolute).
- **Session types** (`session_manager.rs`): `User | Scheduled | SubAgent | Hidden | Terminal | Gateway | Acp`; subagent sessions link via `parent_session_id` (`acp/server/load_session.rs` checks `SessionType::SubAgent`).
- **Resume / fork / edit / export / import** (`goose-cli/src/cli.rs` lines ~519–610, ~917–985):
  - `goose session --resume` (last used, or `--name` / `--session-id`), `--fork` (copy history into a new session), `--edit` (open the conversation in `$VISUAL/$EDITOR` before resuming, combinable with `--fork`), `--history`;
  - subcommands: `list` (text/json, `--ascending`, limit), `remove` (interactive or regex), `export` (markdown / HTML — `session/export_markdown.rs`, `session/export_html/`), `import` (JSON or Claude Code / Codex / Pi `.jsonl` — `session/import_formats/`), `rename`, `diagnostics`.
- **Search**: full-text chat-history search (`session/chat_history_search.rs`); chatrecall platform extension for in-agent recall.
- **Legacy migration**: old per-file JSON sessions under a `sessions/` folder are imported at startup (`session/legacy.rs`; `import_legacy_sessions` call in `session_manager.rs` ~line 1288).
- **Compaction / summarization limits** (recap): threshold 0.8, 225k token cap, 2 context-error retries, tool-pair batches of 10; compacted conversations replace history via `save_compacted_conversation` (`state_machine/session.rs`).
- **Recipes as state**: recipe + user parameter values stored on the session row; `RecipeOperation` (`ops_recipe.rs`) injects instructions, enforces `recipe__final_output` for JSON-schema responses (with "call it alone" and "MUST call final_output NOW" continuation prompts — `agents/final_output_tool.rs`), and handles `/recipe` reload.
- **Cross-session memory**: memory extension writes categorized memory files (global vs working-dir scope — `goose-mcp/src/memory/mod.rs::get_memory_file`); `.goosehints` provides project instructions with import support and size limits (`hints/load_hints.rs`, `hints/import_files.rs`); sessions auto-named from the first 3 messages (`session/session_naming.rs`, prompt `prompts/session_name.md`).

## 8. UI/UX

- **CLI subcommands** (`goose-cli/src/cli.rs`, `Command` enum lines ~917–1110 plus sub-enums at 519/612/679/733/806):
  - `session` (alias `s`; subcommands list/remove/export/import/diagnostics/rename; flags `--resume`, `--fork`, `--edit`, `--history`, `--system`, `--name`, `--session-id`);
  - `run` (instruction file or stdin; `InputOptions`, `RunBehavior`, `OutputOptions` incl. `--quiet` and output formats text/json/markdown);
  - `recipe` (validate / deeplink / open / list), `skills`, `plugin` (git install/update), `schedule` (add/list/remove/sessions/run-now/deprecated services-status|stop/cron-help);
  - `gateway` (status/start/stop/pair — Telegram bot, `commands/gateway.rs`, `crates/goose/src/gateway/telegram.rs`);
  - `update` (self-update with Sigstore), `term` (bash/zsh/fish/nu/pwsh integration scripts: `@goose`/`@g` aliases, history preexec hook, command-not-found handler — `commands/term.rs`);
  - `local-models`, `completion`, `review` (severity threshold, custom binary; deterministic check orchestration — `commands/review/orchestrator.rs`);
  - `configure`, `info`, `doctor`, `mcp` (run bundled server by name), `acp` (stdio agent), `roam` (share/connect), `serve` (ACP over HTTP+WS with `--with-builtin`, TLS, CORS, `--enable-scheduler`, `--roam`);
  - helpers: `validate-extensions`, `mcp-probe`, `search`, `download`, `init`, `log`.
  - **No `sponge` subcommand exists** (verified; only a coincidental Whisper-vocab match in `dictation/whisper_data/tokens.json`).
- **In-REPL commands**:
  - CLI-handled: `/model`, `/mode`, `/extension`, `/builtin`, `/prompt`, `/prompts`, `/skills`, `/new`, `/exit`, `/quit`, `/help`, `/t` (theme toggle) — `goose-cli/src/session/input.rs` line 247, `completion.rs`;
  - agent-handled: `/compact` (aliases "Please compact this conversation", `/summarize`), `/clear`, `/doctor`, `/status`, `/recipe`, `/skills`, `!cmd` shell passthrough (`ops_slash_command.rs`, per-op `run_command`, `ops_bang_shell.rs`);
  - `goose doctor` starts a headless session whose first message is `/doctor` (`commands/doctor.rs`) — the agent diagnoses its own setup.
- **Themes**: `Theme::{Light, Dark, Ansi(base16)}` persisted in config (`goose-cli/src/session/output.rs` lines 52–77).
- **Desktop** (`ui/desktop`):
  - main process spawns the goose binary and connects over ACP (`src/gooseServe.ts`, `src/acp/acpConnection.ts`, `capabilities.ts`, `chatSessionController.ts`);
  - React renderer: Hub (extension gallery), ToolApprovalButtons / ToolCallConfirmation / ToolCallStatusIndicator, RecipeHeader + consent + parameter modals, SubagentSessionNotice, LiveVoiceButton, McpApps window host, MentionPopover, MessageQueue, usage stats (`src/components/`);
  - infrastructure: i18n (`I18N.md`), announcements, auto-update (`updates.ts`, `app-update.yml`), proxy + certificate verification (`proxy.ts`, `backendCertificateVerifier.ts`), remote backends (`remoteBackends.ts`), Playwright e2e + vitest unit tests (`package.json`).
- **ACP / editor integration**: `goose acp` (stdio) with generated schema `crates/goose/acp-schema.json` (`src/bin/generate_acp_schema.rs`); ACP custom methods via `goose-acp-macros` proc macros typed in `goose-sdk-types/src/custom_requests.rs`; npm entrypoint `@aaif/goose-acp` + per-platform `@aaif/goose-binary-*` packages (`ui/goose-acp/README.md`, `ui/goose-binary/README.md`).
- **Voice & dictation**: realtime live voice (`goose-providers/src/live.rs`, `openai_live.rs`, `browser_live_transport.rs`, `live_transport_websocket.rs`) with a desktop controller; local Whisper dictation with bundled vocab (`crates/goose/src/dictation/whisper.rs`, `whisper_data/`).

## 9. Extensibility (MCP-first extension model)

- **Transport kinds** (`agents/extension.rs` `ExtensionConfig`, line ~158):
  - `stdio` — cmd/args/envs/env_keys/cwd/timeout;
  - `builtin` — bundled in-binary server;
  - `platform` — in-process, full agent access;
  - `streamable_http` — uri, headers, envs, OAuth client_id / client_secret_key / scopes (PKCE, `$VAR` substitution), optional Unix-domain-socket routing incl. Linux `@name` abstract sockets, timeout.
  - **No separate SSE variant** — SSE is handled inside the streamable-http client (`extension_manager/streamable_http.rs`); no CLI-extension type remains (verified absent).
- **Builtin servers** run in-process over tokio duplex streams via a global registry (`builtin_extension.rs`; `extension_manager/builtin.rs`), registered by the CLI from `goose_mcp::BUILTIN_EXTENSIONS`; can be executed inside Docker (`docker exec … goose mcp <name>`).
- **Client capabilities** (`agents/mcp_client.rs`):
  - resources supported (`list_resources`); elicitation supported and correlated to the active tool call (`create_elicitation`, `elicitation.rs`, `action_required_manager.rs`);
  - MCP Apps UI capability (`capabilities.mcpui`; `acp/mcp_app_proxy.rs`; bridge asset `goose-mcp/src/autovisualiser/templates/assets/mcp-app-bridge.js`);
  - **sampling deliberately not advertised** — test asserts `capabilities.get("sampling").is_none()` (despite the marketing blog `documentation/blog/2025-12-04-mcp-sampling/index.md`);
  - OAuth for HTTP extensions: dynamic client registration or pre-registered client IDs with fallback (`streamable_http.rs::should_attempt_oauth_fallback`).
- **Tool-surface control**: name prefixing + `available_tools` allowlists + mangled-name recovery (`extension_manager/mod.rs`); default extension timeout 300s (`config/extensions.rs::DEFAULT_EXTENSION_TIMEOUT`).
- **Subrecipes** (`recipe/mod.rs` line ~120): `SubRecipe { name, path, values, sequential_when_repeated, description }`; summon resolves source types Subrecipe/Recipe/Agent (agent = frontmatter file with name/description/model), merging fixed `values` with call-time parameters; delegated runs flagged via `DELEGATED_META_KEY` (`ops_subagent.rs`).
- **Variable interpolation**: minijinja `UndefinedBehavior::Strict` so unknown `{{param}}` references fail validation (`recipe/template_recipe.rs`); parameters typed with input_type, requirement, default, options (`RecipeParameter`).
- **Output schemas**: recipe `response.json_schema` → `final_output` tool injection with rejection messages when called alongside other tools or without being called (`agents/final_output_tool.rs`); unsupported providers get an explicit remediation message.
- **CLI extensions via plugins**: `goose plugin install <git-url>`; `plugin.json` + `hooks/hooks.json` following the Open Plugins hooks spec (`hooks/mod.rs` header; example `examples/plugins/hello-hooks/`); hook matchers like `developer__shell|developer__text_editor`.
- **Skills**: agentskills.io `SKILL.md` frontmatter spec; discovered in project `.goose/skills/`, global `~/.agents/skills/`, and plugin skill dirs (`skills/mod.rs` lines ~47, 170–318); `/skills` slash command; `load_skill` tool with supporting-file read/write guards (`skills/supporting_files.rs`).
- **SDK surface**: uniffi Python/Kotlin bindings re-export provider + compaction APIs (`goose-sdk/justfile`, `maven/`, `python/`) so other runtimes can embed goose inference and summarization.

## 10. Configuration (config.yaml, settings, env vars, profiles)

- **Layering** (`config/base.rs`):
  1. system config — `/etc/goose/config.yaml`, `%PROGRAMDATA%\goose\config.yaml` on Windows (test override `GOOSE_TEST_SYSTEM_CONFIG_PATH`);
  2. additional files — `GOOSE_ADDITIONAL_CONFIG_FILES` path list;
  3. user config — `config.yaml` in the config dir;
  4. env overrides — key uppercased, JSON-parsed (`parse_env_value`).
  - Merging via `merge_config_values`; writes symlink-protected (max 1 hop).
- **Secrets**: env → keyring (service `goose`; `GOOSE_DISABLE_KEYRING`) → `secrets.yaml` (0600, re-tightened if loosened — tests `test_secrets_file_created_with_restricted_permissions`); secret cache invalidated via `generation()` counter.
- **Key parameters** (accessors `base.rs` lines ~1310–1384): GOOSE_PROVIDER, GOOSE_MODEL, GOOSE_CONTEXT_LIMIT, GOOSE_MAX_TOKENS, GOOSE_THINKING_EFFORT, GOOSE_DOCS_ROOT, OPENAI_STORE; cross-cutting env: GOOSE_MODE, GOOSE_AUTO_COMPACT_THRESHOLD, GOOSE_AUTO_COMPACT_TOKEN_LIMIT, GOOSE_MAX_TOOL_RESPONSE_SIZE, GOOSE_MAX_CONCURRENT_SUBAGENTS, GOOSE_ADDITIONAL_CONFIG_FILES, GOOSE_PATH_ROOT, GOOSE_DISABLE_KEYRING, GOOSE_MOIM_MESSAGE_TEXT/FILE (tom extension), SECURITY_PROMPT_ENABLED_OVERRIDE; per-run CLI flags `--max-turns` (default 1000), `--max-tool-repetitions`, `--debug`.
- **Profiles: not found** — no profile feature exists under `config/` (grep verified). Closest mechanisms: recipes (settings + extension sets), per-session overrides, declarative provider configs, and desktop preference keys (`goose-sdk-types/src/custom_requests.rs::PreferenceKey`).
- **Experiments**: flag registry synchronized against `ALL_EXPERIMENTS` (currently an empty list) — `config/experiments.rs`.
- **Extensions & providers in config**: `extensions` map (keys normalized via `name_to_key`) with type-tagged entries; `providers` map for declarative providers (`config/extensions.rs`, `config/declarative_providers.rs`).
- **Workspace / project state**: `goose init` seeds workspace config (`load_init_config_from_workspace`, `base.rs` ~line 1447); `.goosehints` (custom filename configurable), project skills `.goose/skills/`, review checks `.agents/checks/*.md` + `**/.agents/REVIEW.md` (`checks/mod.rs`).
- **Observability**: OpenTelemetry (`otel/`), tracing targets e.g. `goose::state_machine`, PostHog telemetry with consent dialog (`posthog.rs`, `commands/configure.rs::configure_telemetry_consent_dialog`), request logging (`goose-provider-types/src/request_log.rs`).

## 11. Standout Features

1. **Effect-sourcing state machine** — steps return effects; the runtime persists/emits; interrupt finalization repairs dangling tool calls (`crates/goose-agent/src/machine.rs`, `finalize`/`interrupted_response`).
2. **Composable operations pipeline** — 15+ ops each contributing tools and prompt parts to every request (`crates/goose/src/agents/state_machine_builder.rs`, `collect_inference_input`).
3. **Platform extensions** — in-process MCP-shaped clients with full host access (developer shell/edit, summon, scheduler) alongside external MCP servers (`agents/platform_extensions/mod.rs`).
4. **Builtin MCP servers over in-memory duplex streams**, optionally executed inside Docker containers (`builtin_extension.rs`, `agents/container.rs`).
5. **Code Mode** — expose extension calls as code execution to cut token overhead, opt-in (`platform_extensions/code_execution.rs`; blog `documentation/blog/2025-12-15-code-mode-mcp/index.md`).
6. **Summon subagents** — per-subagent provider/model/turn budget, resume support, and frontmatter "agent" files as knowledge sources (`platform_extensions/summon.rs`, `prompts/subagent_system.md`).
7. **Layered tool safety** — LLM read-only judge + security/egress/adversary/repetition inspectors + per-tool `permission.yaml` + veto-capable hooks (`permission/`, `security/`, `agents/services.rs`).
8. **Recipe system** — YAML/JSON, minijinja params, subrecipes, retry + shell success checks + on_failure, JSON output schema enforced via `recipe__final_output`, deeplinks, cron scheduling (`crates/goose/src/recipe/`, `src/scheduler/full.rs`).
9. **Compaction stack** — 0.8-threshold auto-compact with 225k cap, context-error retry (×2), tool-pair summarization batches of 10, extracted into a reusable library (`crates/goose/src/context_mgmt/mod.rs`, `crates/goose-context-management/`).
10. **Diagnostics-as-agent** — `goose doctor` runs an LLM session to repair the user's setup (`goose-cli/src/commands/doctor.rs`, `ops_doctor.rs`).
11. **Deterministic review orchestration** — `goose review` fans checks out as `goose run -q` subprocesses (semaphore 4), each returning strict `{"findings": [...]}` JSON (`goose-cli/src/commands/review/orchestrator.rs`, `crates/goose/src/checks/mod.rs`).
12. **`goose term` shell integration** — generated scripts for 5 shells alias `@goose` and feed command history into sessions (`goose-cli/src/commands/term.rs`).
13. **Roaming p2p** — iroh-based internet exposure of the full ACP surface with an ed25519 trust book and revocations, no open ports (`crates/goose-roaming/src/lib.rs`, `trust.rs`).
14. **Signed self-update** — Sigstore attestation verification before replacing the binary (`goose-cli/src/commands/update.rs`).
15. **Subscription providers via ACP** — Claude Code, Codex, Copilot, Cursor, Gemini CLI as providers with `runs_own_tool_loop`, exempt from host compaction (`providers/init.rs`, `providers/claude_code.rs`, `providers/codex.rs`).
16. **MCP Apps + deliberate no-sampling stance** — extensions can render HTML/JS app windows (mcp-apps bridge, desktop McpApps) while the MCP client does not advertise sampling (`goose-mcp/src/autovisualiser/templates/assets/mcp-app-bridge.js`, capability test in `agents/mcp_client.rs`).

## 12. Lessons for Marfim

- **Adopt the effect-sourcing state machine.** Separating "decide" (operations return effects) from "persist/emit" (runtime applies) makes interruption, steering, compaction and permissions independently testable, and keeps the framework crate (`crates/goose-agent/src/machine.rs`) reusable — the best-architected part of goose.
- **Adopt in-process platform extensions with the MCP interface.** File/shell tools without a subprocess hop while keeping an identical tool-schema surface is why the developer extension is fast yet swappable (`platform_extensions/mod.rs`).
- **Make every pipeline operation contribute tools + prompt parts.** Aggregation via `collect_inference_input` lets skills/recipes/turn-context compose without a monolithic prompt builder, with duplicate-tool detection as a hard error (`machine.rs`).
- **Persist permission decisions at three tiers** — session mode, per-tool user/smart-approve store, cached LLM read-only classification — and default undecidable requests to "ask" (`permission/permission_judge.rs`, `permission_inspector.rs`).
- **Treat agent errors as model-visible prompts.** Encoding recovery hints into error tool results (per `documentation/docs/goose-architecture/error-handling.md`) is cheap self-healing; pair with provider-level transient-only retries.
- **Don't let the core crate become a monolith.** `crates/goose` src is ~175k LOC after absorbing server, recipes, scheduler, gateway and voice; Marfim should keep those separate crates from day one.
- **Design the session schema once.** Goose's JSON→SQLite migration carries many per-column ALTER TABLE migrations plus a legacy importer (`session/session_manager.rs`); include the usage ledger and metadata columns in v1.
- **Make steering, turn budgets, and interruption first-class.** Budget tags in the turn context (`ops_maxturns.rs`, `moim.rs`), queued user messages (`ops_steer.rs`), and synthesized tool responses on cancel (`machine.rs`) are what make long autonomous runs survivable — build them early.
- **Distrust tool names from the model.** Normalize `functions.`-prefixed and dot-mangled namespaced names at dispatch (`extension_manager/mod.rs::recover_mangled_tool_name`); cap consecutive identical calls (`tool_monitor.rs`).
- **Verify folklore before porting.** The "lead/worker model", `goose-server`/`goose-recipe` crates, `sponge` command, and profiles do not exist at this commit; clean-room designs should copy what is in-tree (summon-style subagents, ACP server, recipe handling) rather than outdated blog-era architecture.
