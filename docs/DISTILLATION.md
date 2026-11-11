# Distillation Ledger — every surveyed harness → Marfim

Sources scanned (latest builds, deep-scan of every module family):
- **Oh My Pi** `can1357/oh-my-pi` v18.18.0 — TS agent surface + ~763k-line Rust core (12 `pi-*` crates), 81 providers / 8,675 models in catalog. (§R–§X + second ultra-heavy pass §OMP2)
- **Hermes** `NousResearch/hermes-agent` v0.23.0 (2026-11-05) — ~10.8k Python files, agent loop + aux failover stack + SQLite/WAL durable state. (§R–§W)
- **opencode v2** `sst/opencode` (anomalyco) 2.2.5, commit `1bf08ae` (2026-11-02) — ~617k-line Effect-TS monorepo, client/server core + 5 surfaces. (§OC)
- **dsh-superagent** `deepseek-ai/dsh-superagent-prototype` commit `6337fc8` (2026-10-31) — proactive repo agent bundle: deterministic gate + Mandates + durable effects/outbox. (§DSH)
- **ECC** `affaan-m/ECC` 2.2.3, commits `b38be6a7`→`d2ef2680` (2026-10-29→11-06) — 71 agents / 98 commands / 314 skills / 61 hook scripts, the "harness OS" discipline pack. (§ECC, delta §CC)
- **claude-code** npm 2.1.317→2.1.319 (§CC).

Every extracted spec is ledgered below with a decision: **ADOPTED** (in this
build, with the commit), **QUEUED** (spec captured, scheduled), **REJECTED**
(with the reason). Marfim's hard constraints (forbid unsafe, no
unwrap/expect outside tests, determinism, deny>ask>allow, fail-closed)
applied as the acceptance filter.

---

## §R Reasoning parameters — ADOPTED

| Spec | Source | Decision |
|---|---|---|
| Canonical ladder `minimal\|low\|medium\|high\|xhigh\|max` | OMP `packages/catalog/src/effort.ts` | ADOPTED — `providers/effort.rs::Effort` (config key `effort`, unknown levels fail closed) |
| Anthropic budget table xhigh 32k / high 16k / medium 8k / low 4k | Hermes `anthropic_adapter.py:73` | ADOPTED — `anthropic_budget_tokens()` |
| `max_tokens ≥ budget + 4096` when thinking enabled | Hermes adapter rule | ADOPTED — enforced in `anthropic::build_body` (differential golden updated) |
| Thinking pins `temperature: 1` | Hermes adapter | ADOPTED — user temperature honored only when thinking off |
| Chat-completions `reasoning_effort` accepts `minimal..high` only | OMP wire | ADOPTED — xhigh/max collapse to `high` |
| Responses `reasoning.effort` accepts `xhigh` | OMP wire | ADOPTED — max collapses to `xhigh` |
| Gemini `thinkingLevel MINIMAL\|LOW\|MEDIUM\|HIGH` | OMP `mapEffortToGoogleThinkingLevel` | ADOPTED — xhigh/max → `HIGH`, `includeThoughts: true` |
| GPT-5.6+ "Juice" forced-off trick (`# Juice: N`) | OMP `openai-responses.ts` | REJECTED — vendor-specific prompt hack; revisit only with a real model need |
| Anthropic adaptive thinking (`{type:"adaptive"}`) + per-model capability tables (fable/opus-5.5 mandatory-thinking etc.) | Hermes adapter | QUEUED — needs a per-model capability registry; see §P |
| Thinking replay policy (keep signed thinking per model generation, sha256-fingerprint suppression of signature-400 blocks persisted per session) | Hermes `anthropic_thinking_policy.py` / `_replay.py` | QUEUED — Marfim currently replays verbatim; the fingerprint ledger is the correct upgrade when a signature-400 is first observed live |

## §R2 OpenAI Responses wire — ADOPTED (new file `providers/responses.rs`)

| Spec | Source | Decision |
|---|---|---|
| Request `{model, input[], instructions, stream:true, store:false, reasoning{effort}, max_output_tokens, tools(flat), tool_choice}` | OMP `openai-responses.ts:1851` | ADOPTED — `build_body` + `validate_body` (fail-fast D10) |
| Flat tool format `{type:"function", name, description, parameters, strict}` | OMP wire | ADOPTED — registry schemas normalized; `strict:false` (harness re-requests unparseable args) |
| SSE: `response.output_item.added/done`, `response.output_text.delta`, `response.reasoning_summary_text.delta`/`response.reasoning_text.delta`, `response.function_call_arguments.delta` | OMP wire | ADOPTED — pure `apply_event`, fixture-tested |
| Terminal item's complete `arguments` win over accumulated deltas | OMP "final parse unconditional" | ADOPTED — `output_item.done` replaces the slot |
| `response.completed/incomplete/failed/error` terminal mapping; incomplete→MaxTokens; tool-call presence wins; failed-with-no-output→Provider error | OMP + Hermes error taxonomy | ADOPTED |
| Usage: `input_tokens`, `output_tokens` (incl. reasoning), `input_tokens_details.cached_tokens` → cacheRead | OMP accounting | ADOPTED |
| `previous_response_id` + `store:true` chaining with stale-chain reset | OMP stateful mode | REJECTED — Marfim owns history and replays it (stateless `store:false`); chaining would split truth between two stores |
| `prompt_cache_key "pc_" + ≤64`, `prompt_cache_retention "24h"` | OMP wire | ADOPTED (batch 2) — `prompt_cache_key(session_id)` on every Responses request (`pc_` + session id, ≤64); retention stays QUEUED (needs cache-TTL plumbing) |
| Custom tools (`apply_patch` grammar tools, `custom_tool_call_input.delta`) | OMP wire | REJECTED for now — Marfim has no custom-grammar tool yet |
| ZDR forces `store:false` | OMP compat | MOOT — Marfim is always `store:false` |

## §R3 Gemini native wire — ADOPTED (new file `providers/gemini.rs`)

| Spec | Source | Decision |
|---|---|---|
| `POST {base}/models/{id}:streamGenerateContent?alt=sse`, auth `x-goog-api-key`, model in URL not body | OMP `google-shared.ts:898` | ADOPTED |
| `systemInstruction:{parts:[{text}]}` (never a system role in contents) | OMP wire | ADOPTED |
| `tools.functionDeclarations[{name, description, parameters}]` + `toolConfig.functionCallingConfig.mode:"AUTO"` | OMP wire | ADOPTED |
| Tool results = `functionResponse{name, response:{result\|error}}` on a USER turn; name resolved from assistant history | OMP conversion | ADOPTED (id→name BTreeMap built per request) |
| `functionCall` parts → synthetic ids `fc-<n>` (v1beta carries no id) | OMP + Marfim harness contract | ADOPTED — deterministic ids |
| `thought:true` parts → Thinking blocks/events | OMP wire | ADOPTED |
| `usageMetadata`: prompt/candidates/thoughts/cachedContent token counts; thought tokens fold into billed output | OMP/Hermes accounting | ADOPTED — fixture-asserted |
| `finishReason` map STOP/STOP_SEQUENCE/MAX_TOKENS/SAFETY; tool-call presence wins over raw reason | OMP wire | ADOPTED |
| Thought signatures preserved per block for replay | OMP wire | QUEUED — store signature alongside Thinking block when first needed |
| Vertex `X-Vertex-AI-LLM-Shared-Request-Type` header / google-vertex family | OMP wire | REJECTED — Marfim targets the public generative-language API; Vertex auth is a separate credential world |
| Context cache via caller-owned `cachedContent` resource | OMP wire | QUEUED — needs resource lifecycle management |

## §F Failover / provider health — ADOPTED (router)

| Spec | Source | Decision |
|---|---|---|
| Cooldown ladder `min(60s × 2^n, 14400s)` on repeated failures | Hermes `fallback_cooldown.py:69` | ADOPTED — `router::cooldown_ms` + `mark_failure` (consecutive-failure counts, deterministic) |
| Provider-declared reset_at / Retry-After beats the ladder when longer | Hermes + OMP | ADOPTED — `mark_failure(retry_after_ms)` takes `max(ladder, declared)` |
| Success resets the ladder AND clears any stale availability window | Hermes `restore_primary_runtime` semantics | ADOPTED — `mark_success` |
| Aux client stack: parameter-strip rungs (temperature → format → reasoning floor → max_tokens), credential refresh, key rotation, quarantine walk, narrowed-error propagation | Hermes `auxiliary_client.py` recovery ladder | QUEUED (large) — the ladder shape is captured; Marfim's provider transports already own the single retry layer (INV-06), so the rungs belong there as `retry-after-shape-errors` once a live 400-shape error justifies each rung |
| Health TTL 600s keyed (profile, label\|endpoint); transient candidate quarantine 60s; payment scoped to endpoint | Hermes `auxiliary_health.py` | QUEUED — maps onto `mark_failure` with per-class TTLs |
| Key rotation mutating the published context in place | Hermes `auxiliary_key_rotation.py` | REJECTED for now — single-credential-per-provider model; revisit when credential pools exist |
| In-provider capability-scoped recovery (strict-tool 400 → disable strict for session) | OMP settings | QUEUED — needs session-scoped compat flags |
| `no retry after user-visible output` (first emitted delta commits the attempt) | OMP retry rules | ALREADY HELD — Marfim's provider transports retry only before the stream is consumed (transport retry loop wraps the POST; stream errors after first byte are terminal `stream error`) |
| Stream watchdogs: first-event 300s + idle 300s, env-overridable | OMP `PI_STREAM_*` | ADOPTED (batch 2) — `providers::StreamWatchdog` in all four SSE loops (`PI_STREAM_FIRST_MS`/`PI_STREAM_IDLE_MS`; bad/zero values keep the default — the watchdog never disables itself) |

## §C Compaction / context — ADOPTED (trigger math)

| Spec | Source | Decision |
|---|---|---|
| Trigger = `(ctx − max_out) × pct`, 64k floor **capped at 85%** of effective, `<512k` windows get raise-only 75% floor | Hermes `_compute_threshold_tokens` | ADOPTED — `compact::trigger_tokens` (pure, 6 hand-derived golden tests); wired into `maybe_compact` as a raise-only floor on the configured threshold; actuation override still wins |
| `compression.threshold = 0.50` default | Hermes config | ADOPTED — `DEFAULT_TRIGGER_PCT` |
| Lean tail budget: floor 10k / cap 25k / 2.5% window / ≤20% window | Hermes `context_compressor_tail.py` | ADOPTED (batch 2) — `compact::tail_budget_tokens` + `micro_compact_budget`: the message-count tail EXPANDS to cover the token budget (never shrinks); a budget covering the whole history elides nothing and stage 2 owns the reduction |
| Verbatim user-message section in summaries (24k chars / 4k per msg, newest-first) + recovery footer steering to session search | Hermes lean summary | ADOPTED (batch 2) — `compact_via_small` carries the verbatim section + the footer (pairs with SessionSearch, adopted in the same batch) |
| Cut point never at a toolResult; split-turn prefix summary | OMP compaction | PARTIAL — micro_compact already never elides the live exchange (protected tail); full-summarizer cut-point logic QUEUED |
| Micro-compaction defrag ≥2000 tokens; 3-failure skip | Hermes `micro_compaction.py` | PARTIAL — Marfim has micro-compaction; the defrag counter QUEUED |
| Skill-prune marker re-injection (`[SKILL_PRUNED: …]`, max 20) | Hermes | QUEUED — applies once skills enter the prompt (§S) |
| Cache-aware idle compaction (fire 5s before 1h-cache refresh; arm at ≥200k ctx) | OMP compaction | QUEUED — valuable only after prompt-cache TTL plumbing |
| Shake (tool-output→artifact refs, protected 40k recent tokens, ≥20k savings) + snapcompact bitmap frames | OMP | REJECTED (snapcompact) — PNG-framed history is a vision-priced niche; QUEUED (shake) — artifact-ref elision |
| Context promotion to larger model before compacting on overflow | OMP trigger order | QUEUED |

## §P Provider surface — PARTIAL

| Spec | Source | Decision |
|---|---|---|
| Preset table: gemini native + openai-responses wires | this distillation | ADOPTED — presets now 10, gemini on native wire, new `openai-responses` preset; xai/openrouter/groq stay on chat-compat (documented) |
| Anthropic cache_control: ≤4 breakpoints, last-block placement, TTL-reject retry ladder | OMP `anthropic-wire.ts` | ADOPTED (batch 2) — hard cap: system + last 3 tools, deterministic suffix placement; TTL-reject ladder stays QUEUED (needs a live 400 to justify) |
| Usage de-dup math (DeepSeek `prompt_cache_miss_tokens` = billed input, OpenRouter `cache_write_tokens`) | OMP `openai-shared.ts:526` | ADOPTED (batch 2) — `normalize_usage` in `openai.rs`: input_tokens is the NON-cached billed part (golden fixture updated to the corrected semantics, note recorded in the fixture) |
| Model catalog: 81 providers / 8,675 models, KDL compat cascade | OMP `packages/catalog` | REJECTED (scale) — Marfim's committed models.dev snapshot is deliberately small; the KDL cascade is over-engineering at this stage |
| Credentials precedence chain (env → models.yml → OAuth pool → keychain) + `.env` chain | OMP docs/providers.md | PARTIAL — env-var model kept; OAuth pool QUEUED |
| Service tiers (`auto\|default\|flex\|scale\|priority\|ultrafast` per family) | OMP | REJECTED for now — single-tier requests; the enum is captured |

## §H Header / TUI — ADOPTED

| Spec | Source | Decision |
|---|---|---|
| Status segments `token_total`, `cost`, `context_pct` (+cache) | OMP `packages/tui/src/status-line/segments.ts` | ADOPTED — header now renders `tok in/out (cache N) | $cost | ctx N%` once usage is observed (never invented); ctx% = (in+cacheR+cacheW)/catalog window |
| `token_rate` (live tok/s) segment | OMP segments | ADOPTED (batch 2) — bounded 64-sample ring in the TUI (chars→tokens at 4/char), shown only while streaming; display-only, never persisted |
| Interrupt-and-redirect (type while streaming → deny pending + steer), queued follow-ups | OMP/Hermes TUI | PARTIAL — Marfim has steer queues; mid-stream deny-and-steer QUEUED |
| Display transcript survives compaction with divider | OMP TUI | QUEUED |
| Deny-and-steer approval UX (typed message denies + steers) | OMP approval | QUEUED |
| TUI `session_exit` pending-tool diagnostics + synthetic aborted assistant on resume | OMP sessions | PARTIAL — resume rebuild exists; synthetic-aborted entry QUEUED |

## §A Approval / permissions — LEDGERED (Hermes floor stack)

| Spec | Source | Decision |
|---|---|---|
| Floor-before-bypass ordering: user deny globs + hardline patterns run BEFORE yolo/mode-off | Hermes `approval_floors.py` | ADOPTED (already held) — Marfim's PermissionEngine evaluates deny before mode shortcuts; re-verified in gates |
| Hardline pattern corpus (root/home deletes, mkfs, dd to raw device, fork bombs, shutdown…), command-position anchoring, quote-masking, GNU rm flag permutation, Windows tier | Hermes `approval_detection.py:124-157` | ADOPTED (batch 2, unix tier) — `ShellRisk::Hardline` + `hardline_matches` over EVERY argv segment; denies BEFORE rules/modes (floor-before-bypass); GNU flag-permutation + Windows tiers QUEUED |
| Compound-command allowlist guard (quote-aware `$()`/backtick detection; control-chars + `-c/-e` reinterpretation) | Hermes port of OMP#7553 | PARTIAL — Marfim's shellparse splits compound commands already; quote-aware allowlist short-circuit QUEUED |
| Fail-closed unattended matrix (`cron_mode/single_query_mode/unattended_mode` default deny; no-human plugin escalation blocks) | Hermes config defaults | ADOPTED (already held) — no-UI ask fails closed in Marfim's permission engine |
| Approval provenance notes ("auto-approved by allowlist entry …" into tool result; provenance never decides) | Hermes `approval_provenance.py` | ADOPTED (batch 2) — `Verdict.provenance` on rule-allow, appended as `[auto-approved: …]` by `with_provenance`; never decides |
| Denial circuit breaker (3 consecutive denies → hard-stop addendum; anti-retry "STOP" contract) | Hermes breaker | ADOPTED (batch 2) — turn-scoped consecutive counter in the loop; exactly one addendum per trip, journaled; resets on success / turn end |
| Guardian-LLM smart approval (untrusted-command delimiting, temp 0, 16-token verdicts, APPROVE=command-only) | Hermes `approval_smart.py` | REJECTED for now — an LLM guardian needs an aux-model budget surface; revisit after §F aux rungs |
| Parser-limit save-and-recover (oversized payload auto-saved with `bash <file>` hint) | Hermes | QUEUED (S) |
| Human-wait accounting excluded from batch deadlines (timeout+60s ceiling) | Hermes `approval_human_wait.py` | REJECTED for now — Marfim approvals are synchronous |

## §S Sessions / memory / skills — LEDGERED

| Spec | Source | Decision |
|---|---|---|
| FTS5 session search: external-content FTS + trigram tokenizer, trigger-sync with stale state machine, quoted-token sanitizer, lineage-aware `session_search` tool (2k-char reads, scan cap 300) | Hermes `hermes_state_fts.py`/`_search.py` | ADOPTED (batch 2, Marfim corpus) — pure `core::sessionsearch` (BM25-lite + quoted phrases, scan cap 300, ≤2k snippets, deterministic ties) + `SessionSearch` tool over the JSONL journal; SQLite/trigram/stale-state-machine machinery QUEUED (no SQLite in the tree — the JSONL corpus needs none of it) |
| Memory loop: MEMORY.md/USER.md caps (2200/1375 chars), nudge every 10 turns, background review fork (read-before-write, do-not-capture list) | Hermes memory stack | PARTIAL — nudge cadence ADOPTED (batch 2: `NUDGE_EVERY_TURNS=10` appends a one-line checkpoint to the system surface, byte-identical base prompt otherwise); caps + background review fork QUEUED |
| Skills: agentskills.io SKILL.md format, usage telemetry sidecar, curator lifecycle, audit ledger with blob backups | Hermes skills system | PARTIAL — skills.rs loads SKILL.md; telemetry/curator QUEUED |
| JSONL session tree (`parentId` + leaf pointer), fork + branch summaries, `reset_boundary`, UUIDv7 | OMP sessions | PARTIAL — journal is append-only JSONL already; tree/fork QUEUED |
| Hashline edit anchors `[PATH#TAG]` + stale-tag recovery + seen-line enforcement (−61% tokens measured) | OMP pi-edit | QUEUED (L) — highest measured value; needs edit-tool redesign, planned after §S |
| Fuzzy-match edit policy suite (exact→banded Levenshtein→dominant ≥0.97/+0.08 auto-apply→≤5 occurrence previews; constants 0.95/0.92/0.8) | OMP `pi-edit/fuzzy.rs` | ADOPTED (batch 2, conservative subset) — 4th rung `fuzzy-dominant`: banded Levenshtein (band = len/8+4, capped 64), auto-apply only at ≥0.97 similarity with ≥+0.08 dominance; the 0.95/0.92 preview bands NEVER auto-apply (no mid-edit confirm surface → fail-closed refusal) |
| Structural file summarization (tree-sitter fold + BFS unfold, min 4/6 lines) | OMP `pi-ast/summary.rs` | REJECTED for now — tree-sitter dependency budget; regex-based repomap stays |
| Read-tool summarization + selector grammar (`:A-B,:-N,:raw`), 1-lead/3-trail context | OMP read tool | ADOPTED (batch 2, selectors) — strict-suffix grammar (invalid suffixes stay part of the path), ranges add 1-lead/3-trail `[ctx]` lines; summarization QUEUED (needs the selector to drive it) |

## §X Rust-core engineering — LEDGERED (Oh My Pi natives)

| Spec | Source | Decision |
|---|---|---|
| Fail-closed trait defaults as sandbox pattern (`signal_process → Unsupported`, `child_opens → false`, `is_native_local → false`) | OMP `bash/host.rs`, `pi-vfs` | ADOPTED (already held) — Marfim sandbox denies by default; pattern documented |
| CancelToken split semantics (`abort_reason()` excludes elapsed deadlines so completed work is never spuriously rejected) | OMP `pi-natives/task.rs` | AUDITED (batch 2) — Marfim's token is flag-only and never conflated with deadlines (timeouts live in `tokio::time::timeout` wrappers that own their error); the OMP invariant holds by construction — audit conclusion documented on the type |
| Chunk-fed grep state machine (`search_push`, Send across awaits) | OMP vendored grep-searcher | QUEUED (M) — for the async grep tool |
| Stamp-based directory listing cache (64 shards, 64MiB, 50ms racy window, zero invalidation API) | OMP `pi-walker/cache.rs` | QUEUED (M) |
| Parse-cache cost model (16 B/byte + 64 B/node, 64MiB cap) | OMP `pi-ast/parse_cache.rs` | QUEUED |
| Safepoint VM loop (Budget{20k ops, 2ms}, per-op control word) | OMP `pi-shell/vm` | REJECTED for now — Marfim spawns real processes, not an in-process shell VM |
| Panic-containment wrapper (catch_unwind + double-panic-safe payload disposal) at FF/worker boundaries | OMP `pi-natives/task.rs:200-285` | MOOT — `#![forbid(unsafe_code)]` + no panic paths in Marfim core |
| CoW session isolation ladder (clonefile→FICLONE→btrfs/ZFS→overlayfs→git worktree→rcopy) | OMP `pi-iso` | QUEUED (M) — for subagent worktree isolation |
| Walker discipline (no owned runtime, drop-future=cancel, `coop::consume_budget`) | OMP `pi-walker` | QUEUED (S) |
| Ordered bounded execution pool (INLINE_BUDGET 250µs, chunks ≤32) | OMP `pi-walker/exec.rs` | QUEUED (S) |
| N-API-free core + thin adapter (pure rlibs, bin owns boundaries) | OMP `pi-voice`/`pi-predict` | ADOPTED (already held) — marfim-core is pure rlibs |
| Measured operating-point docs (constants + the bench that justified them) | OMP AGENTS.md | ADOPTED — this ledger + doc-comments follow the convention |
| Embedded coreutils (uutils) / obash differential testing / pi-voice / pi-predict ngram | OMP crates | REJECTED for now — binary size and scope; the *differential-testing pattern* is QUEUED for shellparse |

## §G Gateway / cron / delegation — LEDGERED

| Spec | Source | Decision |
|---|---|---|
| Platform-specific toolset presets (untrusted webhook surface gets only web_search/web_extract/vision/clarify) | Hermes `toolsets.py` | QUEUED — maps to Marfim gateway tool filtering |
| Async delegation ledger (SQLite dispatch rows, replay ≤48h, ≤8 attempts, orphan sweep 30s, results enter as NEW turn) | Hermes `async_delegation.py` | QUEUED (L) — when subagents land |
| Subagent contract (forced `yield`, per-call model/effort caps, yolo-forced children, blocked-tool set {delegate, clarify, memory, send_message, cron}, worktree isolation) | OMP task tool + Hermes delegation | PARTIAL — blocked-tool set ADOPTED (batch 3): `subagent::blocked_rules` denies {Task, TaskOutput, Memory, SessionSearch, Monitor, MonitorKill} on the child engine (deny>ask>allow; recursion + parent channels closed); forced yield already held (TaskOutput poll → results enter as a NEW turn); worktree isolation already held (D16); per-call model/effort caps QUEUED (the spawner has no model surface yet — honest refusal, not a stub) |
| Cron: natural-language 5 schedule forms, `paused_reason` audit, missed-run catch-up, per-job model pinning, `[SILENT]`/`[CRON_FAILURE]` markers | Hermes cron | QUEUED (M) |
| Denial-breaker + coalesced pending approvals + request_id/settle | Hermes approval wait | QUEUED |
| Browser URL/SSRF floors (IMDS unconditional, private/CGNAT gated on backend, JS URL-literal scan incl. scheme-relative) | Hermes `browser_url_policy.py` | QUEUED (M) — if a browser tool is added |
| Catalog trust model (PR-reviewed YAML, SHA pins, no self-updaters) | Hermes optional-mcps | ADOPTED (already held) — Marfim MCP config is explicit trust-gated |
| Worker receipts exactly-once (execution_id + sequence + payload_digest) | Hermes worker_executions | QUEUED |
| Turn leases + compression locks (TTL 300s, dead-holder reclaim, wait notices 15s) | Hermes state | QUEUED — relevant for multi-surface sessions |

## §W Durable state — LEDGERED (Hermes state machine)

| Spec | Source | Decision |
|---|---|---|
| WAL policy module (journal_mode wal→delete fallback matrix, cross-VM fs refusal via mountinfo, macOS fullfsync, 64MiB journal_size_limit, never-live-downgrade, fail-closed probe) | Hermes `hermes_state_wal.py` | QUEUED (M) — before any SQLite state lands in Marfim |
| BEGIN IMMEDIATE write core (3 patience tiers, one pre-callback IOERR retry, FTS fail-open vs structural quarantine, checkpoint every 50 writes) | Hermes `hermes_state.py` | QUEUED (M) |
| Token-accounting writer (background coalescing deltas, atexit drain, non-blocking flush) | Hermes `hermes_state_usage.py` | PARTIAL — Ledger is synchronous today; background writer QUEUED |
| Maintenance prune+VACUUM (90d retention, freelist >0.25 gate, PASSIVE→VACUUM→TRUNCATE) | Hermes `hermes_state_maintenance.py` | QUEUED |
| Interrupt placeholder healing (structural `[interrupt: …]`, never prose) | Hermes placeholders | ADOPTED (batch 2) — an aborted turn closes with the exact `[interrupt: aborted]` marker at the next turn start; journal replay can never echo-poison |
| Trajectory compressor (protect head/first-of-role/last-4, tool-pair boundary snap, target 15250/summary 750) | Hermes `trajectory_compressor.py` | QUEUED (S) — for eval dataset prep |

---

## Count

- **ADOPTED in this build**: reasoning ladder + 4 wire emitters; Responses
  wire; Gemini wire; router cooldown ladder + mark_failure/mark_success;
  compaction trigger math; TUI header usage/cost/ctx segments; presets +
  config `effort` key (10 presets).
- **ADOPTED in batch 3 (this commit)**: the subagent blocked-tool
  contract (children cannot recurse, write the parent's memory, read its
  journal, or own long-lived monitors — whole-tool deny rules on the
  child engine).
- **ADOPTED in batch 2 (this commit)**: stream watchdogs; usage de-dup
  (DeepSeek/OpenRouter dialects); Anthropic 4-breakpoint cap;
  `prompt_cache_key`; token-budget compaction tail; verbatim summary
  section + recovery footer; hardline floor corpus; approval provenance
  notes; denial circuit breaker; session search (`core::sessionsearch` +
  `SessionSearch` tool); read selector grammar; fuzzy-dominant edit rung;
  memory nudge cadence; interrupt placeholder healing; token_rate header
  segment; CancelToken deadline audit; the brain's LearningFaculty
  actuations (Hoeffding model_select + permission_tighten, trait
  why/forget) attached to the production harness with /why and /forget.
- **QUEUED (spec fully captured above)**: aux failover rungs, health
  TTLs, thought signatures, cachedContent, lean-summary cut-point logic,
  skill-prune markers, cache-aware idle compaction, shake artifact refs,
  context promotion, TTL-reject ladder, OAuth pools, mid-stream
  deny-and-steer, display-transcript divider, synthetic-aborted entries,
  hashline edits, session tree/fork, chunk-fed grep, walker cache,
  parse-cache, CoW isolation, delegation ledger, subagent contract, cron
  forms, browser floors, worker receipts, turn leases, WAL state module,
  background usage writer, maintenance prune, trajectory compressor.
- **REJECTED with reasons**: snapcompact bitmaps, Juice prompt hack,
  Vertex family, KDL 8.7k-model cascade, LLM guardian approvals,
  safepoint shell VM, in-place key rotation.

### Ultra-heavy distillation wave (this commit) — §OC + §OMP2 + §DSH + §ECC

Four ultra-deep passes over opencode v2, Oh My Pi (second sweep),
dsh-superagent, and ECC. **303 decision rows** (script-audited: every
section of every forensic doc maps to at least one ledger row, every row
carries a decision, no malformed table rows — audit script lives
workspace-side per the G8 no-.py gate); **nothing stays exclusive to the
source repos**.

- **§OC opencode v2**: 105 rows across architecture/loop/tools/providers/
  permissions/sessions/TUI/extensibility/config/lessons (§OC-A…§OC-J). The held core is
  large (inbox W6.2, bounding W5.4, catalog snapshot, worktrees, hooks,
  unattended rulesets, CodeMode-off); the top QUEUED items are per-step
  content-addressed snapshots + revert (L), parallel tool dispatch (M),
  cassette testkit (M), managed-settings tier (M), custom commands/
  agents discovery (S), interrupted-stream continuation + max-steps
  toolChoice forcing (S), compaction `precompute` head-start (S),
  recurring cache-TTL warming (S).
- **§OMP2 second OMP sweep**: 87 rows over loop machinery, compaction
  extras, provider/auth surface, permission extras, session/memory
  extras, the un-rowed tool set, and TUI/UX. Top QUEUED: deferred LSP
  diagnostics with staleness (M), harmony/DSML leak recovery (M),
  reversible secret redaction (M), live in-flight steering (M), advisor
  reviewers (L), vibe mode (L). Doctrinal REJECTS: magic keywords,
  yolo-default, marketplaces, foreign-root discovery.
- **§DSH dsh-superagent**: 55 rows. W10.2 already carries mandates/
  effects/outbox/sources/cron/delegation; this pass captures the rest —
  admission table + observe-mode "would-accept" (S), router no-wait
  discipline (S), worker slots with the human priority lane (S), input
  read-tracking (S), ticket-end by source reality (M), worker env
  binding by shims (M), prompt versioning bound to sessions (M), the
  source-plugin cookbook (S), SETUP-as-agent-playbook (S).
- **§ECC full depth**: 56 rows beyond the §CC delta. W10.1 already
  carries instincts/vault/curator/caps/review; the top remaining items
  are GateGuard first-write fact-forcing (S — highest-value row left),
  hooks manifest + fingerprint drift test (S), config-protection floors
  (S), hookify trust rules (S), AgentShield self-audit command (M),
  santa-loop dual-model + verifier-defaults-refuted (M), pass@k eval
  kernels (M).

Every ADOPTED item is wired, bounded (explicit caps/clamps), observable
(events/journal/tests), reversible (pure functions + additive config), and
covered by tests in this commit.

---

## §CC Claude Code + ECC latest-build distillation (2026-11-07)

Baseline refresh loop against the latest builds of both reference repos
(prior forensic baselines: ECC `b38be6a7` 2026-10-29 v2.2.3; claude-code
`964d695` 2026-11-03 npm 2.1.317). Every number below is computed from the
artifacts themselves by an analysis script run in the build workspace
(NOT tracked in this repo — the no-.py gate G8 keeps the tree single-
language); the computed numbers and evidence digests are mirrored into
the forensic docs and `docs/forensic/`, which are the durable record.

### Deltas vs the prior baselines (computed)

| Metric | ECC (b38be6a7→d2ef2680) | claude-code (964d695→0ed9469, npm 2.1.317→2.1.319) |
|---|---|---|
| new commits | 6 | 4 |
| files changed | 6 (+203/−0) | 18 (+806/−0) |
| version | 2.2.3 (unchanged) | 2.1.319 |
| inventory | 71 agents / 98 commands / 314 skill dirs / 620 files — IDENTICAL; hook scripts recounted 61 at BOTH commits (prior dossier's "59" was a counting slip); 0 hook scripts touched in the delta | mods 4 → **5** (new `privacy`); sdk-tools.d.ts 4,234 lines, 90 export decls, 45 tool-union members, sha256 `2565f41f…` (2 doc/type atoms vs 2.1.317: `next_cursor` pagination semantics clarified; `status?: "proactive"` made optional) |
| CHANGELOG | — | 2.1.318: 84 entries (5 Added / 8 Changed / 41 Fixed / 29 Other / 1 Removed), Shannon H = 1.6762 bits; 2.1.319: 146 entries (15 / 16 / 76 / 38 / 1), H = 1.7318 bits; combined H = 1.7205 bits |
| CLI surface | — | 66 flags captured from the shipped linux-x64 binary (sha256 `dabafec8…`, 340,226,576 bytes); NEW vs dossier: `--brief` (SendUserMessage tool), `--remote-control-session-name-prefix`; subcommands unchanged |

### ADOPTED (this commit)

| Spec | Source | Decision |
|---|---|---|
| Outbound privacy guard: refuse (NO prompt) a shell call that BOTH holds the account email AND names a User-Agent; the address counts as-written, quote-stripped, case-insensitive, and one-pass percent-decoded; UA named by a UA word (`user-agent`, `user_agent`, `--user-agent` prefixes, `UA:`/`UA=`) or a client+flag pair (curl/curlie `-A`, wget/wget2 `-U`, `-User` for web clients, unicode dashes included); the address is excised before the UA test so `jo@x.ua:` is not misread; explicit deny beneath keeps its own reason; allow rules do not clear it; honest bypass catalog (computed values, variables, web tools, other encodings pass — "a courtesy, not a boundary") | claude-code `mods/privacy` (10 files, 234 LOC, README spec) | ADOPTED — clean-room `core::privacy::PrivacyGuard` (no upstream code copied); wired on every production engine via `settings.accountEmail`; 7 module tests + 3 engine tests (override-allow, deny-before-prompt, deny-beneath-keeps-reason) |
| `permissions.denyInsteadOfPrompting`: a call that would wait on a permission prompt is denied instead and the turn continues | claude-code 2.1.319 CHANGELOG | ADOPTED — engine flag `with_deny_instead_of_prompting`, config `permissions.denyInsteadOfPrompting`; converts rule-asks AND mode-default asks before the queue; test-asserted that no prompt job is queued |
| Config integrity fail-closed: a damaged config file fails the start with a readable error naming the file, and is never silently replaced or mutated | claude-code 2.1.318 CHANGELOG (invalid `~/.claude.json` handling) | ADOPTED — `load_layered` errors now carry the file path; regression test asserts path + "invalid TOML" in the error and byte-identical file afterwards |
| Raw shell command text reaches the permission engine (not just parsed argv) | claude-code privacy mod `textOf` doctrine | ADOPTED — `check_with_input`; Bash passes the raw line alongside argv |

### QUEUED (spec captured, scheduled)

| Spec | Source | Reason not yet in |
|---|---|---|
| PR-burst-guard moderation doctrine: event-anchored excess selection (opened events only close PRs NEWER than the trigger, creation-order + PR-number tiebreak), grandfathering (historical open PRs stay open), reopening consumes a new slot and closes only itself, `queue: max` with the honest 100-pending-runs limitation, reversible closures, dry-run reporting | ECC `pr-burst-guard` (+8 new tests: 27 total in guard suite) | repository-CI moderation, not harness behavior; maps to Marfim's future repo-automation/job-queue wave (ROADMAP v0.5) |
| System-prompt snapshot semantics: record the rendered prompt ONCE per conversation, reuse verbatim on every request and resume even when a later launch passes different text, reset at compaction | claude-code 2.1.319 `--system-prompt-snapshot` help | Marfim's section order is already byte-stable; snapshot-once requires session-state plumbing for a property that is currently an invariant by construction |
| MCP legacy-handshake pinning: a stdio server that dies <3s on the version probe is pinned to the legacy handshake for 7 days | claude-code 2.1.318 CHANGELOG | needs a probe-death ledger in the MCP client |
| `$.session.identity()` analog: managed-settings-gated identity op returning `{kind, organizationId?, email?, isVerified}` with a discriminated denial and "read unknown kind as no identity" forward-compat rule | claude-code mods/types d.ts +81 lines | Marfim has no managed-settings tier yet |
| Idle compaction must never replace a conversation with a non-summary | claude-code 2.1.318 fix | verified-by-construction in Marfim (micro-compaction is deterministic elision + verbatim tail); revisit when model-written summaries land |
| Permission-prompt input race (keys typed just after sending a message must not answer prompts) | claude-code 2.1.318 fix | TUI approval modal isolation; low incident probability, scheduled with the TUI wave |
| Gateway governance bundle: pooled budget conflict/priority rules, `login_notice`, JSON gateway log format, spend reporting windows | claude-code 2.1.319 CHANGELOG | Marfim's gateway (serve) has no multi-tenant budget ledger yet |

### REJECTED (with reasons)

| Spec | Reason |
|---|---|
| `--brief` SendUserMessage tool | agent-to-user channel exists in Marfim already (steering + approval queue); a second surface would split truth |
| Privacy-mod disable channels (`/plugin disable`, `enabledPlugins` keys, server-side switch, above-tier hooks) | the guard is a hardline-adjacent floor in Marfim, not a plugin: config-off (`accountEmail` empty) is the only switch, and it is honest in the docs |
| MCP task backgrounding + `CLAUDE_CODE_DISABLE_MCP_TASK_BACKGROUND` | Marfim's MCP calls are synchronous by design (deterministic journal ordering); async task settlement is the QUEUED durable-inbox wave |

Every ADOPTED item above is wired, bounded (fixed tool list, single decode
pass, excision-before-test), observable (refusal reason surfaces in the
tool result), reversible (empty `accountEmail` disables the guard), and
covered by tests in this commit.

---

## §OC opencode v2 (2.2.5, `1bf08ae`) — ULTRA-HEAVY DISTILLATION PASS

Full-tree extraction from `docs/forensic/opencode.md` (§1–§12 there).
opencode v2 is the closest architectural cousin (client/server core, PTY
daemon, durable sessions, tool registry, permission rulesets), so much was
already held; this pass decision-ledgers EVERY remaining spec so nothing
stays exclusive to that repo. Earlier adoptions from opencode landed under
§W (W6.2 durable inbox: "distilled from opencode v2 `specs/v2/session.md` +
Hermes worker receipts") and §X/W5 (CodeMode W5.9, hashline W5.1 lineage).

### §OC-A Architecture & surfaces

| Spec | Source | Decision |
|---|---|---|
| Layering rule: HTTP ops = protocol; public shapes/durable events = schema; runtime/persistence = core; server composes core; clients depend only on schema+protocol | root `AGENTS.md` + `specs/v2/README.md` | ADOPTED (already held) — Marfim's crate graph enforces the same cuts (`marfim-gateway` protocol vs `marfim-core` runtime vs `marfim-state` persistence; core is pure rlibs) |
| One server API, five clients (TUI/web/desktop/VS Code/ACP) on generated typed clients | `packages/protocol`, `packages/client` | PARTIAL — Marfim ships TUI + `serve` (HTTP+SSE) + ACP agent/client + LSP server; generated client flavors + committed OpenAPI QUEUED (S) (gateway protocol fixtures already pin the wire byte-exact) |
| `opencode api` — call any OpenAPI operation by operation-ID or `METHOD path` from the CLI | `packages/cli/src/commands/commands.ts` | QUEUED (S) — maps to a `marfim api` subcommand over the gateway routes |
| Bounded SSE subscriber queue (4,096 capacity) | `packages/server/src/event-feed.ts` | ADOPTED (already held) — gateway SSE is seq-enveloped, lifetime+ping bounded (`serve.rs` `sse_ping`, hard stream lifetime) |
| Local server auth: password + HMAC-signed 30-day tokens + 5-minute pairing codes for browser pairing | `packages/server/src/auth.ts`, `pairing.ts` | PARTIAL — bearer token MANDATORY with constant-time compare (W2.3); HMAC session tokens + pairing codes QUEUED (S) (needs a browser client to pair) |
| Location-scoped services (one layer graph per working directory; multi-root workspaces) | `packages/core/src/location.ts` | PARTIAL — Marfim is cwd-scoped per session; multi-root Location graph QUEUED (L) with the workspace wave |
| `workerd`-portable conditional imports (`#sqlite`, `#pty`, `#fff`) | `packages/core/package.json` imports map | REJECTED — Marfim is a single static binary by design (G10 size budget); no edge-runtime target |
| Per-directory PTY daemon with authenticated local sockets + owner-handoff tickets | `packages/process-daemon` (Rust, "opencode-pty") | ADOPTED (already held, different process shape) — `marfim-term` TermServer: PTY sessions, bounded `max_sessions`, list/close/active, in-process (no socket auth surface to attack; W3.1) |
| Cassette HTTP/WS record-replay with secret redaction + scripted openai-compatible backend for e2e agent tests | `packages/http-recorder`, `packages/simulation` | QUEUED (M) — already the W11 testkit-cassettes line; the redaction requirement is captured there |

### §OC-B Session loop — the durable-inbox turn model

| Spec | Source | Decision |
|---|---|---|
| Turn admission precedes execution: prompt publishes durable `session.inbox.enqueued` fact, projected, `inbox.delivered` atomically consumes it; prompts idempotent by inbox item id | `specs/v2/session.md`, `session/inbox.ts` | ADOPTED (already held) — W6.2 `marfim-state::inbox`: STORED FIRST, settle-once receipts keyed by `execution_id`, leases TTL 300s + dead-holder reclaim with wait notice |
| Delivery modes: `steer` (default; enqueue order at next Safe Step Boundary) vs `queue` (idle boundary); steers jump ahead; manual compaction and session-move are delivery boundaries | `specs/v2/session.md` | PARTIAL — Marfim has steer queues + follow-ups (W9.4 partial: mid-stream deny-and-steer QUEUED); control-item boundaries (compaction/move as inbox items) QUEUED (S) |
| Crash recovery on wake: settle stale `running` compactions and streaming/running tools by publishing failures before any new step | `session/runner/llm.ts` `settleStaleCompactions`/`settleStaleToolCalls` | ADOPTED (already held, different mechanism) — journal replay + `[interrupt: aborted]` placeholder healing (batch 2): an aborted turn closes with the exact marker; replay can never echo-poison |
| Hidden `title` agent auto-titles untitled sessions in a background fiber that never delays the main model | `session/title.ts` | QUEUED (S) — an aux-model job; maps onto the background-summarization runner (W8) |
| Hidden `summary` agent writes PR-style session summaries | `plugin/agent.ts` `PROMPT_SUMMARY` | QUEUED (S) — same aux runner |
| Compaction is a hidden primary agent so the summary request looks like a normal step | `plugin/agent.ts` | REJECTED for now — Marfim's summarizer runs through the small-model `compact_via_small` path (Hermes doctrine); a hidden agent would duplicate the aux runner |
| `steps` limit reached → append `MAX_STEPS_PROMPT` and force `toolChoice:"none"` so the model must answer in text | `session/runner/max-steps.ts` | QUEUED (S) — complements the W5.5 loop guards (which stop repeats); this caps total steps per turn and forces a text answer |
| Step outcomes `Completed\|Retry\|Continue\|RecoverFull\|Compacted\|OutputLimit` as an explicit enum | `session/runner/step.ts` `Outcome` | PARTIAL — Marfim's loop has typed turn results (retry rungs in providers, compaction actuation); an explicit Outcome enum at the step boundary QUEUED (S) |
| Provider context-overflow rejection BEFORE any output → compact → retry the step (`Outcome.Compacted`) | `step.ts` | QUEUED (S) — the shape rung ladder (W7.2) is the adjacent machinery; overflow→compact→retry needs the loop-level outcome wiring |
| Interrupted/partial stream → synthetic continuation message ("The previous response was interrupted. Continue from where you left off…") | `llm.ts` | QUEUED (S) — cheap, high-value recovery prompt |
| `finish === "length"` → up to TWO output-limit continuations before failing (inspects last 3 user/assistant rows) | `llm.ts` | QUEUED (S) |
| Pluggable retry policy chain (`model-retry.ts` attempt/delay; `session.retry` hook override; `RetryScheduled` event + runner sleeps the delay) | `model-retry.ts`, `session/runner/retry.ts` | PARTIAL — retry-before-first-byte is held (INV-06) + cooldown ladder (§F); the pluggable-chain + scheduled-retry-event shape QUEUED (M) |
| Multiple tool calls in one assistant turn run in PARALLEL (each in its own fiber); local execution starts only after the `Tool.Called` publication | `session/runner/step.ts` | QUEUED (M) — Marfim executes tool calls sequentially by determinism doctrine; parallel dispatch with journaled completion order is the upgrade path |
| Start snapshot capture forked CONCURRENTLY with the provider request | `step.ts` | QUEUED with snapshots (below) |
| Delivering new input resets the agent's step allowance | `specs/v2/session.md` | QUEUED (S) — pairs with MAX_STEPS |
| Durable session-event vocabulary (`session.inbox.*`, `session.step.*`, `session.compaction.*`, `session.revert.*`, `session.forked/moved/synthetic/permissions/…`) | `packages/schema/src/session-event.ts` | PARTIAL — Marfim's journal + bus events cover the same ground (typed, fixtures-pinned); the full naming table is the reference for any additive event |
| Three-trigger compaction `auto` (threshold) / `overflow` (provider rejected) / `manual`; `buffer` = tokens or %-of-window (default keeps 15% and ≥16k free); `keep` = verbatim tail beside the summary; `precompute` = background summary started NEAR the threshold and swapped in when compaction fires | `session/compaction.ts` (~1.2k lines) | PARTIAL — trigger math + raise-only floors + tail budget held (§C); background (async) summarization held (W8); the `precompute` NEAR-threshold head-start + swap-in QUEUED (S); overflow-trigger is the §OC-B Compacted-outcome line |
| Prompt-cache warming: idle keep-alive prompts aligned to model cache TTLs (`warming.prompt`, `interval` default "4 minutes"/`auto` = 90% of cache TTL, `duration` default "30 minutes") | `packages/schema/src/config/warming.ts` | PARTIAL — Marfim's bounded warming (W7.2: system-only prefix, 1 token, at most once per session when `cache.warm`) is the session-start form; the RECURRING idle keep-alive at 90%-of-TTL cadence QUEUED (S) — pairs with the §C cache-aware idle compaction row |
| Plan mode AS DATA: a plan agent + a re-injected `<system-reminder>` protocol + the plan-file-only edit exception, all config/prompt artifacts rather than bespoke code | `plugin/plan.ts` | PARTIAL — EnterPlanMode/ExitPlanMode tools held; the plan-file-only edit exception + compaction-surviving reminder re-injection QUEUED (S); the as-data framing is the doctrine |

### §OC-C Tools

| Spec | Source | Decision |
|---|---|---|
| Generic tool-output bounder: 2,000 lines / 50 KiB / 7-day retention, full output kept in a backing file | `packages/core/src/tool-output.ts` | ADOPTED (already held, different constants) — W5.4 `tools::bounding`: 30k-char cap, head 20k + tail 8k, verbatim spill to `.marfim/outputs/` with the backing path returned (BACKING_KEEP 64, newest-kept); REA retention doctrine |
| `read` 2,000-line/50 KiB page cap, 1-based offset | `tool/plugin/read.ts` | ADOPTED (already held) — Read with offset/limit, line-numbered, truncation marked, `:A-B/:-N/:raw` selectors (batch 2) |
| `edit` exact-string unique-match enforced unless `replaceAll`; CRLF-aware; typographic-variant fuzzy retry | `tool/plugin/edit.ts` | ADOPTED (already held) — Edit exact→line-anchored→whitespace ladder + fuzzy-dominant rung (batch 2, banded Levenshtein ≥0.97/+0.08); `replace_all` param present |
| `patch` structured multi-hunk (add/update/delete) | `tool/plugin/patch.ts` | ADOPTED (already held) — ApplyPatch/ApplyDiff tools |
| `glob` default limit 100, 30s timeout, hidden flag | `tool/plugin/glob.ts` | ADOPTED (already held) — Glob |
| `grep` ripgrep-syntax, literal/case flags | `tool/plugin/grep.ts` | ADOPTED (already held) — Grep |
| `shell` foreground timeout or background; combined output in a backing file with a bounded tail | `tool/plugin/shell.ts` | ADOPTED (already held) — Bash + BashOutput/KillShell background jobs, output bounding |
| `todowrite` items pending/in_progress/completed/cancelled | `tool/plugin/todo.ts` | ADOPTED (already held) — TodoWrite |
| `question` structured multi-choice/multi-select user questions | `tool/plugin/question.ts` | QUEUED (S) — pairs with the approval queue; clarify machinery already exists in the brain (confusables); needs a headless-safe surface (fail-closed when no UI) |
| `webfetch` URL→markdown/html; `websearch` via configured provider | `tool/plugin/webfetch.ts`, `websearch.ts` | ADOPTED (already held) — WebFetch/WebSearch with SSRF floors (W9.5) |
| `subagent` spawn/continue child session; nesting capped by `experimental.subagent_depth` (default 1) with parent-chain checks | `tool/plugin/subagent.ts` | ADOPTED (already held) — Task/TaskOutput with recursion denied by `subagent::blocked_rules` (batch 3) |
| `skill` loads a named skill on demand | `tool/plugin/skill.ts` | ADOPTED (already held) — Skill tool + curator lifecycle (W10.1) |
| `session_rename` / `session_move` manage the session from inside the loop | `tool/plugin/opencode.ts` | QUEUED (S) |
| `worktree_create/list/remove` git worktree management tools | `tool/plugin/opencode.ts` | PARTIAL — subagent worktree isolation held (WorktreeGuard, D16/W5.8); agent-facing worktree TOOLS QUEUED (S) |
| `models` lookup tool for routing/choice | `tool/plugin/opencode.ts` | QUEUED (S) — surfaces the offline catalog (13/788) to the model |
| `list_mcp_resources` / `read_mcp_resource` | `tool/plugin/mcp-resource.ts` | QUEUED (S) — MCP client held; the resources surface is additive |
| `artifact_create/read/update/delete` (publish/serve HTML behind experimental flag) | `tool/plugin/artifact.ts` | QUEUED (L) — needs the gateway to serve session-scoped artifacts |
| `schedule_create/list/pause/resume/delete` recurring agent runs | `tool/plugin/schedule.ts` | PARTIAL — cronjobs (W10.2: five NL forms, paused_reason, catch-up, model pinning, markers) held as the `/cron` surface; agent-facing schedule TOOLS QUEUED (S) |
| `experimental_image_generate` image gen/edit as background job | `tool/plugin/image-generate.ts` | REJECTED for now — no media surface in scope; the custom-tool registry is the adoption path when one exists |
| Model calling `bash` transparently routes to `shell` | CHANGELOG v2.2.5 | QUEUED (S) — cheap alias at dispatch (name normalization) |
| Tools default into CodeMode (`codemode: true`) unless they opt out; MCP opts in | `tool/AGENTS.md` | PARTIAL — CodeMode is opt-in (W5.9 batching/composition surface, default off, ToolSearch-discoverable); inverting the default REJECTED (deterministic journal ordering first) |
| Three-value tool response: schema-validated `output` (machine value) vs `content` (model-facing, durable) vs `metadata` (UI JSON) | `tool/AGENTS.md`, `specs/v2/tools.md` | QUEUED (S) — separates machine-consumable results from model-facing text for CodeMode composition |
| Registry semantics: latest valid registration wins; disposal reveals the previous definition; each request CAPTURES the tool definitions it advertises; MCP keeps ONE stable transform whose refresh preserves plugin override precedence | `tool/AGENTS.md` | PARTIAL — Marfim's registry is append/replace with MCP namespacing (`mcp__<server>__<tool>`, factory wiring); disposal-reveals-previous + per-request capture QUEUED (S) |
| Permission resource conventions: `read`→path, `glob`→pattern, `grep`→pattern (not path), `edit/write/patch`→path, `shell`→each scanner-parsed command, `websearch`→query, `subagent`→agent id | `tools.mdx` | PARTIAL — Marfim permissions are argv/path-granular for shell (shellparse) and path rules for fs tools; the full per-tool resource table QUEUED (S) to converge + document |
| Per-vendor tool-list deletion (grep/glob removed for GPT/Claude native lists) | `plugin/optimize.ts` (in-code: "unsettled") | REJECTED — keep ONE consistent tool surface for every model; opencode's own comment admits the UX is unsettled |

### §OC-D Providers & catalog

| Spec | Source | Decision |
|---|---|---|
| models.dev fetched at runtime, decoded defensively (unknown models/API entries SKIPPED, never fatal), priced (cache R/W + TTL multipliers), KV-cached; bundled snapshot = offline fallback; only types chat/decision/image kept | `packages/core/src/models-dev.ts` | PARTIAL — the bundled-snapshot half is HELD (offline catalog 13/788, corrupt→empty degrade, no unwrap); the runtime-fetch half REJECTED by D11 (no runtime network); refresh = committed script + diff (documented) |
| ~45 hand-written wire protocols in one in-house SDK | `packages/ai/src/protocols/` | PARTIAL — Marfim ships 4 native wires (anthropic, openai-chat, responses, gemini) + bedrock/vertex/mistral/custom-grammar (W7.5, 13 presets); breadth grows per-need, never catalog-driven |
| Provider auth matrix: OAuth device flow; browser callback page; PKCE; azure-cli; ChatGPT account sharing; SSO-aware Console | `plugin/provider/*`, `oauth/page.ts` | PARTIAL — OAuth2 PKCE + credential pools + rotation-as-failover HELD (W7.4); device flow + local browser callback QUEUED (S); azure-cli credential method QUEUED (S); account-sharing REJECTED (single-credential-per-provider doctrine) |
| Credentials listable/switchable/exportable/importable as JSON (`auth export\|import\|switch\|list`) | `packages/core/src/credential.ts` | PARTIAL — pools + rotation held; export/import/switch CLI QUEUED (S) (secrets-at-rest format needs an owner decision) |
| Well-known remote manifests: origins declare an auth command + remote config that injects providers | `packages/core/src/wellknown.ts` | REJECTED — remote config injection is a supply-chain hole; Marfim config is explicit + trust-gated |
| `baseURL` overlay keeps catalog metadata while redirecting traffic; per-provider/model `headers`/`body` overlays; compat knobs (`reasoning_field`, `max_tokens_field`, `media_input`) | `providers.mdx`, `model-compatibility.ts` | QUEUED (S) — extends the capabilities table (`providers/capabilities.toml`, §P per-model registry line) |
| Free models ordered first for signed-out users; free = every cost tier input price 0 | `tui/component/model-options.tsx` `isFreeModel` | QUEUED (S) — pricing is fail-closed already; the ordering is a catalog-display rule |
| Region/resource providers build their own URLs when baseURL unset (`BUILDS_OWN_URL`: Azure, Bedrock VPC) | `models-dev.ts` | PARTIAL — Bedrock SigV4 + Vertex bearer held (W7.5); the general BUILDS_OWN_URL table folds into the compat knobs line |

### §OC-E Permissions

| Spec | Source | Decision |
|---|---|---|
| Ordered `{action, resource, effect: allow\|deny\|ask}` rules; first-match; NO match defaults to `ask` | `packages/schema/src/permission.ts`, `core/permission.ts` | ADOPTED (already held) — same shape, plus Marfim's absolute deny floors run before modes/bypasses |
| Plain decline → `DeclinedError` ("user declined"); decline WITH FEEDBACK → typed tool failure the model acts on (`CorrectedError`) | `tool/AGENTS.md`, `step.ts` | PARTIAL — denial guidance addendum held (W9.4 breaker appends steering); the typed corrected-failure contract QUEUED (S) |
| "always" replies saved per action/resource; tools pass `save: ["*"]` or per-resource patterns | `permission/saved.ts` | PARTIAL — session-scoped allows held; durable per-resource "always" saves QUEUED (S) |
| Built-in `explore`/`title`/`summary` agents deny-all with narrow allowlists; `general` denies `question` + nested `subagent` | `plugin/agent.ts` | ADOPTED (already held, Marfim form) — subagent blocked-tool set (batch 3): {Task, TaskOutput, Memory, SessionSearch, Monitor, MonitorKill} denied on child engines; aux roles run capability-scoped |
| Plan mode denies `edit` everywhere except the plan dir; re-injects Plan-mode reminders when compaction strips them | `plugin/plan.ts` | PARTIAL — EnterPlanMode/ExitPlanMode tools held; plan-file-only edit exception + reminder re-injection QUEUED (S) |
| Unattended ruleset: scheduled jobs evaluate a SEPARATE ruleset; anything not explicitly allowed is DENIED; configured denies still win | `permission.ts` `Permission.Unattended` | ADOPTED (already held) — fail-closed no-UI ask + cronjobs run through the same engine with denies absolute |
| Enterprise managed settings (macOS plists / system dirs) have highest precedence; `policies` users cannot override | `managed-policy.ts`, `policies.mdx` | QUEUED (M) — the W9.4 managed-settings tier line; opencode is the precedent for precedence placement |
| `--auto` / hidden `--yolo` CLI overrides | `commands.ts` | PARTIAL — Marfim modes exist with deny floors absolute (W1.4); a yolo alias is deliberately NOT shipped (dangerous-default doctrine) |

### §OC-F Sessions, state, snapshots

| Spec | Source | Decision |
|---|---|---|
| SQLite WAL, `synchronous=NORMAL`, busy timeout, passive checkpointing; durable facts + projections (`SessionMessageTable` monotonic `seq`) | `database/database.ts`, `specs/v2/session.md` | PARTIAL — marfim-state ships the inbox/effects/delegation projections in sqlite (W6/W10.2) over the append-only JSONL journal (W6.3: journal provenance is the durable form; the tree is a pure index); WAL-policy module stays QUEUED (§W) |
| Session export/import as files | `cli/handlers/session/export.ts` | QUEUED (S) — journal files are already portable; a canonical export format + import validation is the addition |
| Revert/snapshots: content-addressed FS captures before/after work, changed-file attribution on `session.step.ended`, staged/committed/cleared revert events | `snapshot.ts` | QUEUED (L) — the highest-value remaining opencode feature: per-step snapshots + user-facing revert; Marfim has git.rs + journal rewind-with-audit-tail but no FS snapshot ladder |
| Token accounting per step (usage + cost recorded on the step; aggregates + rate-limit state; usage toasts + `/models` shortcut) | `session/usage.ts`, `usage-limits.ts` | PARTIAL — UsageRecord/Ledger held (`core::usage`, cost book, by_role); rate-limit state + toasts QUEUED (S) |
| Workspaces/projects model + session "move" between locations via inbox control items | `project.ts`, `workspace/` | QUEUED (L) — with the Location wave |
| Background work checked at idle boundaries + `experimental.idle` hook | `session/background.ts`, `llm.ts` | PARTIAL — BashOutput/KillShell/TaskOutput/Monitor held; idle-boundary wake + idle hook QUEUED (S) |
| v1 compatibility layer + DB migration + accepted-but-unsupported keys ignored WITH WARNINGS | `v1/`, `config/normalize.ts`, `migrate-v1.mdx` | PARTIAL — Marfim's config is strict-unknown-key (fail-closed, docs-as-tests); the "accept-with-warning" tier is deliberately NOT taken (silent-drift risk); the Node→Rust port doctrine lives in PORT-LEDGER |

### §OC-G TUI / CLI

| Spec | Source | Decision |
|---|---|---|
| Custom commands `.opencode/commands/<name>.md` with `$ARGUMENTS` + frontmatter (description/agent/model); nested paths = nested names; JSON commands config | `commands.mdx` | QUEUED (S) — prompt-template commands discovered from `.marfim/commands/`; complements the 67 built-ins |
| ~35 JSON themes + v2 token system; leader key (default ctrl+x) + named keymap commands | `ui/theme/themes/`, `keybinds.mdx` | QUEUED (S) — TUI wave; theme JSON + token set is self-contained |
| Slash surface: `/new /sessions /models /agents /usage /status /update /pair …` + server-defined commands merged into autocomplete | `tui/app.tsx` | PARTIAL — 67 commands + aliases held; the merge-with-server-defined surface QUEUED (S) |
| `opencode stats` shareable usage summary (days/year/lifetime, model/tool breakdowns, cost) | `commands.ts` | QUEUED (S) — over the held Usage Ledger |
| i18n 60+ locales + translated READMEs | `ui/src/i18n/*.ts` | REJECTED — single-language honest (English); the cost of locale drift is the LSP/share lesson in miniature |
| `service start/stop/status` background daemon; `debug agents\|config\|heap-dump\|paths` | `commands.ts` | PARTIAL — gateway serve + headless modes held; a daemonizing service manager REJECTED (single-binary doctrine); debug subcommands QUEUED (S) |

### §OC-H Extensibility

| Spec | Source | Decision |
|---|---|---|
| MCP: stdio + remote socket client; full OAuth client (auth-code + refresh rotation + concurrent-refresh dedup + per-server credential rows); >10 MiB stdio frames; server instructions injected into prompts | `mcp/stdio.ts`, `mcp/oauth.ts` (539 lines) | PARTIAL — MCP client + server held (fail-closed); OAuth-for-MCP QUEUED (S) (the W7.4 provider OAuth machinery is the pattern); frame cap is config-bounded |
| Plugins as npm/Git packages with semver + local dirs + `.opencode/plugins/`; typed capability contexts (tool/agent/session/event/provider/…) | `plugin/src/effect/*.ts`, `plugins.mdx` | REJECTED for now — remote-code plugins conflict with the clean-room + no-runtime-network doctrine; the ADOPTED form is: hooks (INV-09 command hooks, fail-closed) + MCP as the extension wire + skills as the knowledge wire |
| Custom agents `.opencode/agents/<name>.md` frontmatter (mode primary\|subagent, permissions, model, disabled) | `agents.mdx` | QUEUED (S) — markdown agent definitions discovered + compiled to child-engine configs (blocked-tool set + model pin) |
| Skills: SKILL.md + frontmatter, advertised when relevant, loaded on demand; global/project/served-library discovery | `skills.mdx`, `skill/discovery.ts` | ADOPTED (already held) — skills.rs + curator lifecycle + telemetry sidecars (W10.1); library-served discovery REJECTED (no runtime network) |
| Formatters: `formatter` config + built-in runners (prettier/gofmt/mix) with per-language executable discovery | `formatter/builtins.ts` | QUEUED (S) — post-edit format hook (pairs with ECC's Stop-time batching, §ECC) |
| LSP: accepted-but-ignored config; v1 implementation REMOVED | `migrate-v1.mdx` | DIVERGES (held, honestly) — Marfim ships an LSP SERVER (gateway) as a W11 surface; opencode's removal is recorded as the maintenance-cost lesson: any ambient feature needs an explicit removal path |
| ACP server for editor integrations | `cli/src/acp/` | ADOPTED (already held) — gateway ACP agent + client (W12-era) |

### §OC-I Config

| Spec | Source | Decision |
|---|---|---|
| Precedence: managed → global → project (`opencode.json(c)` / `.opencode/…`); list-valued keys CONCATENATE across files | `config.mdx` | PARTIAL — Marfim layering is env→global→project with consumed-section validation (W1.3); concatenation semantics for list keys QUEUED (S) with the managed tier |
| JSONC + variable interpolation + hot reload via watcher | `config/variable.ts`, `config/watch.ts` | REJECTED (JSONC/interpolation) — strict TOML, unknown keys fail closed; hot reload QUEUED (M) (watcher + invalidation) |
| Config schema published at a URL | `config.mdx` | MOOT — the schema is the Rust types + docs-as-tests (KNOWN_KEYS), the strictest form |

### §OC-J Engineering lessons (opencode's own §12, decisioned)

| Lesson | Decision |
|---|---|
| Adopt catalog+snapshot with defensive decode | ADOPTED (held) — offline snapshot, skip-not-fail decode |
| Adopt ordered permission rules + saved replies + unattended ruleset | ADOPTED (held) + saves QUEUED (S) |
| Adopt the durable-inbox turn model | ADOPTED (held, W6.2) |
| Adopt registry semantics for tools | PARTIAL (held core; disposal/capture QUEUED) |
| Adopt generic output bounding | ADOPTED (held, W5.4) |
| Data-driven prompt assembly (base template + per-family overlays + orthogonal instruction sources) | PARTIAL — section-ordered prompt assembly held; per-model-family overlay FILES QUEUED (S) |
| Crash-consistency: settle stale work on wake | ADOPTED (held, journal + interrupt healing) |
| Be cautious with CodeMode | ADOPTED as caution — CodeMode shipped opt-in/off (W5.9) |
| Avoid per-vendor tool-list hacks | ADOPTED (doctrine) |
| Budget for feature removal (LSP/share) | ADOPTED (doctrine — honest TODO/BLOCKED ledgers + removal paths) |
| Effect-TS trade-off: replicate the guarantees on a smaller core | ADOPTED (doctrine — Rust workspace, no Effect runtime) |
| Record traffic in tests (cassettes) | QUEUED (M, W11 testkit) |
| Ship client/server with generated clients from day one | PARTIAL — server+protocol held; generated clients QUEUED (S) |

---

## §OMP2 Oh My Pi — SECOND ULTRA-HEAVY PASS (gap sweep)

The first OMP distillation (§R–§X) swept the model/wire/failover/compaction/
approval/session families. This pass re-walks the full forensic doc
(`docs/forensic/oh-my-pi.md` §1–§12) and decision-ledgers EVERY spec the
first pass did not row, so the OMP knowledge is fully resident here.

### §OMP2-A Agent-loop machinery

| Spec | Source | Decision |
|---|---|---|
| Two-queue steering with mode knobs: `steer()` interrupting vs `followUp()` turn-boundary, each `"all"\|"one-at-a-time"` | `session/agent-session.ts` | PARTIAL — steer queues + follow-ups held (§H); the per-queue mode knobs QUEUED (S) |
| LIVE steering injected into the provider's IN-FLIGHT response via `LiveSteeringChannel` + latency-based conversion (converts only the steering batch, never the transcript) | `agent/live-steering.ts`, `agent-loop.ts` ~1900 | QUEUED (M) — needs provider-stream surgery; Marfim steers at boundaries today (honest) |
| Three abort controllers per tool batch: user abort, steering interrupt, and a SOFT steering signal tools may opt into (auto-backgroundable bash) | `agent-loop.ts` ~3288 | QUEUED (S) — the soft-cancel contract for long tools |
| Replayed/pending tool results from an interrupted run execute BEFORE the first model call; steering parks until the batch finishes | `agent-loop.ts` ~1277 | ADOPTED (already held) — journal replay owns exactly this ordering (durable inbox + receipts) |
| A stop boundary only continues the turn for LIVE user steering; asides fold into the next turn | `agent-loop.ts` ~1793 | ADOPTED (held in form) — turn continuation rules are the loop's own |
| Aside messages with explicit COMMIT/DISCARD contracts (advisor cards, hidden notices never corrupt state) | `types.ts` `ASIDE_MESSAGE_*` | QUEUED (S) — typed transient context slots |
| Harmony-XML / DSML tool-markup LEAK detection + recovery, with audit events and retry escalation | `pi-ai/utils/harmony-leak`, `docs/ERRATA-GPT5-HARMONY.md` | QUEUED (M) — provider-leak hardening in the wire layer |
| Speculative execution coordinator (968 lines): pre-launch work on predicted next tool calls; streaming tool-arg validation feeds already-validated prefixes | `agent/speculative-execution.ts`, `tool-arguments.ts` | REJECTED for now — determinism-first doctrine; the adoption form is the CodeMode batch surface (W5.9) once a measured need exists |
| Tool arguments validated incrementally WHILE the model still streams | `packages/agent/src/tool-arguments.ts` | QUEUED (M) — pairs with the streaming SSE parsers |
| `stripSchemaDescriptions` provider-side schema slimming | `agent-loop.ts` import | QUEUED (S) — per-provider compat knob (§OC-D line) |
| Output budget incl. image-token accounting (`image-tokens.ts`) + tokenizer module | `packages/agent/src/output-budget.ts` | PARTIAL — token budgeting held in compact (tail budgets); image-token table QUEUED (S) with vision inputs |
| OpenTelemetry spans (chat/execute-tool/invoke-agent) | `packages/agent/src/telemetry.ts` | REJECTED for now — no OTel dep allowed unverified; the journal + bus events are the observability surface; an OTLP exporter is the adoption form |
| Global pause gate | `packages/agent/src/pause.ts` | QUEUED (S) — a loop-level pause that parks tool dispatch (maps to the dsh Pause, §DSH) |
| Append-only-context mode (context only grows, no elision) | `append-only-context.ts` | QUEUED (S) — a compaction profile (methodOrder entry) |
| Run collector: per-run coverage + `ToolCallBlockedError` | `run-collector.ts` | PARTIAL — denial provenance + journal record the same facts per call |
| Sent-tool-definitions tracking (what schema the provider last saw) | `sent-tool-definitions.ts` | QUEUED (S) — pairs with §OC registry capture |
| Turn recovery: persisted turn state lets a crashed/interrupted turn RESUME; unexpected-stop classifier injects retry contracts | `session/turn-persistence.ts`, `unexpected-stop-classifier.ts` | PARTIAL — interrupt healing + resume rebuild held; the unexpected-stop CLASSIFIER (dead-end taxonomy + retry contract prompt) QUEUED (S) |
| Retry classification/backoff/credential-rotation/model-fallback orchestrated at `agent_end`; context-overflow EXCLUDED from retry (routed to compaction) | `session/turn-recovery.ts`, `docs/non-compaction-retry-policy.md` | PARTIAL — cooldown ladder + shape rungs held (§F); overflow→compact→retry is the §OC-B line (same gap, cross-harness agreement) |

### §OMP2-B Compaction extras (beyond §C)

| Spec | Source | Decision |
|---|---|---|
| Compaction is a method ORDER (`provider-native → snapcompact → summarize → handoff → shake`), per-config | `compaction/methodOrder` | PARTIAL — W8 landed ordered compaction strategies + background summarization; provider-NATIVE compaction calls (Anthropic/OpenAI/Bedrock compact APIs) QUEUED (M); handoff QUEUED (S) |
| Mid-turn threshold checks before EACH provider request; subagents always pin mid-turn on | `docs/compaction.md` | ADOPTED (already held) — `maybe_compact` runs per request (§C wiring) |
| `shake`: mechanical elision of heavy tool-result text, protected 16k tail, min 4k savings | `compaction/shake.ts` | QUEUED (§C row — artifact-ref elision) |
| Branch summarization captures abandoned `/tree` branches | `compaction/branch-summarization.ts` | QUEUED (S) — with sessiontree fork surfaces |

### §OMP2-C Provider/auth surface (beyond §P/§F)

| Spec | Source | Decision |
|---|---|---|
| Model-role lattice: chat roles `default, smol, slow, vision, plan, commit, tiny, memory, task, advisor, widget`; kind roles `image, web, speech, dictation, judge`; per-scope storage (`modelRoleStorage: project`) | `config/model-roles.ts` | PARTIAL — router roles held (usage by_role, failover per role); the full named-role lattice + per-scope storage QUEUED (S) |
| `modelProviderOrder` + `enabledModels` / `enabledProviders` / `disabledProviders` / `cycleOrder` / `modelTags` | `config/model-settings.ts` | QUEUED (S) — catalog filtering/pinning config |
| Per-provider compat as DATA files (243 KDL rule files) instead of conditionals | `catalog/src/compat/rules/*.kdl` | PARTIAL — REJECTED at KDL scale (§P) but the DOCTRINE is ADOPTED: `providers/capabilities.toml` is the quirks-as-data table |
| Auth broker + auth gateway: centralized SQLite credential vault + OAuth refresh; a forward proxy so containerized clients NEVER see access tokens | `omp auth-broker serve`, `docs/auth-broker-gateway.md` | REJECTED for now — single-user surface; the doctrine (tokens never in child env) is already held as the MARFIM_* env contract + sanitize_env |
| Quota/reset tracking per subscription provider (ChatGPT Codex backend, Claude, Gemini CLI) with auto-reset managers | `ai/src/usage/openai-codex.ts`, `claude-auto-reset.ts` | QUEUED (M) — provider-specific quota ledgers fold into the failover health surface |
| Cursor bridge transport + in-process tool handoff (run behind Cursor's agent infra) | `ai/src/providers/cursor.ts` | REJECTED — vendor-bridging out of scope |
| Subscription plans as first-class providers (ChatGPT Codex, Claude, Gemini CLI, Antigravity, coding plans) | catalog | REJECTED for now — Marfim providers are API-key/OAuth based; plan-backed providers need the quota ledger first |
| Local inference: ollama discovery, `local` catalog provider, MLX on-device, GGUF SmolLM composer prediction | `docs/models.md`, `tiny/local-inference-api.ts` | PARTIAL — local/openai-compatible endpoints work via presets; discovery + on-device REJECTED (binary size, G10) |
| Measured perf samples (tps/ttft) feeding the model browser | `tui/overlays/model-browser.ts` `ModelBrowserPerf` | QUEUED (S) — the header token_rate segment (batch 2) is the in-TUI form |

### §OMP2-D Permissions/safety extras (beyond §A)

| Spec | Source | Decision |
|---|---|---|
| Three-tier tool declaration `read\|write\|exec`; unknown/custom tools default `exec`; MCP tools FORCED `write` | `docs/approval-mode.md` | ADOPTED (already held, Marfim form) — tool-declared approval tiers (W5.6, the class system covers MCP) |
| Approval modes `always-ask`, `always-ask-cached` (grants persisted to `approval-cache.db`), `write`, `yolo` (default) | `tools.approvalMode` | PARTIAL — Marfim modes are deny-floor-absolute (W1.4); cached grants QUEUED (S) (the durable "always" saves line, §OC-E); yolo-by-default REJECTED (dangerous-default doctrine — OMP's own lesson) |
| Argument-dependent policy: tools return `policy: allow\|deny\|prompt` with `override` + `policyKey` per sub-tool dispatch; user overrides tighten but never bypass denies | `tools/approval-targets.ts` | PARTIAL — argv-level shell analysis + path rules held; the per-sub-tool policyKey surface QUEUED (S) |
| Restricted folders: force a prompt for anything inside listed paths regardless of mode, resolved through symlinks/case-folding | `tools.restrictedFolders` | QUEUED (S) — a permission-floor path class |
| Hook-based confirmation: extension `tool_call` events block with a reason | `extensibility/hooks/types.ts` | ADOPTED (already held) — INV-09 PreToolUse hooks answer `{"decision","reason"}` fail-closed |
| Reversible `$$HASH$$` secret obfuscation in provider-visible text, restored in tool args; env-pattern + credential-shape regexes | `docs/secrets.md`, `memory-backend/redact.ts` | PARTIAL — privacy guard (account-email excision, §CC) + sanitize_env held; the reversible redaction round-trip QUEUED (M) |
| Subagent read-only plans, plan-mode tool gating, bash git-guard, per-repo write locks, ulimit isolation tests | `task/read-only-policy.ts`, `pi-shell/git/guard.rs` | PARTIAL — worktree isolation + blocked tools held; a git-guard (refuse history-rewriting commands in agent shells) QUEUED (S) |
| Browser/computer tools get exec-tier approval + inspection helpers + `ComputerSafetyCheck` | `docs/tools/browser.md` | QUEUED (M) — rides the W9.5 CDP/browser line |

### §OMP2-E Sessions/memory/skills extras (beyond §S)

| Spec | Source | Decision |
|---|---|---|
| Internal URL router: `ssh:// xd:// skill:// memory:// history:// artifact:// agent:// cfg:// rule://` onto typed readable resources | `src/internal-urls/router.ts` | PARTIAL — the ADOPTED form is the backing-file spill (`.marfim/outputs/<tool>-NNNN.txt` paths returned in bounded views, W5.4) + SessionSearch over the journal; a typed internal-URL scheme QUEUED (M) |
| Five memory backends (`off/local/hindsight/mnemopi/sharpshooter`) surfaced via retain/recall/reflect/learn/memory_edit + `memory://` URLs | `docs/memory.md` | PARTIAL — Memory tool + memcaps + memvault + instincts held (W10.1); remote/vector backends REJECTED (no external services; mnemopi already REJECTED in §S for SQLite) |
| Foreign-session import (Claude/Codex transcripts) | `foreign-session-*.ts` | REJECTED — clean-room doctrine; the memvault is the ONLY foreign import surface, and it imports memory, not transcripts |
| Session entries beyond messages: typed entries (compaction, custom, usage, checkpoints) with branch trees, pins, export/share/fork/resume | `session/session-entries.ts` | PARTIAL — journal is append-only typed JSONL + sessiontree index (W6.3); pins + export/share/fork UX QUEUED (S/L) |
| Cost gates bounding session-TREE spend | `session/cost-gate.ts` | QUEUED (S) — a budget floor over the Usage Ledger (per-session-tree cap) |
| Provider boundary pinning + credential affinity seeding for child sessions | `session/` lifecycle services | PARTIAL — prompt_cache_key(session) + failover state held; affinity seeding QUEUED (S) with credential pools |
| Idle maintenance + exit diagnostics services | `session/idle-maintenance.ts`, `exit-diagnostics.ts` | QUEUED (S) |
| Workspace tree snapshot injected into prompt footer, SHARED with subagents | `workspace-tree.ts` | QUEUED (S) — the repomap is the on-demand form; a footer snapshot changes the base prompt (needs a byte-stability test) |
| System prompt = ordered block array with STATIC prefix; cache breakpoint before the first cwd-derived block so subagents in different worktrees share the cached prefix; children inherit the parent's `getPromptCacheKey` | `docs/system-prompt-customization.md` | PARTIAL — `prompt_cache_key(session_id)` on every request held (batch 2); sibling-prefix sharing across worktrees QUEUED (S) |
| Context files discovery across `.omp/.claude/.codex/.gemini` roots with `@import` support | `discovery/at-imports.ts` | PARTIAL — AGENTS.md/skills discovery held; foreign-root scanning REJECTED (clean-room); `@import` composition QUEUED (S) |

### §OMP2-F Tools not rowed in §S/§X

| Spec | Source | Decision |
|---|---|---|
| LSP as a loop citizen: 14-action tool + WRITETHROUGH (edits fed to language servers) + DEFERRED diagnostics injected at next yield with staleness counters per file-mutation | `lsp/tool.ts`, `lsp/deferred-diagnostics.ts` | QUEUED (M) — the W11 LSP-diagnostics line; deferred-diagnostics-with-staleness is the exact spec to build (OMP's lesson: "cheapest high-value IDE-wired-in feature") |
| DAP debug tool: 28 actions (launch/attach/breakpoints/stepping/evaluate/memory), stop-state caching, awaited outcomes | `tools/debug.ts`, `src/dap/` | QUEUED (L) — W11 DAP line |
| `eval` tool: persistent Python + JS kernels with retained state, streaming, tool/subagent bridges, budget/judgment/workpool bridges | `tools/eval.ts`, `src/eval/` | REJECTED for now — a persistent kernel is a process-lifetime liability; Compute (safe arithmetic) + CodeMode (composition) are the held forms; a persistent kernel QUEUED (L) behind an owner decision |
| `ast_grep` structural search + `ast_edit` structural rewrite (tree-sitter) | `tools/ast-grep.ts`, `ast-edit.ts` | PARTIAL — repomap tree-sitter frontends held (W5.3, feature `treesitter`, default off); ast_edit QUEUED (M) |
| `github` gh-CLI wrapper tool with per-repo write lock | `tools/gh.ts` | QUEUED (S) — a skills/commands concern more than a core tool |
| `wait` tool: block until background job / peer message / steering interrupt | `tools/wait.ts` | QUEUED (S) — pairs with TaskOutput/Monitor |
| `checkpoint`/`rewind`: mark state / collapse exploratory context into a report, safety-paired auto-include | `tools/checkpoint.ts` | PARTIAL — journal rewind with audit tail held; the checkpoint-REPORT (context collapse) QUEUED (S) |
| `context_notes` / `new_context` experimental context management | `tools/context-notes.ts` | QUEUED (S) |
| `learn` / `manage_skill` at runtime | `tools/learn.ts` | ADOPTED (already held) — Instinct + Skill + curator (W10.1) |
| `security_scan` SARIF scanner | `tools/security-scan.ts` | REJECTED for now — external scanner surface; AgentShield-style self-scan is the §ECC line |
| `ida` IDA Pro integration | `tools/ida.ts` | REJECTED — RE-specific binary surface |
| TTS + image-gen as BUNDLED CUSTOM tools (not built-ins) | `tools/tts.ts`, `image-gen.ts` | REJECTED for now — no media surfaces; the custom-tool REGISTRY is the adoption path |
| Browser/computer as eval-prelude facades (CDP supervisor, stealth patches, relay backends) | `tools/browser.ts`, `computer.ts` | QUEUED (M) — W9.5 CDP line |
| xdev tool virtualization: unmount discoverable tools from the request schema, remount as `xd://` devices behind read/write | `tools/xdev.ts`, `essential-tools.ts` | REJECTED for now — 41 tools is under the pain threshold; ToolSearch (W5.9) is the held discoverability form of the same goal |
| File-mutation QUEUE serializing conflicting writes + conflict detection + racy stamps + `cfg://` conflict URIs | `tools/file-mutation-queue.ts`, `conflict-detect.ts` | PARTIAL — hashline content-anchors structurally kill stale edits (W5.1); cross-tool write serialization QUEUED (S) |
| Oversized outputs spill into session artifacts with `agent://` addresses + truncation metadata | `task/output-manager.ts` | ADOPTED (already held, Marfim form) — bounding.rs spill with explicit backing path (W5.4) |
| Tool-choice directives queued per turn; loop-redirect prompts push the model off repeating failed calls | `session/tool-choice-queue.ts` | QUEUED (S) — the denial breaker (batch 2) is the guidance half; forced toolChoice QUEUED |
| Per-tool timeout clamps | `tools/tool-timeouts.ts` | PARTIAL — Bash/terminal timeouts held; a uniform per-tool clamp table QUEUED (S) |

### §OMP2-G TUI/UX (beyond §H)

| Spec | Source | Decision |
|---|---|---|
| Composer text prediction (n-gram + web-prior + GGUF SmolLM) | `crates/pi-predict`, `src/predict/` | REJECTED for now — client-side UX luxury; the brain ships the n-gram machinery, so the adoption form exists if a composer surface ever lands |
| Magic keywords (`ultrathink`, `orchestrate`, `workflowz`, `jevify`) injecting hidden per-turn contracts | `modes/magic-keywords.ts` | REJECTED — hidden magic (OMP's own "avoid" lesson); contracts stay explicit slash commands |
| Two-slot `theme.dark`/`theme.light` with luminance auto-switch; flat `theme:` migration | `config/settings.ts` | QUEUED (S) — TUI wave (same line as §OC-G themes) |
| Model browser overlay with measured tps/ttft; agent hub; live visualizer; context viewer; jobs panel | `tui/overlays/*` | QUEUED (S) — TUI wave |
| ~100 slash commands incl. `/seance /collab /vibe /goal /hub /wt` | `slash-commands/builtin-registry.ts` | PARTIAL — 67 held; OMP-specific ones are REJECTED/QUEUED with their features (seance §OMP2-H, vibe §OMP2-H) |
| Multi-root workspaces, tmux integration, desktop notifications, OSC 8 hyperlinks, Tern Surface Protocol, live collab with native guest rendering + browser join, IRC inter-agent messaging | `tui/src/tmux.ts`, `wire/tsp.ts`, `src/collab/`, `src/irc/` | REJECTED (collab/Tern/IRC — multi-user out of scope; gateway serve is the remote surface) / QUEUED (S) (tmux hints, desktop notify, OSC 8 — TUI wave) |
| Vibe mode: director/worker split — main session read-only director steering persistent worker sessions, verifying claims itself | `docs/vibe-mode.md`, `src/vibe/` | QUEUED (L) — a preset+contract over the held subagent engine; valuable once routers exist |
| Seance: consult/revive PAST sessions as read-only advisors with fork-from-history | `task/seance.ts` | QUEUED (M) — SessionSearch + sessiontree fork cover the retrieval half |
| Goal mode persistence tool (hidden `goal`) | `goals/tools/goal-tool.ts` | QUEUED (S) |
| Auto-research experiment loops on a git branch | `src/autoresearch/` | REJECTED for now — delegation ledger + cron cover the scheduling; the experiment-loop contract QUEUED (L) with recipes |
| BTW side-questions (aside turns) | `session/btw-turn.ts` | QUEUED (S) — with asides (§OMP2-A) |
| Advisor/"WATCHDOG" auxiliary reviewers watching the transcript, tool-equipped but isolated, advice cards, roster file | `advisor/runtime.ts`, `WATCHDOG.yml` | QUEUED (L) — the opt-in aux runner (W9.3 judge factory) is the seam |
| Extensions: 45 typed events, registerTool/Command/Provider, UI dialogs, link schemes; legacy hooks still loaded | `extensibility/extensions/types.ts` | REJECTED (runtime TS extensions — clean-room) / ADOPTED (the EVENT taxonomy informs the bus vocabulary) |
| TTSR rulebook: tool-triggered sticky rules (`tool:edit(*.ts)` matchers on streamed payloads, per-file digests) re-injecting warnings when violated | `session/ttsr-coordinator.ts` | QUEUED (S) — a hook+digest mechanism over the held hooks surface |
| Marketplaces + Skillshare registry + plugin packages | `docs/marketplace.md` | REJECTED — no runtime network; skills load from disk, curated |
| Foreign discovery providers (Claude/Codex/Gemini skills/commands/rules read-only) | `discovery/claude.ts` etc. | REJECTED — clean-room; SKILL.md + memvault are the interop formats |
| Failure isolation: extension load errors surfaced without killing the session; `--no-extensions`; `--trusted-extension <path>` | `docs/extension-loading.md` | PARTIAL — hooks are fail-closed per-call (a bad hook blocks only its tool, session survives) |
| Settings-as-registered-schema colocated with features + automatic legacy migration (queueMode→steeringMode, theme slots, isolation backends) | `docs/settings.md` | PARTIAL — strict KNOWN_KEYS + docs-as-tests held (a setting that isn't registered cannot exist); the legacy-migration engine REJECTED (no legacy users to migrate — Marfim is v1) |
| Layering: arrays REPLACE across project/global; broken YAML → `.broken-*` backup; unknown keys never silently promoted | `docs/settings.md` | PARTIAL — Marfim layering is validated + consumed-sections-checked (stricter); array-concat semantics QUEUED (S) (§OC-I line) |
| Metaharness self-measuring benchmarks (edit/compaction evals, session-stat scripts) | `packages/metaharness` | PARTIAL — differential fixtures + gates + docs-as-tests held; the bench DASHBOARD QUEUED (M) |

---

## §DSH dsh-superagent prototype (`6337fc8`) — ULTRA-DEPTH DISTILLATION

Full extraction from `docs/forensic/dsh-superagent.md` (§1–§12 there).
The W10.2 wave already ADOPTED the five load-bearing dsh doctrines
(Mandates, effect queue/outbox, source plugins, cron, delegation ledger —
see CHANGELOG 1.0.0/W10.2); this section decision-ledgers the REMAINING
spec so the whole dsh design is resident. dsh's governing principle —
"take as little human attention as possible, and give as much value as
possible" — is the autonomy doctrine Marfim's gateway wave builds on.

### §DSH-A Authorization & the gate

| Spec | Source | Decision |
|---|---|---|
| Two-layer Eval: DETERMINISTIC gate (allowlists + admission table + accept mode decides admissibility & strength) + model assessment ONLY over admitted candidates, under a schema forcing one verdict per rule + quoted intent evidence | `src/policy.ts` gate, `src/app/assessment.ts` AssessmentSchema | ADOPTED (W10.2, Marfim form) — `core::mandates`: the gate is deterministic with a pinned refusal order (revoked → expired → tool scope → path scope → budget); the LLM assessment is advisory, recorded on the outcome, and NEVER flips the gate (both directions test-asserted) |
| Admission table: cells listedUnaddressed / unlistedAddressed / unlistedUnaddressed → `explicit\|inferred\|ignore`; an explicit cell for unlisted-but-addressing senders DELIBERATELY beats follow-up strength ("a thread's tickets never weaken an explicit request") | `src/policy.ts` gate, `docs/002-authorization.md` | QUEUED (S) — the gate's matrix extension; the precedence rule (explicit > follow-up) is the load-bearing line |
| Accept mode `off\|observe\|auto`: observe records positive assessments as "would accept" (rehearsed, audited, inert) before autonomy is ever enabled | `src/policy.ts` EvalConfigSchema | PARTIAL — observe mode held for the Windows hardline tier (W9.3); the mandates-gate "would-accept" recording QUEUED (S) — the single most transferable autonomy dial |
| Mandates as first-class revocable authorization objects: basis `via` (addressed\|followup\|admission\|bot), scope, matched rules, POLICY VERSION stamped; revalidated before any resume; failed revalidation revokes; only accept creates, a person's stop withdraws; workers can never extend them | `src/core/model.ts` Mandate, `src/policy.ts` revalidate | ADOPTED (W10.2) — versioned revocable mandates, monotonic revocation, amendment history |
| "Outside text is data": messages/comments/quoted documents cannot change permissions; prompt precedence ladder = tool/host-enforced facts > built-in text > matched-rule instructions > team sections > outside text | `docs/002-authorization.md`, `docs/004-prompts.md` | ADOPTED (held, Marfim form) — untrusted-webhook toolset (W10.2: workers get web_search/web_extract/vision/clarify ONLY), privacy guard, deny floors; the explicit precedence LADDER is documented in DESIGN |
| Bots/CI messages can never authorize work (`machineGate`); bots are at most inferred candidates and only if on the Source's bot list | `src/policy.ts` machineGate | ADOPTED (held in form) — source routing treats machine events as data (kind allowlists + dedupe); the explicit bot-strength rule QUEUED (S) with the admission table |
| Identities are never mapped across Sources ("a person is not a domain concept") | `docs/001-domain.md` | ADOPTED (doctrine) — Marfim trust is per-surface (trust.rs), never cross-source identity inference |
| Intent evidence: acceptance requires quoted source statements establishing implementation_requested + settled target basis + no open design decision, audited on the Decision | `assessmentOutcome`, prompts/admission.md | QUEUED (S) — extends the mandates assessment record |
| An inferred accept outside `auto` mode is downgraded to an `ignore` recorded as "would accept" | `src/policy.ts` | QUEUED (S) — with observe mode |
| Setup-time identity checks: agent GitHub account must have PUSH access to the canonical repo name (renames resolved); Feishu CLI must be strict bot mode; a CLI that could also act as a user is REFUSED | `src/deployment.ts` resolveDeployment | QUEUED (M) — the source-plugin trust probe (a source that can act as a human must fail setup) |

### §DSH-B Router & worker session design

| Spec | Source | Decision |
|---|---|---|
| The router NEVER polls: candidates, follow-ups, reports, job results are steered INTO its turn as messages; `noWait` guard refuses `sleep`, shell timeouts > 30 s, `job_output` waits > 5 s | `src/app/sessions.ts` noWait, prompts/orchestrator.md | QUEUED (S) — the router-session contract over the gateway; maps to loop guards + the pause gate (§OMP2-A) |
| Pass scheduler: every 2 s run due effects then `evaluateDue`; one Eval per Thread with queued triggers BATCHED; expired Evals requeue twice, third expiry records `unavailable` | `src/app/passes.ts`, `core/engine.ts` | PARTIAL — the effect queue's advance loop is held (W10.2); the pass cadence + expiry ladder QUEUED (S) |
| Eval deadline starts only when the candidate card REACHES the session (delivery time never consumes budget); undelivered candidates never expire; a Policy change under an Eval discards & requeues it | `core/engine.ts` evaluateDue | QUEUED (S) |
| Batch routing: `orchestrator_route` takes 1–20 items; each runs as its own transaction; per-item field validation; a refused item does not stop the others | `src/app/tools/main.ts` routeTool | QUEUED (S) |
| Assessment read-only tools (the MasterAssessor protocol): `orchestrator_eval` (bounded window: 5 first + 5 around the trigger, images included), `orchestrator_eval_read` (expand stubs: message/root/parent/thread/file/directory/image), `orchestrator_eval_decide` (structured schema) | `src/app/tools/main.ts`, `src/app/assessment.ts` MasterAssessor | QUEUED (M) — the read-only assessment surface over source resources; pairs with the sources.rs resource model |
| Follow-up ownership: a person's message on a ticket's OWN thread goes straight to its worker (handle or handback); the router may only stop work meanwhile — removes the router from the hot path | `docs/002-authorization.md` "Two paths", `followupOwners` | QUEUED (M) — with router sessions |
| Worker loop: durable `launch` effect → reserve slot → prepare command (10 min timeout, env vars) → create/reuse `worker-<ticketId>` session in the ticket dir → deliver assignment (≤1500 chars + trigger card) | `src/executor/dsh.ts` | PARTIAL — the delegation ledger (W10.2) is the dispatch surface; per-ticket worker sessions + prepare commands QUEUED (M) |
| Worker self-management: `orchestrator_report` progress/ready/blocked/failed/dropped + structured `human_action` question + optional PR binding (atomically with the report) | `src/app/tools/executor.ts` | QUEUED (S) — the typed report contract |
| Worker turn lifecycle: idle → turnEnded; settling watcher while background jobs run (quiet or 30-min limit, then kill + report waiting with last words); a completed job WAKES the worker with its result in the same slot; after `ready`/question, handoff frees the slot; ticket end kills jobs and releases subagents BEFORE the replacement assignment starts | `src/executor/dsh.ts`, `background.ts` | QUEUED (M) — the worker lifecycle state machine |
| Worker slots: `maxWorkers` 1–16 (default 3) + ONE extra priority slot reserved for human messages/status so new work can never starve a person's continuation | `src/executor/dsh.ts` WorkerSlots | QUEUED (S) — the priority-lane doctrine |
| Worker tool allowlist (`WORK_TOOLS`): read/write/edit/glob/grep/bash/job_*/web_search/skill/todo/subagent…; workers DENIED any tool that asks a person, changes the profile, schedules work, starts other sessions, or edits external documents | `src/app/sessions.ts` workOnly | PARTIAL — the subagent blocked-tool set (batch 3) is exactly this doctrine for children; the source-worker profile QUEUED (S) |
| `waitsForPerson` tickets take no automatic inputs but KEEP their tools so a person can continue in-session | `core/model.ts` | QUEUED (S) |
| Ticket-ending driven by SOURCE reality, not self-grading: a PR ticket ends when the threads it follows closed (`followsDone`) | `core/tickets.ts`, `github-pull.ts` | QUEUED (M) |
| Ticket stages: closed mutually-exclusive set (person/delivery, person/answer, person/review, person/blocked, person/failed, working/queued, idle, working) derived from stored facts; `canRetry` gates retry offers | `core/stage.ts` | QUEUED (S) |
| Structured attention handoff: a worker's question is a typed `human_action` field relayed verbatim to the original conversation, not buried in prose | prompts/handoff-worker.md, `notices.ts` | QUEUED (S) |
| Everything-else-as-notice: ticket_ended, settings saves, requeues, unsubscribes are injected as background notices WITHOUT waking the session | `src/app/notices.ts` | QUEUED (S) — the event-routing policy |
| Input read-tracking: an Input counts as READ only after the session takes it into history AND a report or turn end follows — drives re-delivery after restart and what returns to Eval on ticket end | `core/engine.ts` inputsRead | QUEUED (S) — refines the delegation settle contract |

### §DSH-C Durable state & effects (beyond W10.2)

| Spec | Source | Decision |
|---|---|---|
| Effects = stored work — STORE-THEN-ACT everything: every post-decision action enqueued INSIDE the engine transaction, performed after commit, with per-kind concurrency limits, exponential backoff, `Later` deferrals, per-kind health entries that error only after 3 consecutive failures | `src/app/effects.ts` | PARTIAL — W10.2 landed store-first + strict insertion order + dead-letter at 8 attempts + `advance` as the human stall-breaker; per-kind coalescing/concurrency + Later deferrals QUEUED (S) |
| Outbox: per-message receipts, ordered per ticket+conversation, WAITING while a source is off, dropped (never retried) only when the conversation is provably gone; sends idempotent per `message.id` | `src/app/outbox.ts` | ADOPTED (W10.2) — the outbox half of `marfim-state::effects` |
| Acknowledgements/accepts/notices/sends deduplicated ACROSS retries and restarts | prompts/replies.md, effects.ts | PARTIAL — settle-once receipts held (inbox); reply-level dedupe QUEUED (S) |
| Single sqlite store + directory LOCK so a second process cannot migrate or write (proper-lockfile) | `src/app/store.ts`, runtime.ts | PARTIAL — marfim-state owns its sqlite file; a cross-process state-dir lock QUEUED (S) |
| Versioned kv families: settings/prompt revisions as numbered snapshots with a current pointer; per-session prompt revisions; source cursors/fingerprints; display labels | `src/kv.ts` | PARTIAL — journal provenance held (W6.3); a versioned-kv surface QUEUED (M) |
| Paging stability: thread timeline pages continue from `<at>\|<id>` cursor with times normalized to ONE ISO form at receive so equal timestamps never skip rows | `core/engine.ts` isoTime | QUEUED (S) |
| Pause semantics durable: holds Evals, dispatch, deliveries, replies while sources keep RECORDING; deadlines shift by the pause length; ended tickets touched during pause resume after it (Mandate revalidation applies) | `docs/003-execution.md` "Pause processing" | QUEUED (M) — with the pause gate |
| Resumability: on startup report interrupted assignments (recovery note: check dir, git history, upstream BEFORE repeating outside actions), requeue pending launches oldest-first, re-deliver inputs the session never took | `src/executor/dsh.ts` resume | PARTIAL — delegation replay ≤48h + orphan sweep held (W10.2); the recovery-note doctrine (verify before repeating external actions) ADOPTED as documentation |
| Growth is unbounded and ADMITTED (triggers/dispositions/decisions/outbox never deleted yet) | docs/007 "Persistence" | ADOPTED (doctrine — honest scale admission; Marfim's journal states the same) |
| Health board: every observable part claims exactly ONE health entry; a second claim is refused (one writer, one reporter) | `src/infra/health.ts` | ADOPTED (already held, Marfim form) — board.rs cross-agent message board + typed bus events; the one-writer-per-state rule is the design doctrine |
| Fixed refusals: typed `UserError(code)` from a CLOSED set, answered 404/400, localized in the panel | `core/errors.ts` | ADOPTED (held in form) — closed error enums everywhere (e.g. `sources::RouteError`); no stringly-typed refusals |

### §DSH-D Sources & environment

| Spec | Source | Decision |
|---|---|---|
| Source plugin contract (`SourceKind`): stable `kind` id, zod schema + DECLARATIVE fields (switch/select/number/text/fact/rows) auto-generating panel forms, declared health entries, `scope`/`channel`/`threadContext`/`find`/`policy`/`environment` hooks | `src/sources/kind.ts`, registry.ts | PARTIAL — W10.2 `core::sources` normalizes GitHub/Feishu events with kind allowlists + dedupe + fail-closed routing; the full declarative plugin contract QUEUED (M) |
| Source registry lifecycle: config validated against the kind schema; default instance from schema defaults (defaults that RECEIVE NOTHING); an instance whose stored config no longer validates is DISABLED; `send` answers sent/gone/waiting so the outbox holds or drops correctly | `src/sources/registry.ts` | QUEUED (S) |
| Cookbook for new source kinds: stable id derivation (re-reads dedupe), scope depends only on meta+config, out-of-scope messages still `receive`d (so the panel can EXPLAIN filtering), channel.send idempotent per message.id, `gone` only for permanent failure, policy.rows ids matching scope's, empty environment values for other sources' tickets | `docs/008-cookbook.md` | QUEUED (S) — the extension playbook, to be written as a Marfim doc when the plugin contract lands |
| Worker environment binding by SHIMS, not trust: private-PATH `gh`/`lark-cli` shims + one fail-shim per blocked command; isolated gitconfig (`useConfigOnly`, bound credential helper, URL rewrites, no askpass); ambient token/git-identity env vars UNSET for agent processes; repo-local `credential.*`/`http..extraheader` git keys in a ticket directory REFUSED | `src/infra/environment.ts` | PARTIAL — sanitize_env + the MARFIM_* env contract held (W1.2); CLI shims + isolated gitconfig + directory verification QUEUED (M) |
| Worker env vars carry a `DSH_SUPERAGENT_` prefix and deliberately AVOID credential-shaped names (`KEY\|PASSWORD\|SECRET\|TOKEN`) because the host strips those from child environments | `src/infra/environment.ts` header | ADOPTED (held) — the MARFIM_* contract follows exactly this rule |
| Read-resource surface: paged (4,000 UTF-16 units default / 8,000 max; lists 20/50), executor grammar filters to `executor: true` resources, pages OMIT ids of what it cannot read | `src/app/resources.ts`, `tool.ts` readTool | QUEUED (S) |
| Prompt assembly order: shared+role → team sections → subscriptions+replies → dispatch/work → admission → handoff → environment → policy LAST (so a Policy change leaves all preceding text byte-identical); saved prompt config capped (`promptMaxChars` 100k); validation never truncates | `src/app/prompts.ts`, `docs/004-prompts.md` | PARTIAL — Marfim prompt sections are ordered + byte-stable (docs-as-tests); the policy-LAST ordering + prompt size budget QUEUED (S) |
| Prompt versioning bound to sessions: every request binds the current revision; sessions may read ONLY revisions they ran under; overridden defaults surface "Default updated" diffs; the prompt config itself is versioned snapshots of 15 built-in parts (`runtime/*`, `skill/feishu`, `skill/github` with required `{variables}`/`{version}`/`{mode}`/`{rules}` placeholders) + three team sections + arbitrary team skills (`PromptConfigSchema`/`PART_KEYS`) | `src/app/prompts.ts`, `src/app/sessions.ts` promptFor/mayRead | QUEUED (M) |
| Skills registered per prompt revision into each session's skill scope | `src/app/sessions.ts` bindSkills | PARTIAL — skills load per-session already; revision binding QUEUED with prompt versioning |
| Models as spec enforcement: `tests/model-surface.spec.ts` checks the ENTIRE model-facing surface (tools/prompts/skills/inputs) within a size budget per role | docs/007 "Tests" | QUEUED (S) — a docs-as-test over the prompt+tool budget (Marfim already count-pins 41 tools / 67 commands) |
| IM word limits enforced at TRANSPORT (Feishu 100 words); markdown-to-rich-text transport | `src/infra/text.ts` IM_WORDS | ADOPTED (doctrine — bound outputs at the transport edge); channel transports QUEUED with sources |
| GitHub reply surface: issue/PR comments + review-thread replies via GraphQL endpoints (not `pr comment`); PR binding by branch-name convention (`agent/` prefix + ticket short name) claimed atomically | `src/sources/github-channel.ts`, `github-pull.ts` claimPull | QUEUED (M) — with the GitHub source channel |
| Panel: "does anything need me now?" first; every element answers one of three questions or is removed; honesty rules (a count equals the length of the list it links to; healthy states take no space; destructive actions confirm; internal ids only in folded details) | `docs/005-panel.md` | ADOPTED (doctrine — the TUI why-panel + docs honesty rules follow these); the web panel QUEUED (L) with the gateway UI wave |
| Backend API hardening: loopback-only + same-origin enforcement + header marker, 128 KiB body cap, one status per failure kind | `src/app/http.ts` | PARTIAL — gateway is token-gated with Host/Origin allowlists + bounded bodies; the loopback-only profile QUEUED (S) |
| SETUP as an agent playbook: probe first (versions, identity, existing profile), ≤9 one-question steps with recommended answers, single plan message, operator-only secret steps, then install via management API + verification handover with three operator rules | `SETUP.md` | QUEUED (S) — Marfim's onboarding doc form |
| Service script: tmux session per profile, exit codes separating runtime vs component vs waiting-for-setup, manual-stop marker, 5-min graceful stop, log retention (20 newest), refusal of duplicates; driven by an hourly check agent | `scripts/dsh-superagent` | REJECTED for now — single-binary doctrine; the lifecycle concerns are covered by cron + delegation + the effect queue; the EXIT-CODE taxonomy is the reusable part (captured) |
| Locale en/zh plugin meta + panel glossary | `locale/*.json` | REJECTED — single-language honest |

---

## §ECC Everything Claude Code (2.2.3, `b38be6a7`→`d2ef2680`) — ULTRA-DEPTH DISTILLATION

Full extraction from `docs/forensic/ecc.md` (§1–§12 there), beyond the
delta scan already ledgered in §CC. W10.1 ADOPTED the ECC learning stack
(instincts, memory vault, skills curator, memory caps, review fork); this
section decision-ledgers the REST of the "harness OS" discipline: the
enforcement layer (hooks), the quality loop (plan→test→implement→review→
verify→remember→improve), and the portability doctrine.

### §ECC-A The hook/enforcement layer

| Spec | Source | Decision |
|---|---|---|
| Hooks are the enforcement layer, not prompts: exit 0 continue / exit 2 block (PreToolUse), stderr warns, stdout carries decisions; deterministic checks OUTSIDE the model | `hooks/README.md:191-226` | ADOPTED (already held) — INV-09 PreToolUse: JSON on stdin, `{"decision","reason"}`, non-zero exit or failure BLOCKS the tool (fail-closed) |
| Hook metadata SIDECAR: stable hook IDs + descriptions + fingerprints; must never drift from `hooks.json`; CI validates | `hooks/hooks.metadata.json`, `scripts/ci/validate-hooks.js` | QUEUED (S) — a hooks manifest with IDs + fingerprints + a drift test |
| Hook dispatchers: ONE bootstrap + one process for all pre-Bash and all PostToolUse hooks (latency) | `scripts/hooks/pre-bash-dispatcher.js` | MOOT (held better) — Marfim hooks run in-process per call; there is no process-spawn cost to batch away |
| `ECC_HOOK_PROFILE = minimal\|standard\|strict` + `ECC_DISABLED_HOOKS`/`ECC_HOOKS_ENABLED` + per-hook input caps and idle timeouts with named fail-closed hooks | `hooks/README.md:108-169` | PARTIAL — hooks are config-listed with timeouts (`timeout_ms`); named profiles QUEUED (S) |
| GateGuard fact-forcing: DENY the first Edit/Write per file until concrete facts (importers, schemas, verbatim instructions) are gathered; sensitive targets (secrets, auth/payment, migrations, CI) ALWAYS gated, including in subagents; A/B-tested +2.25 quality ("self-evaluation doesn't work; investigation creates context") | `skills/gateguard/SKILL.md`, `hooks.metadata.json:46-49` | QUEUED (S) — highest-value ECC row left: a permission-engine rule class (per-file first-write gate + always-gated sensitive paths) over the held hooks + floors |
| Config-protection: BLOCKS edits to existing linter/formatter configs (eslint/prettier/biome) so agents fix code, not the rules — "the agent may not weaken its own guardrails" | `scripts/hooks/config-protection.js` | QUEUED (S) — a deny-floor pattern for harness/config files (extends exec-policy self-tests doctrine) |
| Hookify: project rule files (`.claude/hookify.*.local.md`) evaluated before shell/file tools and prompts; GIT-TRACKED rule files DISABLED BY DEFAULT (opening a repo must not activate repo-authored instructions); symlinked/oversized/malformed skipped; 25 ms regex timeout | `hooks/README.md:180-189` | QUEUED (S) — the trust rule (repo-authored rules inert until trusted) matches Marfim's trust.rs doctrine; 25ms regex timeout is the honest bound |
| Commit-quality gate: staged-file lint, commit-message format, console.log/debugger/secret scan — can block | pre-bash-dispatcher consolidated checks | PARTIAL — deny.toml + CI gates held; a pre-commit hook rule QUEUED (S) |
| Dev-server blocker: `npm run dev` outside tmux blocked; tmux reminder; git-push reminder | `scripts/hooks/` | QUEUED (S) — example hook rules, ship as a documented recipe set |
| MCP health check: calls to unhealthy servers BLOCKED (fail-closed); PostToolUseFailure marks failed servers unhealthy + reconnects | `scripts/hooks/mcp-health-check.js` | PARTIAL — MCP client is fail-closed per call; the health LEDGER (mark unhealthy, block until reconnect) QUEUED (S) |
| Suggest-compact: nudge manual compaction every ~50 tool calls | pre:edit-write:suggest-compact | QUEUED (S) — cheap QoL over the held compaction trigger |
| Stop-time BATCHED format + typecheck (moved off per-Edit for latency) + console.log audit | `stop-format-typecheck.js`, `check-console-log.js` | QUEUED (S) — the batching-at-Stop lesson rides the formatter wave (§OC-H) |
| PreCompact saves state before compaction; SessionEnd marker | `pre-compact.js`, `session-end-marker.js` | PARTIAL — the journal IS the pre-compaction state; an explicit snapshot event QUEUED (S) |
| Desktop notify (macOS/WSL) at Stop | `stop:desktop-notify.js` | QUEUED (S) — TUI wave |
| Governance capture (opt-in env) of secrets/policy/approval events | `pre:governance-capture.js` | QUEUED (S) |

### §ECC-B The harness discipline (prompt/rules layer)

| Spec | Source | Decision |
|---|---|---|
| Core loop `plan → test → implement → review → verify → remember → improve`; maxim "Optimize the context window. Persist everything else." | `README.md:134-140` | ADOPTED (doctrine — Marfim's REA retention + compaction + instincts/curator ARE this loop; the maxim is quoted in DESIGN) |
| Planning gate: `/plan` restates requirements, assesses risks, writes a phased plan, WAITS for user confirm before touching code | `rules/common/development-workflow.md` step 1 | PARTIAL — EnterPlanMode held; the plan-file + confirm-gate contract QUEUED (S) |
| Research-first as a RULE: GitHub code search (`gh search`) → vendor docs → web research LAST; registry checks; prefer adopting/porting proven implementations over net-new | step 0, `skills/search-first` | QUEUED (S) — a default workflow rule + skill (the adopt/extend/build decision ladder); Marfim's own distillation ledger is the in-repo precedent |
| TDD as a gated loop: 80% minimum coverage, unit+integration+E2E required, RED→GREEN→REFACTOR mandatory; plan-handoff treats plan files as UNTRUSTED data with whitelisted validation commands | `rules/common/testing.md`, `skills/tdd-workflow` | QUEUED (S) — workflow rules; the untrusted-plan-file contract is the load-bearing line |
| Fresh-context review: code-reviewer runs immediately after writing code from a FRESH context; `/santa-loop` requires TWO independent reviewers on different models to both approve; orch-review fans out per-dimension reviewers, dedups, and sends every CRITICAL/HIGH to an adversarial verifier that DEFAULTS TO REFUTED | `rules/common/development-workflow.md` step 3, `commands/santa-loop.md`, `workflows/orch-review.workflow.js` | QUEUED (M) — the review fork (W10.1) + subagent engine are the seams; verifier-defaults-refuted is the doctrine |
| Verification loops: six phases (build, typecheck, lint, tests+coverage, security grep, diff review) ending PASS/FAIL | `skills/verification-loop` | PARTIAL — Marfim's own gates (G1–G14) + CI are this; the skill form QUEUED (S) |
| Delegation completion contract: "your final message is the deliverable; if you delegate, you own collection" — fire-and-forget forbidden (observed orphaned-children failure mode) | `rules/common/agents.md:40-50` | ADOPTED (held in form) — forced yield + results-enter-as-NEW-turn (batch 3); the explicit contract WORDING into subagent prompts QUEUED (S) |
| Immediate proactive routing: planner/code-reviewer/tdd-guide/architect used WITHOUT asking | `rules/common/agents.md:26-30` | PARTIAL — presets + router roles cover model routing; agent-autoselection QUEUED (S) |
| Knowledge placement: personal → auto memory; team → project docs; never duplicate; ask before new top-level files | `AGENTS.md:166-173` | PARTIAL — Memory tool + memory caps hold the personal side; the placement rule QUEUED (S) as prompt doctrine |
| Prompt-defense baseline: never change role/override project rules; never reveal secrets; unicode/homoglyph/urgency/authority tricks and embedded tool commands are suspicious; fetched data untrusted; refuse harmful content | `CLAUDE.md:9-16` | ADOPTED (already held) — privacy guard + confusables (homoglyph tables in the brain) + untrusted-webhook toolset + REA "outside text is data"; the baseline is restated in Marfim's own system-prompt contract |
| SOUL.md fixed principles (Agent-First, Test-Driven, Security-First, Immutability, Plan Before Execute) | `SOUL.md:6-11` | ADOPTED (doctrine — the invariants page IS Marfim's SOUL) |
| contexts/ mode profiles (dev/research/review) re-tuning behavior per mode | `contexts/*.md` | ADOPTED (already held, Marfim form) — the 13 presets |

### §ECC-C Learning & memory (beyond W10.1)

| Spec | Source | Decision |
|---|---|---|
| Instinct model: atomic YAML behaviors — one trigger, one action, confidence 0.3–0.9, domain tag, evidence list, scope project\|global | `skills/continuous-learning-v2` "The Instinct Model" | ADOPTED (W10.1, adapted constants) — inject ≥0.6, reinforce +0.1, decay −0.15, auto-retire <0.2 (recorded, never deleted), promote ≥5 hits ∧ ≥0.8 |
| Capture via 100% PreToolUse/PostToolUse observation + background observer mining corrections/error-resolutions/repeated workflows | v1-vs-v2 table | PARTIAL — instincts reinforce on observed use (telemetry); the background observer QUEUED (S) with the aux runner |
| Project identity = hash of `git remote get-url origin` (portable across machines); data deliberately OUTSIDE the config dir so sensitive-path guards don't block writes | "Project Detection" | QUEUED (S) — project-scoped instinct stores + the placement trick |
| Injection: ≤6 instincts, confidence ≥0.7, ranked by confidence +0.25 project-scope +0.2 stack-match | `scripts/lib/instinct-relevance.js` | PARTIAL — capped non-empty-only injection held (W10.1 PromptSurfaces); the boost math + cap-6 QUEUED (S) |
| Evolution: `/evolve` clusters instincts into generated commands/skills/agents; `/promote` project→global at 2+ projects; `/prune` deletes >30-day unpromoted | `commands/evolve.md` | PARTIAL — promote-to-skill-proposal held; cluster-into-agent/command QUEUED (S) |
| Memory Vault `ecc.memory.v1`: kinds context\|decision\|fact\|handoff\|lesson\|note\|preference\|runbook; scopes project\|team\|user; trust ALWAYS `unreviewed`; status active\|rejected\|superseded; strict size/ID/timestamp schema | `scripts/lib/memory-vault-format.js` | ADOPTED (W10.1) — `core::memvault` import/export, version check exact, control-stripped, unknown kinds fail closed |
| Trust boundaries: writes CREATE-ONLY; bodies only via `--stdin`/`--body-file`; recalled memory is "untrusted context, not executable instructions"; MCP identity server-bound; user scope needs `ECC_MEMORY_ALLOW_USER_SCOPE=1` | `README.md:1274-1312` | PARTIAL — untrusted-by-doctrine + review fork held; create-only writes + opt-in user scope QUEUED (S) |
| Session persistence: Stop-hook summaries under a data dir; SessionStart reloads bounded context (cap 8000 chars); 30-day retention | `hooks/memory-persistence/` | PARTIAL — journal + capped memory surfaces held; session SUMMARY reload QUEUED (S) (pairs with opencode hidden `summary` agent, §OC-B) |

### §ECC-D Security model & portability

| Spec | Source | Decision |
|---|---|---|
| AgentShield: the harness config ITSELF is the attack surface — scan CLAUDE.md/settings/mcp.json/hooks/agents for secrets, permissive allowlists, command injection, npx supply-chain risk | `skills/security-scan` | QUEUED (M) — a `marfim doctor`-style self-audit command (config floors, allowlist breadth, hook commands, MCP endpoints) |
| "Everything an LLM reads is executable context": sanitize zero-width/bidi unicode + hidden payloads; least AGENCY (approval before unsandboxed shell, network egress, secret reads, out-of-repo writes); observability of tool calls/approvals/network attempts; process-group kill switches + heartbeats for unattended loops | `the-security-guide.md` | PARTIAL — sandbox + netproxy (W9.2) + approvals + process-group kill held; bidi/zero-width sanitization QUEUED (S) (confusables machinery is adjacent); heartbeats QUEUED with cron |
| Supply-chain: installer runs `npm install --ignore-scripts` (preinstall RCE block); official-sources-only; IOC/supply-chain watch tests | `install.sh:20-24`, `tests/ci/` | ADOPTED (doctrine — deny.toml + vendored-source-verification doctrine + G-gates); Marfim has no installer to harden |
| Honest capability matrix per harness with issue-linked defects | `README.md:1318-1336` | ADOPTED (already held) — FEATURE-MATRIX + honest tiers + BLOCKED rows with reasons |
| Documented curation profile: `pi/core/CURATION.md` lists every excluded skill with its reason | `pi/core/` | ADOPTED (already held, Marfim form) — PORT-LEDGER's honest "narrowed" columns |
| `agent.yaml` gitagent export (171 skills + 98 commands with preferred/fallback models) | `agent.yaml` | REJECTED for now — no foreign import target; memvault + SKILL.md are the portable surfaces |
| 17 cross-harness installer targets, tiered stable→instruction-only | `scripts/lib/install-targets/` | REJECTED — single static binary; cross-harness portability = the vault format + skills format, honestly stated |
| Skill portability gate: fail Claude-only substitutions (`${CLAUDE_PLUGIN_ROOT}`), restrict frontmatter keys on projections | `skills/skill-host-compat` | QUEUED (S) — a skills lint (frontmatter key allowlist + no-env-substitution) |
| Install modules/profiles + idempotent install-state + `ecc doctor/repair/uninstall` (managed files only) | `manifests/`, `COMMANDS-QUICK-REF` | MOOT — single binary; the doctrine (managed-files-only uninstall) is captured for any future installer |
| Skills-first migration: skills are the canonical surface, commands demoted to compatibility shims, retired commands mapped to maintained skills | `AGENTS.md:175-180` | ADOPTED (held by construction) — Marfim started skills-first; 67 commands are the compatibility surface |
| Eval-driven development: capability/regression evals with code/model/rule/human graders; pass@k vs pass^k metrics | `skills/eval-harness` | PARTIAL — differential fixtures + gates held; pass@k eval kernels QUEUED (M) |
| Context economics everywhere: SessionStart 8000-char cap, ≤6 instincts, strategic-compact suggestions, token budgets | `README.md:1383-1401` | PARTIAL — caps held on every prompt surface (memcaps 2200/1375, bounded instinct injection); the total-prompt-budget test QUEUED (S) (dsh model-surface line, §DSH-D) |
| Native Workflow pilot: deterministic background fan-out with dedup-before-verify barriers | `workflows/orch-review.workflow.js` | QUEUED (L) — job/workflow wave over the delegation ledger |
| PR moderation (burst guard): event-anchored excess selection, grandfathering, reopen-consumes-slot, queue:max with honest 100-run limit | delta scan (§CC) | QUEUED (§CC row — repo-automation wave) |
| Epic coordination commands (GitHub-issue-native decompose/validate/claim/sync/review/publish/unblock) | `commands/` | REJECTED for now — repo automation rides the same job-queue wave as PR moderation |
| Multi-model collaborative development (multi-plan/workflow/backend/frontend/execute) | `commands/` | PARTIAL — router roles + failover held; deliberate multi-model debate QUEUED (L) |
| PRP chain prd→plan→implement→commit→pr | `commands/` | QUEUED (S) — command-chain recipes |
| GAN loops (planner/generator/evaluator with bounded iterations, Playwright scoring) | agents + commands | REJECTED for now — the eval-kernel + verifier doctrine is the reusable part (captured) |
| Per-language reviewer/build-resolver agent roster (71 agents: rust/go/python/react/… reviewers + build resolvers) | `agents/*.md` | PARTIAL — skills + presets cover the model side; the ROSTER as shippable markdown agent definitions QUEUED (S) (§OC-H custom agents line is the mechanism) |
| Model-route per task (Haiku/Sonnet/Opus) | `commands/model-route` | ADOPTED (already held) — router roles + effort ladder |
| Token/cost reporting commands | `cost-report`, `token-card` | PARTIAL — Usage Ledger held; report commands QUEUED (S) (§OC-G stats line) |

---

## §FORMER-REJECTS — adoption forms (all default OFF unless stated)

Every REJECTED row above still ships: each conflicts with an invariant or a
scope line only in its original form, so it ships in the adoption form below
(default-off behind a flag). Nothing stays rejected; nothing ships in a form
that violates an invariant.

| Original rejection | Adoption form (default state) |
|---|---|
| Juice trick (`# Juice: N` prompt hack) | per-model compat quirk `openai.juice_off` (off) |
| Responses `previous_response_id` chaining | `responses.chain = off\|hint`; hint sends the id only as an optimization, journal replay stays truth, any stale-chain error falls back to stateless replay (off) |
| Custom grammar tools (`apply_patch` grammar, `custom_tool_call_input.delta`) | `custom` tool kind on the Responses wire only with a function-tool fallback (off) |
| Vertex family | `google-vertex` preset via CredentialSource (off) |
| In-place key rotation | credential pool with per-request immutable snapshots — never mutate a published context (off) |
| Service tiers | validated per-family config, default unset |
| KDL cascade / 8.7k-model catalog | TOML compat rules, no KDL dependency |
| Guardian-LLM approval | opt-in flag (off) |
| Human-wait accounting | opt-in flag (off) |
| Snapcompact bitmap frames | opt-in flag (off) |
| CodeMode (`Code` tool interpreter) | opt-in flag (off) |
| tree-sitter frontends | cargo feature `treesitter` (off) |
| shell-vm (safepoint interpreter on desktop) | opt-in flag (off); W14 keeps it default-ON for Android only |
| Safepoint VM / uutils / obash | W14 embedded shell (default ON for Android, OFF on desktop) |
| `--brief` SendUserMessage | an Event variant, journaled and routed through the steering/approval channel; on only in headless `--brief` |
| Privacy-mod disable channels | only the managed-settings tier can disable the guard; config-off (`accountEmail` empty) stays the documented switch |
| Panic containment (catch_unwind wrapper) | catch_unwind at the FFI boundary only (marfim-ffi), core stays forbid(unsafe) |
| ZDR store:false | already moot — Marfim is always `store:false` |
