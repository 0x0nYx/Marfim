# Distillation Ledger — Oh My Pi + Hermes → Marfim

Sources scanned (latest builds, deep-scan of every module family):
- **Oh My Pi** `can1357/oh-my-pi` v18.18.0 — TS agent surface + ~763k-line Rust core (12 `pi-*` crates), 81 providers / 8,675 models in catalog.
- **Hermes** `NousResearch/hermes-agent` v0.23.0 (2026-11-05) — ~10.8k Python files, agent loop + aux failover stack + SQLite/WAL durable state.

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
| Anthropic adaptive thinking (`{type:"adaptive"}`) + per-model capability tables (fable/opus-5.5 mandatory-thinking etc.) | Hermes adapter | QUEUED — needs a per-model capability registry; see §M |
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

Every ADOPTED item is wired, bounded (explicit caps/clamps), observable
(events/journal/tests), reversible (pure functions + additive config), and
covered by tests in this commit.
