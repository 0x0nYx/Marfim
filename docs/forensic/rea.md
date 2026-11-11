# Forensic Analysis: REA — Reverse Engineer Anything (github.com/morluto/rea)

- **Analyzed commit:** `9ebbe67e05af08b567e215340da993c08dae378f`
  (2026-11-03, "fix(ci): await website worker tests and format Wrangler config"), branch `main`.
- **Package version at this commit:** `rea-agents` **6.8.0** (`package.json`).
- **License:** MIT (root `LICENSE`); third-party notices in `THIRD_PARTY_NOTICES.md` and `licenses/`.
- **Identity:** "REA: Reverse Engineer Anything — One MCP for reverse engineering across binaries,
  applications, and runtime behavior" (`README.md`).
- npm package `rea-agents`; MCP registry name `io.github.morluto/rea`
  (`package.json#mcpName`, `server.json`); website `rea.tools`; docs catalog badge links
  `docs/mcp-contracts.md#generated-catalog`.
- The "~256k stars" figure comes from the task brief and is not verifiable inside the clone;
  the repo self-references a trendshift badge (#82054) and a Discord community.
- **Product shape:** one npm package that is simultaneously
  (a) a CLI (`rea` and `rea-agents` bins → `scripts/rea.mjs`) and
  (b) a local MCP server over stdio (`rea mcp` / `rea --mcp`) or opt-in streamable HTTP
  (`rea mcp --http`). "Analysis runs locally" (`README.md`, FAQ "Does REA upload my app?").
- **Scale observed in this clone:**
  109 entries under `docs/`;
  116 files in `src/server/`, of which **73 are `register*.ts` MCP tool-registration modules**;
  45+ `*_TOOL_CONTRACTS` arrays in `src/contracts/`;
  **180 CLI commands, 234 MCP tools, 6 MCP prompts** — authoritative counts read live from
  `rea doctor --json` → `identity.catalog.counts` in this sandbox, cross-checked against a live
  MCP `tools/list` which returned exactly 234 tools (`analysis/evidence/doctor.json`,
  `analysis/evidence/rea-mcp-tool-names.json`).
- **Node requirement:** `^22.23.4 || ^24.22.1 || >=26.12.1` (`package.json#engines`);
  `packageManager: npm@11.16.0`; `type: module`; package `files` list ships `dist/`, the bridges,
  `skills/`, plugin manifests, `DISCLAIMER.md`, and a prebuilt Windows native addon.

## 1. Identity & Purpose

- REA gives an AI agent (or a human at a terminal) tools that produce **evidence** about how
  shipped software — native binaries, Electron/JavaScript apps, .NET assemblies, mobile packages,
  firmware, websites, network captures — implements a feature, so the agent can explain it and
  build a clean-room equivalent (`README.md` intro).
- The README pitch: "See a feature you like. Understand how it works, down to the binary level."
- The prescribed user prompt (`README.md` "Ask your agent"):
  *"Understand how search works in the Notes app, show me the evidence, and build a similar
  feature for my project."*
- Your agent calls REA through MCP; "REA returns findings with their evidence. The agent uses
  them to ask follow-up questions, explain the behavior, or write and test an implementation.
  CLI commands use the same workflows." (`README.md` "How REA works").
- Worked showcases anchor the method (`README.md` "Showcases"):
  - **DX-Ball** — reconstructed sound-pan calculation passes 3,205 original-x86 cases and
    reproduces all 63 compiled function bytes.
  - **Notion** — Electron clipboard bridge traced renderer → preload → IPC → main process.
  - **TH04** — DOS/PC-98 bullet-ring calculation recovered and compared against the historical
    compiler output.
- Extra surface: the same recorded interactions can be projected into **AgentEnv bundles** —
  replayable RL/eval environments built from HAR/browser captures
  (`README.md` "Build RL environments"; `docs/agent-environments.md`).

## 2. Architecture

- **Layering** (mandated by `AGENTS.md` "Architecture and Cleanup"; diagram
  `docs/architecture.mermaid`): domain semantics and contracts → provider adapters → shared
  application workflows → CLI/MCP adapters. "Dependencies flow inward"; engine protocols and
  provider-specific code stay out of domain/application layers.
- **Key runtime dependencies** (`package.json#dependencies`):
  - `@modelcontextprotocol/server` + `@modelcontextprotocol/client` — MCP SDK (2.3.1 per doctor).
  - `@babel/parser` + `@babel/types` — JavaScript AST for static JS/Electron analysis.
  - `@electron/asar` — ASAR unpacking.
  - `playwright-core` — browser scenarios; `@lydell/node-pty` + `@xterm/headless` — process capture.
  - `frida` (optionalDependency) — runtime instrumentation.
  - `openpgp`, `isomorphic-git`, `plist`, `@zip.js/zip.js`, `ajv`+`ajv-formats`, `zod`,
    `pino`, `evmole`, `cborg`, `@msgpack/msgpack`, `@clack/prompts` (setup UI),
    `incur` (CLI framework — `scripts/rea.mjs` comment: "Incur owns registration helpers
    such as `mcp add`").
- **Entry routing** (`scripts/rea.mjs`):
  - `mcp` / `--mcp` → `dist/mcp/main.js` (stdio JSON-RPC MCP server).
  - `mcp --http` → streamable HTTP transport.
  - MCP-doctor flag shapes → `dist/cli-runtime/mcpDoctor.js`.
  - everything else → Incur CLI runtime (`dist/cli-runtime/cliBuiltinArguments.js` when built).
  - SIGPIPE (exit 141) and stdout-closed backpressure handled explicitly.
- **Bridges to native engines** (`bridge/`):
  - `hopper_bridge.py` — Hopper (macOS) analysis bridge.
  - `cutter/rea_bridge.py` — Cutter/Rizin plugin bridge.
  - `ghidra/` — 11 Java extension classes (`ReaGhidraBridge.java`, `ReaGhidraBundleActivator.java`,
    `ReaGhidraApplySeeds.java`, MIPS soft-float fix, RISC-V calling convention, DOS/DOL/COM/raw
    preparers).
  - `android/ReaJadxBridge.java` — JADX headless bridge.
  - `native/rea_lldb_tracer.py` + `NativeUI*.swift`, `ReaNativeUI.swift` — LLDB call tracing and
    macOS accessibility-tree/screenshot native UI readers.
  - `process/ProcessRunTokenReader*.swift` — process run-token capture.
  - `mitmproxy/capture.py` — network capture addon; `pwntools/`, `pwndbg/` launcher scripts;
    `nativeaot/*.java` — .NET NativeAOT metadata recovery.
  - `native/windows/build/rea-windows-x64.node` — prebuilt Windows native addon.
- **Engineering/QA machinery** (`package.json` scripts):
  - Turborepo pipelines; vitest projects split by layer (`boundary`, `cli-boundary`,
    `process-boundary`, `mcp-boundary`, `acceptance`, `conformance`, `evaluation`).
  - **Stryker mutation testing** dedicated to reverse-tools semantics
    (`stryker.config.mjs`, `stryker.semantics.config.mjs`).
  - knip (dead exports), jscpd (duplicates), oxlint/oxfmt, `verify:architecture-guards`,
    `verify:module-boundaries`.
  - ~80 `verify:*` scripts exercising real providers: Ghidra per-ISA (MIPS, PPC, RISC-V, 6502,
    m68k, DOS, DOL, Switch, Windows, NativeAOT), IDA, Hopper (+Linux), Cutter, Frida, Electron,
    browser (DOM/network/scripts/source maps), ADB, APK signature, apktool, JEB, firmware
    (UF2/ESP/devicetree), pcap/TShark/qlog/HTTP3, sigrok, WASM/WABT, EVM, IL2CPP, Go, Rust,
    Bun standalone, Node SEA, SQLite, Windows native, Node.js install checks.
  - Generated artifacts (never hand-edited, `AGENTS.md`): `docs/public/product-catalog.json`,
    skill metadata, completion ledger, error schema (`docs/error-contract.schema.json`,
    `docs/user-visible-errors.csv`).
- **Engine protocol** (`docs/mcp-contracts.md`): pinned stdio client budget of 10 MiB per reply
  (text + structured + envelope); results that cannot fit return `resource_constraint`
  (`details.resource: "transport"`) with a same-session Evidence reference instead of truncating
  silently. Output-schema and input-schema profiles let constrained clients shrink `tools/list`.

## 3. MCP Tool Catalog

- **Single source of truth**: `ToolContract` in `src/contracts/toolContractTypes.ts` declares
  `name`, `title`, `description`, `kind` (adapter family), `requiresActiveTarget`,
  `retainedSummary`, `inputSchema` (strict), `outputSchema`, `effects`, `annotations`, `examples`.
- Contract arrays live in `src/contracts/*ToolContracts.ts` (45+ arrays); registration glue in
  `src/server/register*.ts` (73 files), all wired in `src/server/createServer.ts` via
  `EvidenceMcpServer` (wrapping the MCP SDK server) with per-family admission and
  catalog identity digests (`src/catalogIdentity.ts`).
- `scripts/generate-mcp-tool-catalog.mjs` regenerates the published catalog; `rea doctor` reports
  its SHA-256 so a client can pin the exact surface it was built against.
- **Live counts at this commit (verified in this run):** 234 MCP tools, 6 prompts, 180 CLI
  commands; `binary_session` groups availability into 17 families
  (`analysis/evidence/mcp-calls-raw.txt`).
- Adapter kinds (`TOOL_KINDS`, `src/contracts/toolContractTypes.ts`):
  `official-proxy, enhanced, native-provider, artifact-provider, managed-provider,
  android-provider, apktool-provider, flutter-provider, jeb-provider, adb-provider,
  firmware-provider, browser-provider, electron-provider, runtime-provider, pcap-provider,
  application, session`.

### Family table (registration file → purpose → representative tools)

| # | Family / source | Purpose | Representative tools |
|---|---|---|---|
| 1 | official-proxy — `registerOfficialTools.ts`, `src/contracts/officialToolContracts.ts` | Provider-neutral bridge ops over Hopper/Ghidra/IDA/Kuna | `list_procedures`, `search_procedures`, `list_strings`, `search_strings`, `list_names`, `list_segments`, `list_architectures`, `procedure_info`, `procedure_pseudo_code`, `procedure_assembly`, `procedure_callers`, `procedure_callees`, `procedure_references`, `read_function_instructions`, `address_name`, `address_to_file_offset`, `xrefs`, `get_navigation_context`, `goto_address`, `next_address`, `prev_address`, `comment`, `set_comment`, `inline_comment`, `set_inline_comment`, `set_address_name`, `set_addresses_names`, `set_bookmark`, `unset_bookmark`, `list_bookmarks`, `list_documents`, `current_address`, `current_procedure`, `current_document`, `annotate_native_function`, `inspect_native_load_image`, `inspect_native_instruction`, `inspect_native_data_type`, `resolve_native_call_targets`, `search_bytes`, `read_bytes` |
| 2 | enhanced / function workflow — `registerReverseEngineeringTools.ts`, `registerEnhancedTools.ts`, `src/contracts/functionWorkflowToolContracts.ts` | Composed dossiers, graphs, tracing | `analyze_function` (summary-first dossier; `detail`), `batch_decompile`, `decompile_all`, `build_call_path`, `get_call_graph`, `trace_call_path`, `trace_feature`, `compare_functions`, `match_native_functions`, `trace_native_values`, `inspect_native_api`, `demangle_swift`, `find_xrefs_to_name`, `known_function`, `list_unknowns` |
| 3 | session — `registerSessionTools.ts`, `registerAnalysisSettingsTools.ts`, `registerAnalysisViewTool.ts`, `registerCloseLifecycleTool.ts` | Target lifecycle, evidence access, settings | `binary_session` (discovery + `tool_availability`), `open_binary`, `close_binary` (snapshot retention), `inspect_analysis_view` (paged retained views), `get_evidence_bundle`, `export_evidence_bundle`, `import_evidence_bundle`, `update_analysis_settings`, `inspect_analysis_settings`, `guided_workflows` |
| 4 | artifact-provider — `registerArtifactTools.ts`, `registerArtifactComparisonTool.ts`, `registerBundleComparisonTool.ts` | Packages, archives, Apple bundles | `inspect_artifact`, `extract_artifact`, `compare_artifacts`, `compare_bundles`, `open_bundle_resources`, `inspect_plist`, `inspect_signature`, `decode_interface_builder`, `inspect_asset_catalog`, `inspect_keyed_archive`, `intent_filters` |
| 5 | managed-provider — `registerManagedTools.ts`, `registerManagedWorkflowTools.ts` | .NET PE/CLI static analysis | `inspect_managed_artifact`, `inspect_managed_members`, `inspect_managed_native_boundaries`, `verify_managed_native_boundaries`, `compare_managed_members`, `import_managed_reconstruction`, `project_managed_application_graph` |
| 6 | android/apktool/adb/flutter/jeb — `registerAndroidTools.ts`, `registerApktoolTools.ts`, `registerAdbTools.ts`, `registerFlutterTools.ts`, `registerJebTools.ts` | Mobile targets | `inspect_android_package`, `inspect_android_class`, `inspect_android_method`, `inspect_android_jni_bindings`, `search_android_classes`, `trace_android_references`, `project_android_application_graph`, `inspect_apk_signature`, `decode_android_resources`, `identify_flutter_build`, `list_adb_devices`, `list_adb_packages`, `install_adb_package`, `pull_adb_file`, `push_adb_file`, `read_adb_logcat`, `capture_adb_screen`, `collect_adb_bugreport`, JEB client/unit tools |
| 7 | browser/web — `registerBrowserTools.ts`, `registerBrowserScenarioTool.ts`, `registerWebScriptTool.ts`, `registerWebModuleTool.ts`, `registerWebSourceLocationTool.ts`, `registerWebRuntimeTools.ts`, `registerWebNetworkCaptureTool.ts` | Websites & captures | `list_browser_targets`, `inspect_web_page`, `capture_web_screenshot`, `capture_browser_scenario`, `validate_browser_scenario`, `export_web_scripts`, `trace_web_module_imports`, `trace_web_source_location`, `inspect_web_event_listeners`, `observe_web_session`, `observe_web_execution`, `inspect_web_network_capture`, `compare_web_captures`, `compare_web_screenshots`, `discover_webmcp_tools` |
| 8 | electron-provider — `registerElectronTools.ts`, `src/contracts/javascript/electronToolContracts.ts` | Electron apps | `list_electron_targets`, `inspect_electron_fuses`, `capture_electron_scenario`, `inspect_electron_page` |
| 9 | JavaScript application — `registerApplicationTools.ts`, `registerJavaScriptRecoveryTool.ts`, `registerJavaScriptRuntimeObservationTools.ts` | Static JS/ASAR | `analyze_javascript_application`, `analyze_web_bundle`, `recover_javascript_sources`, `trace_application_feature`, `compare_application_versions`, `compare_javascript_export_shapes`, `reconcile_javascript_runtime`, `compare_source_to_bundle`, `import_reference_source`, `list_javascript_runtime_targets`, `observe_javascript_runtime` |
| 10 | runtime-provider — Frida/GDB/Rizin/Cutter/process files | Execution-time observation | `capture_process`, `compare_process_captures`, `correlate_static_and_runtime`, `instrument_with_frida`, `list_frida_devices`, `list_frida_processes`, `start/close_frida_session`, `load/unload_frida_script`, `gdb_console`, `debug_with_rizin`, `execute_rizin_command`, `cutter_command`, `observe_native_calls`, `observe_native_ui`, `capture_native_ui_scenario`, `trace_native_ui_action`, `trace_native_values`, `trace_dylib_resolution`, `get_objc_classes`, `get_objc_protocols`, `inspect_native_dispatch_metadata` |
| 11 | pcap-provider — `registerPcapTools.ts`, `registerCaptureTools.ts`, `registerSignalTools.ts` | Recordings | `inspect_packet_fields`, `inspect_packet_capture`, `inspect_pcap_capture`, `inspect_pcap_client`, `inspect_signal_decoders`, `decode_signal_recording`, `inspect_web_network_capture` variants (HAR/SAZ/mitmproxy/qlog) |
| 12 | target-free static inspectors — `registerFirmwareTools.ts`, `registerDevicetreeTool.ts`, `registerKernelModuleTool.ts`, `registerGpuCodeObjectsTools.ts`, `registerModelArtifactTools.ts`, `registerGoTools.ts`, `registerRustTools.ts`, `registerPythonPycTool.ts`, `registerErlangTools.ts`, `registerWasmTools.ts`, `registerEvmTools.ts`, `registerSqliteTools.ts`, `registerIl2cppTools.ts`, `registerBinaryDiagnosticsTools.ts` | No-engine metadata recovery | `inspect_firmware_regions`, `extract_firmware`, `inspect_devicetree`, `inspect_kernel_module`, `inspect_gpu_code_objects`, `inspect_model_artifact`, `inspect_go_binary`, `inspect_rust_binary`, `inspect_python_pyc`, `inspect_erlang_beam`, `inspect_wasm_artifact`, `inspect_evm_interface`, `inspect_sqlite_database`, `inspect_il2cpp_metadata`, `inspect_binary_layout`, `inspect_byte_entropy`, `inspect_elf_notes`, `inspect_pe_imports`, `inspect_pe_certificates`, `inspect_pe_load_config`, `inspect_pe_resources`, `inspect_native_exports`, `inspect_apple_ips`, `inspect_recorded_crash`, `inspect_dart_aot`, `inspect_desktop_application`, `inspect_with_objdump` |
| 13 | investigation bookkeeping — `registerInvestigationTools.ts`, `registerGuidanceTools.ts`, `registerGuidedWorkflowTool.ts`, `registerSessionRecordTools.ts`, `registerSessionStatusTool.ts`, `registerResetInvestigationTool.ts` | Unknowns ledger & guidance | `record_unknown`, `update_unknown`, `verify_unknown_resolution`, `search_guidance`, `read_guidance_section`, `guided_workflows`, `reset_investigation`, session status |
| 14 | reconstruction — `docs/reconstruction-obligation-ledgers.md`, `docs/reconstruction-readiness.md` | Clean-room closure | `build_reconstruction_obligation_ledger`, `evaluate_reconstruction_coverage`, `verify_reconstruction`, `project_*_application_graph` |

- The complete 234-name list is saved at `analysis/evidence/rea-mcp-tool-names.json`.
- Prompts: 6 registered via `registerPrompts.ts` / `registerGuidedPrompts`
  (`src/contracts/promptContracts.ts`); CLI counterpart `rea guided-workflows`.

## 4. Evidence → Clean-Room Workflow (the prescribed process)

- **Canonical Evidence record.** "Evidence-producing tools return the complete canonical Evidence
  record in both text and structured content. Read `structuredContent.normalized_result` for the
  operation result and `structuredContent.evidence_id` for its identity. `raw_result` holds a
  provider representation distinct from that result… `analysis_profile` is always present"
  (`docs/mcp-contracts.md` "Tool results").
- Fields observed in this run's records: `evidence_id` (`ev_<64hex>`), `subject` (name, SHA-256,
  local path, format), `provider` (id/name/version), `predicate_type: "rea.analysis"`,
  `operation`, `parameters`, `raw_result`, `normalized_result`, `confidence: "observed"`,
  `authority: "shipped-artifact"`, `limitations[]`, `relationship_coverage`, `locations[]`,
  `evidence_links[]`.
- **Workflow steps as the skill prescribes** (`.agents/skills/reverse-engineer-anything/SKILL.md`,
  version 59, shipped to agents at setup):
  1. "Connect only when needed" — use the connected server's actual `tools/list`; do not run
     diagnostics before every investigation; distinct recovery paths for blocked calls, dropped
     connections, and `cleanup_incomplete`.
  2. "Route the target first" — pick the entry tool from the target type (ASAR →
     `analyze_javascript_application`; archive → `open_binary`+`inspect_artifact`; APK →
     `inspect_android_package`; native → `open_binary`; page → `list_browser_targets`; …).
  3. "Work summary-first" — call with `detail: "summary"` for large/unknown-size inputs, page the
     retained parent with `inspect_analysis_view`; `analyze_function` is summary-first by default;
     pass retained dossiers by reference to comparison tools.
  4. "Plan broader investigations" — a staged 7-step method: turn the request into a checklist of
     questions and the evidence each answer needs; inspect session/artifact state and reuse it;
     start with the smallest useful overview; inspect packaged resources alongside code;
     corroborate with the evidence type the conclusion requires (runtime only when static cannot
     answer); decompose into independent questions for parallel workers; keep a finding ledger
     linking each conclusion to Evidence IDs, confidence/evidence type, search boundary, and
     remaining unknowns; reconcile the checklist before declaring completion.
  5. Closure — "Every conclusion must distinguish observations, inferences, and unknowns. Cite
     Evidence IDs, preserve limitations and incomplete coverage, and never imply that static
     analysis observed execution."
- **Retention & paging.** `analyze_function` without `detail` retains the complete dossier in the
  session and returns a summary whose `normalized_result.parent_evidence_id` names it;
  `inspect_analysis_view` pages it by section/collection with `offset`/`limit`
  (`docs/mcp-contracts.md` "Retained application Evidence inputs").
- **Comparison.** `compare_artifacts`/`compare_functions` accept three source forms:
  `{"kind":"retained-evidence","evidence_id":…}`, `{"kind":"evidence-file","path":…}`, or inline
  Evidence; unresolvable references fail with `evidence_integrity_mismatch` rather than being
  silently re-read (`docs/mcp-contracts.md`).
- **Unknowns ledger.** Explicit unknown records (`record_unknown`, `update_unknown`,
  `verify_unknown_resolution`, `list_unknowns`) keep unresolved questions first-class instead of
  letting them evaporate between turns.
- **Obligation ledgers.** `build_reconstruction_obligation_ledger` (MCP tool + CLI
  `rea build-reconstruction-obligation-ledger`) turns authenticated Evidence into a deterministic
  list of reconstruction claims. "A required obligation closes only when one manifest binding
  supplies a unique owner, any required parser/schema/domain type, every required case fixture,
  and a passing verifier whose authority is comparable to the original observation."
  Static graph facts create *candidate* obligations only; "original process authority requires
  observed `controlled-replay` Evidence" (`docs/reconstruction-obligation-ledgers.md`).
- **Readiness.** `npm run verify:readiness` runs public static-analysis journeys plus a synthetic
  nine-stage evaluator; the two are kept in separate report sections and are explicitly never
  fused into a single "reconstruction-ready" verdict (`docs/reconstruction-readiness.md`).
- **Portability.** `export_evidence_bundle` streams canonical JSON into an atomically published
  file; `import_evidence_bundle` validates envelopes and rejects unsupported fields;
  `rea inspect-evidence-bundle` and `rea compare` consume saved bundles offline.

## 5. Setup / Integration mechanics

- Primary path: `npx rea-agents setup` — "Choose your agents, review the proposed changes, and
  approve them. Setup adds REA's MCP server and matching workflow instructions, with backups of
  existing configuration. Restart your agent afterward." (`README.md` "Quick start").
- Setup "records the selected runtime and REA entry script directly"; package-runner users should
  keep the npx cache entry; run acquisition "from a trusted directory outside the target project".
- Supported clients (`docs/installation.md` "Supported agents") — 26 `--client` values:
  `claude_code`, `claude_desktop`, `codex`, `cursor`, `gemini_cli`, `windsurf`, `devin`,
  `opencode`, `antigravity`, `copilot_cli`, `commandcode`, `qwen_code`, `kimi`, `vscode`,
  `grok_build`, `omp`, `pi`, `hermes`, `qoder`, `droid`, `trae`, `workbuddy`, `zcode`, `dsh`,
  `freebuff` (≥6.8.0). Others use manual stdio registration.
- Health check and lifecycle: `rea doctor [--client <id>] [--json]` (readiness, provider
  inspections, skill version, catalog digests); `rea uninstall` removes REA-owned config and
  skill files; `rea update` (and `--update`) is a verified self-update with a 4 MiB output cap,
  followed by re-running setup to refresh registrations.
- **Claude Code / Codex plugin route** (`.claude-plugin/plugin.json`, `.codex-plugin/plugin.json`,
  `.codex-plugin/mcp.json`): the plugin bundles `skills/` and registers its own `rea` MCP server
  via `node ${CLAUDE_PLUGIN_ROOT}/scripts/plugin-mcp-launcher.mjs 6.8.0`, `startup_timeout_sec:
  30`. The skill is explicit that plugin installs live outside `rea setup`/`rea doctor` state and
  need their own recovery path (SKILL.md "Connect only when needed").
- **MCP registry metadata** (`server.json`, schema `2025-12-11`): npm package `rea-agents`,
  transport stdio, `packageArguments: [{"type":"positional","value":"mcp"}]`, runtime hint `npx`
  — the canonical registration is `npx rea-agents mcp`.
- **Context-budget controls** (README + `docs/mcp-contracts.md`):
  - Compact input schemas by default; `REA_MCP_INPUT_SCHEMA_PROFILE=full` restores complete
    field guidance. Both keep canonical server-side validation.
  - Output schemas advertised by default; `REA_MCP_OUTPUT_SCHEMA_PROFILE=compact|off` bounds or
    omits them for clients that cap the `tools/list` frame.
  - Per-family advertising via tool-family env (`src/config/mcpToolFamilies.ts`,
    `src/server/createServer.ts`); server instructions adapt so they never recommend a disabled
    tool (`src/server/serverInstructions.ts`).
- **Providers are bring-your-own.** Setup never installs or upgrades Node.js, npm, Homebrew,
  Java, Ghidra, IDA, JADX, Kuna, Binwalk, or Unblob (SKILL.md; `AGENTS.md`). Hopper installation
  is offered behind explicit approval only (macOS 12+, or x86_64 Ubuntu 24.04+/Fedora 41+/Arch
  family); Ghidra is the recommended fallback elsewhere (`docs/installation.md` "Hopper").

## 6. Guardrails & ethics

- **DISCLAIMER.md** (root; shipped in the npm package): REA is built for lawful analysis —
  security research, malware and incident analysis, vulnerability research and coordinated
  disclosure, CTFs and education, interoperability, software preservation, debugging/auditing
  software you own or are authorized to examine.
- Refused uses (issues closed without response): circumventing DRM, license checks, anti-cheat or
  other technical protection measures; game cheats or tampering with online services; bypassing
  authentication/bot protection/access controls; **"copying proprietary software or reproducing
  its protected code and assets"**; evading AI model provider policies; reselling/sharing AI
  accounts.
- User responsibility: "Public availability of software, possession of a file, or access to a
  debugging endpoint does not by itself establish permission for every operation." Runtime,
  network, and device operations can affect state; agents' plans and scripts must be reviewed;
  untrusted targets belong in an appropriate environment.
- Results honesty: "Decompilation, reconstruction, heuristics, and AI-assisted analysis may be
  incomplete or incorrect… Obtaining source-like output or an analysis report does not grant
  rights to the original program, its data, or the generated output." — the core clean-room
  posture, stated in-product.
- Commercial tools: bridges grant no license entitlement; cracked copies, stolen keys, and
  license-restriction bypass are unauthorized (DISCLAIMER.md "Third-party tools and licenses").
- Non-endorsement and distribution-channel clauses cover every version/platform of REA.
- **In-product guardrails:**
  - Every tool contract declares `effects` and derived `annotations`
    (`src/contracts/toolEffects.ts`, `toolContractTypes.ts`) — read vs. mutate vs. execute.
  - `UNTRUSTED_CONTENT_INSTRUCTIONS` are appended to the server instructions so agents treat
    target-derived text as untrusted data (`src/server/toolResultEncoding.ts`, used by
    `serverInstructions.ts`).
  - Read-only provider stances are codified: "IDA analysis is read-only; never save or close an
    attached GUI database"; Ghidra uses ephemeral databases that never modify executable bytes
    (`AGENTS.md`).
  - Fail-closed integration rule: capabilities with unresolved material scope, authority,
    side-effect, or verification questions do not ship (`AGENTS.md` "Product and Authority").
  - Redaction: transport credentials and explicitly marked sensitive values are redacted;
    secrecy is never inferred from names or patterns (`AGENTS.md`).
  - No per-call permission system of its own: "REA does not require permission grants or
    per-call approval flags. The agent client can independently require approval" (SKILL.md).

## 7. Standout Features

1. **Contract-first catalog** — one `ToolContract` declaration feeds MCP schemas, CLI arguments,
   generated docs, doctor digests, and conformance tests (`src/contracts/toolContractTypes.ts`,
   `scripts/generate-mcp-tool-catalog.mjs`).
2. **Retained-Evidence delivery** — oversized results are retained and paged
   (`inspect_analysis_view`) with explicit `resource_constraint`/`evidence_reference` semantics
   and a documented 10 MiB stdio budget (`docs/mcp-contracts.md`).
3. **Coverage honesty as data** — `coverage.scope`/`coverage.status` on inventories,
   `facet_availability` on function dossiers, first-class unknown records; absence of evidence is
   never reported as evidence of absence.
4. **Cross-surface identity** — the same operation yields matching Evidence identity via CLI and
   MCP, checked in CI (`docs/reconstruction-readiness.md`).
5. **Fail-closed error algebra** — canonical codes (`truncated`, `resource_constraint`,
   `provider_timeout`, `evidence_integrity_mismatch`, `cleanup_incomplete`,
   `configuration_invalid`, …) each with remediation and bounded `recovery` hints; demonstrated
   live in this run by the Bun-standalone expansion-budget refusal
   (`analysis/evidence/artifact-claude-exe.json`).
6. **234 tools / 180 CLI commands across ~30 target formats**, each with a dedicated guide among
   109 doc pages and machine-checked docs (`npm run docs:check`).
7. **Real-provider verification culture** — ~80 `verify:*` scripts plus Stryker mutation testing
   of reverse-tools semantics (`package.json`).
8. **Snapshot reuse** — `close_binary` can retain an analysis snapshot; caches are keyed by
   analysis-profile digests so stale snapshots are not silently replayed
   (`docs/cli.md` "Save and reuse analysis snapshots").
9. **AgentEnv export** — recorded HTTP interactions become replayable RL/eval environments whose
   write tools are enabled only when the capture confirms their effect
   (`rea environment-build/serve/compare/export`).
10. **Family-scoped advertising** — `REA_MCP_TOOL_FAMILIES` trims the advertised catalog per
    client; instructions self-adapt (`src/server/serverInstructions.ts`).
11. **Summary-first defaults everywhere** — `detail: "summary"` + retained views is the default
    interaction pattern, keeping agent context small without losing completeness.
12. **Shipped workflow skill** — a 525-line, versioned SKILL.md encodes the investigation
    methodology so every client gets the same evidence discipline (`.agents/skills/
    reverse-engineer-anything/SKILL.md`).

## 8. Lessons for Marfim (integrating an REA-style evidence pipeline natively)

1. **Make Evidence a data type, not a log.** One canonical record (ID, subject digest, operation,
   normalized result, limitations, authority, coverage) referenced by ID everywhere; return it in
   both structured and text form.
2. **Summary-first with retained paging.** Default replies stay small; complete results persist
   server-side under a `parent_evidence_id` and are paged on demand. This is REA's single biggest
   context-efficiency idea.
3. **Contract registry → generated surface.** Declare tools once (name, description, input/output
   schema, effects, examples); generate MCP schemas, CLI, docs, and catalog digests; checksum the
   catalog so clients can pin identity.
4. **Encode epistemic status in results.** `confidence`, `authority`, `coverage`, and explicit
   unknowns stop an agent from over-claiming — exactly what a clean-room project must prove.
5. **Reference-based composition.** Comparisons accept inline, file, or retained references;
   integrity mismatches are hard errors, never silent re-reads.
6. **Obligation ledgers for closure.** A reconstruction is done when named obligations close
   against Evidence of comparable authority plus a passing verifier — adopt as Marfim's
   definition-of-done for feature parity.
7. **Skill + doctor + uninstall symmetry.** Ship workflow instructions with the server; provide
   machine-readable `doctor --json` triage and a symmetric `uninstall`; setup shows a plan, backs
   up files, and requires approval.
8. **Refuse at capacity, don't degrade silently.** Bounded budgets with `truncated` /
   `resource_constraint` and remediation beat half-results (demonstrated in TARGET C).
9. **Effects taxonomy per tool** surfaced in schemas and instructions, so host permission systems
   map cleanly onto the catalog.
10. **Keep guidance in-product** (`search_guidance` / `read_guidance_section`) so workflow
    knowledge travels with the server instead of a possibly stale skill copy.
