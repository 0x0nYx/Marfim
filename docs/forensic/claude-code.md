# Forensic Analysis: Claude Code — BEHAVIORAL dossier (public knowledge only)

- **Scope discipline:** this dossier documents only publicly observable behavior: the CLI's own
  `--help` surfaces, the public GitHub repository (`github.com/anthropics/claude-code`), and the
  npm package's *shipped public interface files* (`sdk-tools.d.ts`, README, LICENSE). No
  proprietary/minified source was extracted, deobfuscated, or copied; no decompiled excerpts are
  quoted. Names of commands, flags, config keys and shipped type names are interface facts.
- **Analyzed repo commit:** `964d69565952a176ac1975ed218362991db71433` (2026-11-03, "chore: Update
  CHANGELOG.md and feed.xml"), branch `main`.
- **Analyzed installation:** `@anthropic-ai/claude-code` **2.1.317** at
  `/home/z/.npm-global/lib/node_modules/@anthropic-ai/claude-code`; `claude --version` →
  `2.1.317 (Claude Code)` (CLI --help / --version).

## 1. Identity & versioning

- "Claude Code is an agentic coding tool that lives in your terminal, understands your codebase,
  and helps you code faster by executing routine tasks, explaining complex code, and handling git
  workflows — all through natural language commands. Use it in your terminal, IDE, or tag @claude
  on GitHub." (repo `README.md`)
- **Distribution channels** (repo `README.md`): recommended installers are
  `curl -fsSL https://claude.ai/install.sh | bash` (macOS/Linux),
  `irm https://claude.ai/install.ps1 | iex` (Windows), Homebrew (`brew install --cask claude-code`),
  WinGet (`Anthropic.ClaudeCode`); **"Installation via npm is deprecated"** but still supported
  (`npm install -g @anthropic-ai/claude-code`).
- **npm package layout (observed, public files):** `package.json` (bin `claude` → `bin/claude.exe`;
  `postinstall` runs `install.cjs`; `engines.node >=22`; optional per-platform packages
  `@anthropic-ai/claude-code-{darwin,linux,win32}-{arm64,x64,…}` at the same version), a
  331,362,672-byte **ELF x86-64 executable** at `bin/claude.exe` (hard-linked with the platform
  package's `claude` binary; i.e. the current Linux build is a single compiled native executable,
  not a JS bundle), plus `cli-wrapper.cjs`, `install.cjs`, `sdk-tools.d.ts`, README, LICENSE.
- **License:** proprietary — `LICENSE.md`: "© Anthropic PBC. All rights reserved. Use is subject
  to the Legal Agreements outlined here: https://code.claude.com/docs/en/legal-and-compliance."
  (npm package `LICENSE.md`).
- **Versioning cadence:** `CHANGELOG.md` is 9,890 lines of per-release notes (2.1.317 is the
  newest entry in the clone); releases add features, deprecations and env-var knobs at a high
  rate (repo `CHANGELOG.md`).
- **Public repo contents:** README, CHANGELOG, SECURITY.md, `CLAUDE.md` (contributor
  instructions), `examples/` (`gateway/` AWS+GCP LLM gateway Terraform, `hooks/` hook example,
  `mdm/` macOS `.mobileconfig` + Windows ADMX policy templates, `settings/` managed-settings
  presets), `mods/` (four built-in plugins shipped as source), `plugins/README.md` (plugin
  system explanation; actual Anthropic plugins live in `claude-plugins-official`),
  `Script/run_devcontainer_claude_code.ps1`, `scripts/`, `feed.xml`, `demo.gif` (repo tree).

## 2. CLI surface (from `claude --help`, v2.1.317)

- **Invocation:** `claude [options] [command] [prompt]` — "starts an interactive session by
  default, use -p/--print for non-interactive output" (CLI --help).
- **Subcommands** (CLI --help): `agents` (manage background agents), `attach <id|name>`,
  `auth`, `auto-mode`, `doctor`, `gateway` (enterprise auth/telemetry gateway), `import`
  (import config from another AI coding agent), `install [target]` (native build; stable/latest/
  version), `logs <id|name>`, `mcp`, `plugin|plugins`, `purge [path]` (delete a project's Claude
  Code data), `respawn [id]`, `rm <id>`, `setup-token`, `stop|kill <id>`, `ultrareview [target]`
  (cloud-hosted multi-agent code review), `update|upgrade`.
- **Selected flags** (CLI --help; complete list observed):
  - Session/model: `--model <model>` (aliases like 'opus'/'sonnet' or full name), `--effort
    <low|medium|high|xhigh|max>`, `--fallback-model`, `--betas`, `--append-system-prompt`,
    `--system-prompt`, `--system-prompt-snapshot <on|off>`,
    `--exclude-dynamic-system-prompt-sections`, `--agent <agent>`, `--agents <json-or-file>`.
  - Continuity: `-c/--continue`, `-r/--resume [id]`, `--fork-session`, `--session-id <uuid>`,
    `-n/--name`, `--from-pr [value]`, `--teleport [session]`, `--no-session-persistence`,
    `--bg/--background` (background session; prints id for attach/logs/stop/rm).
  - Non-interactive: `-p/--print`, `--output-format text|json|stream-json`,
    `--input-format text|stream-json`, `--include-partial-messages`,
    `--replay-user-messages`, `--include-hook-events`, `--json-schema <schema>` (structured
    output), `--max-budget-usd <amount>`, `--permission-prompts host|none`,
    `--forward-subagent-text`.
  - Access & safety: `--add-dir <directories...>`, `--allowedTools/--disallowedTools`,
    `--tools <tools...>` ("" disables all, "default" default set), `--permission-mode
    acceptEdits|auto|bypassPermissions|manual|dontAsk|plan`,
    `--dangerously-skip-permissions`, `--allow-dangerously-skip-permissions`, `--restricted`
    (removes code-running tools + WebFetch unless named; ignores user/project/local settings;
    confines file tools to working directories), `--safe-mode` (all customizations off for
    troubleshooting), `--bare` (minimal: no hooks/plugins/LSP/memory discovery; strict auth).
  - Extensibility: `--mcp-config <configs...>`, `--strict-mcp-config`, `--plugin-dir <path>`,
    `--plugin-url <url>`, `--settings <file-or-json>`, `--setting-sources user,project,local`,
    `--disable-slash-commands`.
  - Environment/UI: `--ide`, `--chrome/--no-chrome`, `--desktop`, `--worktree [name]`,
    `--tmux`, `--cloud [description|session_id|url]`, `--environment <id>`, `--remote-control
    [name]`, `--autocompact <auto|tokens>` (100k–1M), `--debug [filter]`, `--debug-file`,
    `--verbose`, `--ax-screen-reader`, `--prompt-suggestions`, `--file <file_id:path...>`.

## 3. Slash commands (grounded in repo CHANGELOG.md and README)

- Core session: `/help`, `/clear`, `/compact`, `/context`, `/cost`, `/status`, `/doctor`,
  `/config`, `/model`, `/effort`, `/fast`, `/usage`, `/usage-credits`, `/login`, `/logout`,
  `/exit` (CHANGELOG.md, multiple entries).
- History & state: `/resume`, `/rename`, `/fork`, `/rewind` (checkpoint restore; also
  `--rewind-files`), `/copy`, `/export`-class commands, `/bg`, `/tasks`, `/hooks`.
- Coding workflow: `/init` ("The model can now discover and invoke built-in slash commands like
  `/init`, `/review`, and `/security-review` via the Skill tool" — CHANGELOG), `/review`,
  `/code-review`, `/ultrareview`, `/security-review`, `/diff` (diff pane mod), `/add-dir`,
  `/install-github-app`, `/claude-api`, `/sandbox`.
- Extensibility & platform: `/plugin`, `/reload-plugins`, `/skills`, `/workflows`, `/artifacts`,
  `/mcp`, `/permissions`, `/auto-mode-setup`, `/output-style`, `/theme`, `/terminal-setup`,
  `/tui`, `/vim`-class keybinding commands, `/voice`, `/teleport`, `/remote-control`, `/loop`,
  `/schedule`, `/goal`, `/advisor`, `/btw` (side questions), `/feedback`, `/bug` (repo README:
  "Use the `/bug` command to report issues"), `/upgrade`, `/web-setup`, `/team-onboarding`,
  `/design`/`/design-sync` (CHANGELOG).
- Commands can be disabled wholesale with `--disable-slash-commands` (CLI --help); plugin-supplied
  commands load from plugin `commands/` directories (repo `plugins/README.md`).

## 4. Configuration (settings files, keys, precedence)

- **Settings sources** (repo `examples/settings/README.md`, linking the public settings docs):
  user `~/.claude/settings.json`, project `.claude/settings.json`, project-local
  `.claude/settings.local.json`, enterprise `managed-settings.json` (deployable via the MDM
  templates in `examples/mdm/`), plus `--settings <file-or-json>` and `--setting-sources` to
  control which of user/project/local load. The public docs define the precedence: enterprise
  managed policies > command-line arguments > `.claude/settings.local.json` >
  `.claude/settings.json` > `~/.claude/settings.json`.
- **Managed/enterprise keys observed in shipped examples**
  (`examples/settings/settings-strict.json`, `settings-lax.json`, `settings-bash-sandbox.json`,
  `settings-hipaa.json`, `managed-mcp-hipaa.json`):
  `permissions.{allow,ask,deny,disableBypassPermissionsMode}`,
  `allowManagedPermissionRulesOnly`, `allowManagedHooksOnly`, `allowManagedMcpServersOnly`,
  `allowManagedReadPathsOnly`, `allowManagedDomainsOnly`, `strictKnownMarketplaces`,
  `strictPluginOnlyCustomization`, `disableRemoteControl`, `disableSideloadFlags`,
  `forceLoginMethod`, `forceLoginOrgUUID`, `forceRemoteSettingsRefresh`, `cleanupPeriodDays`,
  `syncClaudeAiPlugins`, `syncClaudeAiSkills`, `skillOverrides`, `enableArtifact`,
  `autoMode` (+ `autoMode.allow` with `"$defaults"`, per CHANGELOG),
  `sandbox{enabled, autoAllowBashIfSandboxed, allowUnsandboxedCommands, excludedCommands,
  enableWeakerNestedSandbox, enableWeakerNetworkIsolation, network{allowedDomains,
  allowManagedDomainsOnly, allowLocalBinding, allowUnixSockets, allowAllUnixSockets,
  httpProxyPort, socksProxyPort}, classifyAllShell, failIfUnavailable}`,
  `allowedHttpHookUrls`, `allowedMcpServers`/`deniedMcpServers`, `allowedProviders`,
  `mcpServers`, `env`/`envVars` (e.g. `HTTP_PROXY`, `OTEL_*`,
  `CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY`, `DISABLE_ERROR_REPORTING`), `credentials`,
  `filesystem{denyWrite, …}`, `crossSessionInbound`.
- **Policy deployment:** Jamf/Iru/Intune/GPO templates ship in `examples/mdm/` (macOS
  `.mobileconfig`/`.plist`, Windows `ClaudeCode.admx` + `Set-ClaudeCodePolicy.ps1`) (repo tree).
- **Gateway deployment:** `examples/gateway/{aws,gcp}` Terraform + Dockerfiles run an enterprise
  auth/telemetry gateway; `claude gateway` runs it locally (repo `examples/gateway/`, CLI
  --help). Gateway admin concepts in CHANGELOG: spend limits, pooled group budgets, IdP-group
  cost reports, `policy_mode`, `guardrail.input_tagging`.
- **Environment variables** (CHANGELOG, >150 `CLAUDE_CODE_*`/`ANTHROPIC_*` identifiers):
  auth & endpoints (`ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_BASE_URL`,
  Bedrock/Vertex/Foundry variants, `ANTHROPIC_CUSTOM_HEADERS`, `ANTHROPIC_MODEL`,
  `ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU,FABLE}_MODEL`, `ANTHROPIC_SMALL_FAST_MODEL`),
  behavior toggles (`CLAUDE_CODE_DISABLE_*` family, `CLAUDE_CODE_SUBAGENT_EFFORT`,
  `CLAUDE_CODE_AUTO_MODE_SERVER`, `CLAUDE_CODE_ATTRIBUTION_HEADER`,
  `CLAUDE_CODE_AUTO_COMPACT_WINDOW`, `CLAUDE_CODE_API_KEY_HELPER_TTL_MS`, …).

## 5. Built-in tools (public names from `sdk-tools.d.ts`, shipped for SDK consumers)

- Core coding tools: `Bash`, `FileRead` (Read), `FileWrite` (Write), `FileEdit` (Edit),
  `Glob`, `Grep`, `NotebookEdit`, `TodoWrite`.
- Agent orchestration: `AgentInput` (the Task/subagent tool), `TaskCreate/TaskGet/TaskUpdate/
  TaskList`, `TaskStop`, `WorkflowInput`, `EnterPlanMode`/`ExitPlanMode`, `AskUserQuestion`,
  `SendFeedback`, `ReportFindings`.
- Web: `WebFetch`, `WebSearch`.
- MCP plumbing: `McpInput` (namespaced `mcp__<server>__<tool>` calls), `ListMcpResources`,
  `ReadMcpResource`, `ReadMcpResourceDir`, `RefreshMcpTools`.
- Platform/auxiliary: `Artifact`, `ClaudeDesign`, `Projects`, `CronCreate/CronDelete/CronList`,
  `ScheduleWakeup`, `RemoteTrigger`, `Monitor`, `PushNotification`, `EnterWorktree`/
  `ExitWorktree`, `ShowOnboardingRolePicker`, `OfferChromeSetup`, `ReadNotifications`,
  `ProposeSkills`, `ProposeGoal`.
- The exact JSON Schema of each tool's input is the shipped, public `sdk-tools.d.ts`
  (`ToolInputSchemas` union) — intentionally published so SDK hosts can validate tool calls
  (npm package `sdk-tools.d.ts` header).
- Tool availability can be curated per session: `--tools`, `--allowedTools`,
  `--disallowedTools`, `permissions.allow/ask/deny` rules with `Tool(specifier)` syntax such as
  `Bash(git *)` (CLI --help; `examples/settings/settings-strict.json`).

## 6. Permission modes

- `--permission-mode` choices (CLI --help): **acceptEdits, auto, bypassPermissions, manual,
  dontAsk, plan**.
- Semantics as publicly described: `plan` = read-only planning until a plan is approved
  (`ExitPlanMode` tool exists to leave plan mode); `acceptEdits` = auto-accept file edits;
  `bypassPermissions` = skip all checks (guarded by `permissions.disableBypassPermissionsMode`
  and `--dangerously-skip-permissions`, "Recommended only for sandboxes with no internet access");
  `auto` = model-driven approval with an `autoMode.allow` policy list and server-side safety
  checks (CHANGELOG auto-mode entries); `manual`/`dontAsk` = interactive prompting behaviors.
- Permission rules: `permissions.allow|ask|deny` lists with `Tool(specifier)` patterns; managed
  settings can forbid user/project rules (`allowManagedPermissionRulesOnly`) and hooks
  (`allowManagedHooksOnly`) (`examples/settings/settings-strict.json`).
- Sandbox: `sandbox` settings key applies to the Bash tool only — "The `sandbox` property only
  applies to the `Bash` tool; it does not apply to other tools (like Read, Write, WebSearch,
  WebFetch, MCPs), hooks, or internal commands" (`examples/settings/README.md`); network
  allowlist, unix-socket and local-binding controls inside `sandbox.network`.
- In headless mode, `--permission-prompts host|none` decides who answers prompts (SDK host /
  permission-prompt tool vs. auto-deny) (CLI --help).

## 7. Memory (CLAUDE.md) rules

- Project memory is loaded from `CLAUDE.md` / `.claude/CLAUDE.md` / `CLAUDE.local.md` up the
  directory tree; `/init` generates one (CHANGELOG `/init` entry; mods/agents-md README).
- `--bare` skips "CLAUDE.md auto-discovery" and auto-memory (CLI --help); `--add-dir` extends
  CLAUDE.md discovery to additional directories
  (`CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD`, CHANGELOG).
- **AGENTS.md support is a built-in mod** (`mods/agents-md`): modes
  `claude-md-or-agents-md` (default — AGENTS.md is read exactly where a project has no CLAUDE.md
  of its own), `claude-md-and-agents-md` (both), `managed-only` (drop user/project instruction
  files, keep the organization's), `claude-md` (engine default only). Nested AGENTS.md files
  attach on `Read` of files in those directories, mirroring CLAUDE.md placement (mods/agents-md
  README + hooks.json description).
- Auto-memory: the changelog references automatic memory features (e.g. `--bare` disabling
  "auto-memory"; memory compaction fixes) — Claude Code maintains memory beyond static CLAUDE.md
  files (CHANGELOG).
- Managed CLAUDE.md: organizations can push instruction content that the `sec-default` mod keeps
  out of reach of installed plugins (mods/sec-default/README.md).

## 8. Subagents, skills, plugins, hooks — format specs

- **Plugins** (repo `plugins/README.md`): standard layout —
  `.claude-plugin/plugin.json` (metadata), `commands/` (slash commands), `agents/`
  (specialized subagents), `skills/` (Agent Skills), `hooks/` (event handlers), `.mcp.json`
  (external MCP servers). Marketplaces are JSON catalogs (`.claude-plugin/marketplace.json`
  in-repo example lists plugins from `claude-plugins-official` with `git-subdir` sources).
  CLI management: `claude plugin enable/disable/configure/details/validate/eval/test`,
  `/plugin` in-session (CLI plugin --help; CHANGELOG).
- **Mods** (repo `mods/README.md`): "A mod is a Claude Code plugin whose behaviour lives in a
  hooks module: one `register(on, options)` entry that hooks the engine's events as functions
  `($, e, next)`." Four mods ship inside Claude Code: `sec-default` (organizational policy
  shield, seated outermost), `diff` (the `/diff` pane), `telemetry` (first-party analytics
  hooks), `agents-md` (AGENTS.md support). Each is a complete plugin with `.claude-plugin/
  plugin.json` + `hooks/hooks.json` naming module files.
- **Plugin function-hook events** (mods/types/claude-code.d.ts + mods sources): engine events
  such as `session.start`, `prompt.context`, `tool.call`, `process.run`, `command.register`,
  `$.telemetry` `log`/`mark`; hooks run chained across tiers (organization prepend tier → user
  tier → append tier) with `next(e)` / `next.to(e, "append")` / `{deny}` / `{refuse}` outcomes
  (mods/sec-default/README.md, mods/diff/hooks/hooks.json).
- **Plugin dev kit:** `/plugin-types` writes `claude-code.d.ts` TypeScript declarations
  ("Written by Claude Code 2.1.277… regenerate with that command"); `claude plugin validate`
  statically checks a hooks module; `claude plugin test` runs `*.test.ts` files in a sandboxed
  engine host (`import { describe, expect, mock, test, tier } from 'claude-code/testing'`) across
  surfaces (terminal/desktop/vscode/mobile) without fs/network/process
  (mods/types/claude-code.d.ts; mods/README.md).
- **Classic settings hooks** (`examples/hooks/bash_command_validator_example.py`): JSON in
  settings — `{"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command",
  "command": "python3 …"}]}]}}`; the example rewrites grep→rg and can deny with exit code 2.
- **Classic hook events** grounded in CHANGELOG: `PreToolUse`, `PostToolUse`,
  `UserPromptSubmit`, `Notification`, `Stop`, `SubagentStop`, `SubagentStart`, `SessionStart`,
  `SessionEnd`, `PreCompact`, `PermissionRequest`, `PermissionDenied`, `TeammateIdle`,
  `TaskCompleted`. SDK hooks gained `onFailure: "block"` in 2.1.317 (CHANGELOG).
- **Skills:** Agent Skills live in `~/.claude/skills/` and project `.claude/skills/` with
  `SKILL.md` metadata (name from frontmatter; folder-name mismatch tolerated — CHANGELOG);
  skills sync from claude.ai (`syncClaudeAiSkills`); `/skills` manages them; the Skill tool can
  invoke built-in commands (CHANGELOG). Built-in `plugin-authoring` skill exists (CHANGELOG).
- **Subagents:** defined via `--agents` JSON or agent files (`agents/` in plugins/projects);
  `--forward-subagent-text` forwards subagent text/thinking into parent stream; subagents
  inherit effort/model per `CLAUDE_CODE_SUBAGENT_EFFORT` rules; agent teams/teammates
  (`TeammateIdle`, inboxes, shutdown approvals) exist for multi-agent work (CLI --help;
  CHANGELOG).

## 9. MCP support

- **Management CLI:** `claude mcp add <name> <commandOrUrl> [args...]` (with `-e KEY=value` env
  and `--` subprocess flags), `claude mcp add --transport http <name> <url> [--header …]`,
  `claude mcp add-json <name> <json>` (stdio/SSE/HTTP/WebSocket), `claude mcp
  add-from-claude-desktop`, `claude mcp list|get` (with "⏸ Pending approval" state for
  unapproved `.mcp.json` servers and health checks), `claude mcp login/logout <name>` (OAuth for
  HTTP/SSE/claude.ai connectors) (CLI `claude mcp --help`).
- **Transports:** stdio, SSE, HTTP (streamable), WebSocket (CLI `mcp add-json` help text).
- **Config sources:** user/project/local settings `mcpServers` keys, project `.mcp.json`
  (requires approval per project), `--mcp-config <files-or-json>` with `--strict-mcp-config` to
  ignore everything else (CLI --help).
- **Runtime features:** MCP tools are namespaced (`mcp__server__tool` shape implied by
  `sdk-tools.d.ts` `McpInput`); resources exposed via `ListMcpResources`/`ReadMcpResource`
  tools; OAuth login; enterprise allow/deny lists (`allowedMcpServers`/`deniedMcpServers`,
  `allowManagedMcpServersOnly`); MCP server env sanitized (e.g. empty `AWS_BEARER_TOKEN_BEDROCK`
  unset rather than passed, CHANGELOG); elicitation/forms supported in cloud sessions
  (link-card and form-card entries, CHANGELOG).

## 10. Headless mode & Agent SDK

- `claude -p "prompt"` prints a response and exits; the workspace trust dialog is skipped in
  non-interactive mode ("Only use this in directories you trust" — CLI --help); invalid settings
  files are silently ignored in this mode.
- **Output formats:** `text` (default), `json` (single result), `stream-json` (realtime streaming)
  with `--include-partial-messages`, `--include-hook-events`, `--replay-user-messages`;
  **input** can be `stream-json` for realtime multi-turn driving (CLI --help).
- **Structured output:** `--json-schema` validates the final result; `--max-budget-usd` caps
  spend; `--no-session-persistence` for ephemeral runs (CLI --help).
- **SDK hosting:** `--permission-prompts host` + permission-prompt-tool integration; sdk
  `sdk-tools.d.ts` ships full input schemas; SDK control requests exist
  (`register_repo_root`, CHANGELOG); hooks callable from SDK with `onFailure: "block"`.
- Exit codes matter: `claude -p` exits 1 with an error when transcript/`~/.claude.json` writes
  fail (full disk/quota) rather than exiting 0 (CHANGELOG 2.1.317).
- Nested `claude -p` inside a cloud/self-hosted session reuses that session's ID/transcript
  unless `CLAUDE_CODE_DISABLE_NESTED_SESSION_ID=1` (CHANGELOG).

## 11. Checkpoints, background tasks, sessions

- **Checkpoints/rewind:** `/rewind` restores conversation and/or code state; `--rewind-files`
  restores files only; checkpoint backups are pruned and bounded on disk ("up to 79x transcript
  size reduction… bounded checkpoint disk usage", CHANGELOG).
- **Background sessions:** `claude --bg` starts detached work and prints an id; `claude agents`
  lists sessions; `claude attach <id|name>` re-attaches (supports partial-name match);
  `claude logs`, `claude stop|kill`, `claude rm`, `claude respawn` (restart on a new Claude Code
  version), `claude agents --json` for JSON listing (CLI --help).
- **Worktrees:** `--worktree [name]` creates a git worktree for the session; `--tmux` puts it in
  a tmux session (iTerm2 native panes when available); background sessions can carry worktrees
  (`claude rm` deletes "its worktree when that is safe") (CLI --help).
- **Session storage:** transcripts, tasks, file history and `~/.claude.json` entries per project;
  `claude purge` deletes them for one project; `cleanupPeriodDays` prunes old transcripts
  (CLI --help; examples/settings keys).

## 12. IDE integrations & surfaces

- **IDE connection:** `--ide` auto-connects when exactly one valid IDE is available;
  `CLAUDE_CODE_AUTO_CONNECT_IDE` env (CLI --help; CHANGELOG).
- **VS Code extension:** extensive CHANGELOG coverage (chat panel sign-in to gateways, effort
  slider, ultrathink rainbow, forked-conversation fixes, file links opening in editor groups).
- **Claude Desktop & cloud:** `--desktop` opens a session in the Claude Desktop app; `--cloud`
  creates/attaches cloud sessions (claude.ai/code URLs); self-hosted runners (`ccpool_…`
  environment ids) resume sessions across machines; `--chrome`/`--no-chrome` integrate Claude in
  Chrome; `--remote-control` drives a session remotely (CLI --help; CHANGELOG sections
  [VSCode]/[Cloud sessions]/[Remote Control]/Self-hosted runner).
- **GitHub:** tag `@claude` on GitHub (repo README); `/install-github-app` sets up the app.

## 13. Standout features (numbered)

1. **Three interfaces, one session model** — interactive TUI, `claude -p` headless, and SDK
   stream-json hosting share sessions, resumable by id/name (`--resume`, `--fork-session`).
2. **Layered settings + MDM** — enterprise `managed-settings.json` with managed-only hooks/rules/
   marketplaces, deployed via Jamf/Intune/GPO templates (examples/mdm, examples/settings).
3. **Permission lattice** — modes × allow/ask/deny rules × sandbox (Bash-only) × restricted mode,
   with fail-closed managed overrides.
4. **Plugin system with marketplace, function hooks, and tiers** — organization prepend tier
   outranks user-installed plugins (sec-default); plugins bundle commands/agents/skills/hooks/
   MCP servers (plugins/README.md, mods/).
5. **Mods as source-shipped built-ins** — diff/telemetry/agents-md/sec-default demonstrate that
   even core UX is plugin-shaped and testable with the shipped `claude-code/testing` kit
   (mods/README.md).
6. **Plugins get a real dev loop** — `/plugin-types` declarations, `plugin validate`, and
   `plugin eval`/`plugin test` harnesses with cross-surface UI testing (mods/types/claude-code.d.ts).
7. **Checkpoints & rewind** — conversation and file-state restore (`/rewind`, `--rewind-files`)
   with bounded disk usage.
8. **Background agents + worktrees + tmux** — parallel detached sessions with attach/logs/stop/
   rm/respawn lifecycle (CLI --help).
9. **Broad MCP surface** — four transports, OAuth login, project `.mcp.json` approval flow,
   resource tools, enterprise allow-lists.
10. **Agentic ergonomics** — auto-compact windows, `--exclude-dynamic-system-prompt-sections`
    for prompt-cache reuse, prompt suggestions, effort levels (low→max), model fallback chains.
11. **Cloud & fleet features** — cloud sessions, scheduled tasks (`/schedule`), self-hosted
    runners, remote control, `ultrareview` multi-agent code review, LLM gateway with spend
    governance.
12. **Accessibility** — `--ax-screen-reader` flat-text mode; screen-reader-aware fixes throughout
    the CHANGELOG.

## 14. Lessons for Marfim

1. **Adopt layered settings with managed overrides** (enterprise > args > local > project >
   user) and fail-closed `allowManaged*Only` keys so an org can pin behavior.
2. **Make every capability both interactive and headless** from day one (`-p` + stream-json +
   structured `--json-schema` output); exit codes must be honest.
3. **Ship plugin shapes as contracts**: manifest + commands/agents/skills/hooks dirs + `.mcp.json`,
   with a marketplace JSON schema and `validate`/`test` tooling — extension without core forks.
4. **Function hooks with tiers** (`($, e, next)` chain, org prepend > user > append) are a clean
   way to let policy and personality coexist; keep a `sec-default`-style shield mod.
5. **Memory is a layered document system** (CLAUDE.md hierarchy + auto-memory + AGENTS.md
   interop mod), not a single file; make discovery skippable (`--bare`) for scripting.
6. **Checkpoint file state, not just transcripts** — `/rewind`-style restore is what makes
   aggressive autonomous editing trustworthy.
7. **Curate the tool surface** (`--tools`, allow/deny specifiers like `Bash(git *)`) rather than
   only boolean permissions.
8. **Publish your tool schemas** (`sdk-tools.d.ts` analog) so SDK hosts and auditors can validate
   agent actions without reading proprietary code.
9. **Namespaced MCP + approval states** ("⏸ Pending approval") make third-party tools governable
   at project scope.
10. **Operator lifecycle commands** (`doctor`, `purge`, `update`, `import` from other agents,
    `setup-token`) reduce adoption friction — Marfim should have equivalents from v1.

---

## Delta scan: 964d695 → 0ed9469 / npm 2.1.317 → 2.1.319 (2026-11-06, latest build)

Computed from the artifacts by `scripts/analyze_specs.py`; raw evidence in
`upstream/analysis-evidence.json` (workspace-side). All observations remain
metadata/interface facts only; no proprietary code was extracted.

**Repo deltas:** 4 new commits, 18 files changed, +806/−0 (`git diff
--shortstat`). CHANGELOG gains **2.1.318** (84 entries: 5 Added / 8 Changed /
41 Fixed / 29 Other / 1 Removed; Shannon entropy over the entry-type
distribution H = 1.6762 bits) and **2.1.319** (146 entries: 15 / 16 / 76 /
38 / 1; H = 1.7318 bits; combined H = 1.7205 bits).

**Mods: 4 → 5.** The new fifth mod is **`privacy`** (10 files, 234 LOC,
shipped with its own README spec): a `tool.check` function hook that refuses
— with no prompt, whatever the permission mode — a Bash / Monitor /
PowerShell call whose input BOTH holds the signed-in account's email address
(as written, quote-stripped, case-insensitive, one-pass percent-encoded) AND
names a User-Agent (the `user[-_]agent` word family, `--user-agent` flag
prefixes, `UA:`/`UA=`, or the client-flag pairs curl/curlie `-A`, wget/wget2
`-U`, `-User` for web clients, unicode dashes included). Two word matches
over the whole input, "no parser, on purpose"; false refusals accepted; a
deny beneath keeps its own reason; disable channels catalogued; an explicit
bypass list (computed values, variables, web/MCP tools, other encodings —
"a courtesy, not a boundary"). Full clean-room distillation: §CC ADOPTED in
Marfim as `core::privacy::PrivacyGuard` + `accountEmail`.

**`$.session.identity()`** (mods/types `claude-code.d.ts`, +81 lines):
managed-settings-gated identity op — `SessionIdentity { kind, organizationId?,
organizationType?, email?, isVerified }` or a `{ deny }` union arm; org/email
fields only where managed `forceLoginOrgUUID` pins the org; `isVerified`
distinguishes the server-confirmed credential from the on-disk (editable)
login record; forward-compat rule: "read an unknown kind as no identity".
Also `session.identity` added to the event-argument maps. QUEUED in Marfim
(no managed-settings tier yet).

**CLI surface (fresh capture from the shipped linux-x64 binary).** Binary:
340,226,576 bytes, sha256 `dabafec8…`, `claude --version` → `2.1.319
(Claude Code)`. Help: 66 flags, 18 subcommands (same list as §2). NEW vs the
dossier: `--brief` ("Enable SendUserMessage tool for agent-to-user
communication") and `--remote-control-session-name-prefix <prefix>`.
Semantics enriched: `--system-prompt-snapshot` now documents the default
(`on` except under `--bare` outside cloud/background) and the
record-verbatim-until-compaction rule; `--bare` sets `CLAUDE_CODE_SIMPLE=1`,
keeps skills resolvable, and locks auth to `ANTHROPIC_API_KEY`/apiKeyHelper;
`--restricted` now refuses bypassPermissions and gates writes to settings,
git and tool-configuration files behind a person; `--fallback-model` accepts
a comma-separated list and re-tries the primary each user turn; `--plugin-dir`
accepts `.zip` archives and repeats; `--bg --resume <id>` continues that
session in the background under the same ID.

**sdk-tools.d.ts (npm interface facts).** 4,234 lines, 90 exported
declarations, 45 `ToolInputSchemas` union members; sha256
`2565f41f…`. Diff vs 2.1.317: exactly two atoms — `next_cursor` documentation
tightened to "passed back unchanged; not an offset, a count or an event
number", and a notification field `status: "proactive"` became optional
(`status?: "proactive"`). The tool-name surface itself is UNCHANGED.

**2.1.318/319 operational waves relevant to harness design** (full ledger in
§CC): config-file integrity (`~/.claude.json` invalid → fail with restore
instructions, never silently replaced; backup-rotation fixes on network
homes), MCP legacy-handshake pinning (probe-death <3s → legacy handshake for
7 days), config-write locking (`mcp add/remove` wait for the lock or exit 1),
login-expiry surfacing (`loginExpiresAt`, `credential_expires_at`, 10-day
stderr warning), idle-compaction summary integrity, permission-prompt input
race, auto-mode server semantics + exit-code honesty (`claude -p` exits 1
when refused with no safety check), quoted-word Bash deny/ask matching fix,
MCP task backgrounding, `permissions.denyInsteadOfPrompting`, spend-report
windows, gateway pooled-budget rules, `/tasks` finished-subagent reopen.
