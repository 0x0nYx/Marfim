# Marfim Roadmap

Rewritten from docs/DISTILLATION.md (the spec ledger): every QUEUED/PARTIAL
row below carries its spec source. Shipped items are NOT listed here — they
live in docs/FEATURE-MATRIX.md and docs/PROGRESS.md. Work-package IDs refer
to the progress ledger.

## v0.2 — embeddability + protocol (W1/W2/W3, critical path)

- **marfim-app lib extraction, platform trait, CredentialSource,
  android sandbox arms, GitBackend (gix)** — the embeddability substrate
  (W1.1–W1.5).
- **Protocol v1**: complete the gateway surface (sessions, steer/cancel/
  undo/rewind/fork, config with redaction, fs/git/MCP/terminal routes),
  SSE envelope with seq + Last-Event-ID ring + heartbeat,
  docs/PROTOCOL.md, byte-checked fixtures, unix-socket + optional TLS
  (W2.1–W2.3).
- **Terminal**: PTY + Rust emulator (vt100 first, alacritty_terminal
  fallback) behind marfim-term; `Terminal` tool; monitor migration (W3,
  SHIPPED — see PROGRESS.md).

## v0.3 — mobile (W4/W14)

- **marfim-ffi** C ABI owning the tokio runtime; **apps/flutter** (Android
  only, cargo-ndk, foreground service, secure secret channel); Flutter UI
  modules per docs/DESIGN.md; mobile CI with emulator suite (W4.1–W4.4);
  alpha gate (W4.5) — BLOCKED on owner inputs 1/2/4.
- **Embedded shell** (W14): ShellBackend trait, in-process interpreter on a
  workspace VFS with safepoint budget, obash-style differential suite vs
  /bin/sh; default Embedded on Android, Process on desktop.

## v0.4 — edit/tools/loop upgrades (W5, from DISTILLATION §S/§X/§A)

- Hashline edit anchors `[PATH#TAG]` with blake2b digests (OMP pi-edit,
  −61% tokens measured) (W5.1).
- Read summarization driven by the selector grammar (W5.2).
- tree-sitter frontends for repomap/AstQuery (feature `treesitter`) (W5.3).
- Generic tool-output bounding with backing file + retention (W5.4).
- Mangled tool-name recovery + identical-call cap + turn budgets
  (goose tool_monitor) (W5.5).
- Per-turn pure tool-spec plan + tool-declared approval tiers (codex/omp)
  (W5.6).
- Chunk-fed grep, stamp-based dir cache, walker discipline, bounded exec
  pool (OMP pi-walker) (W5.7).
- CoW isolation ladder with probe (OMP pi-iso) (W5.8).
- CodeMode restricted interpreter (opencode codemode; starlark vs rhai by
  measured spike) (W5.9).

## v0.5 — durable state (W6, from DISTILLATION §W)

- marfim-state (feature `sqlite`): WAL policy module, BEGIN IMMEDIATE write
  core, schema+migrations, deterministic journal projections, maintenance,
  background usage writer, FTS5+trigram behind the sessionsearch interface
  (W6.1). Durable inbox + crash settlement + receipts + leases (W6.2).
  Session tree with fork + branch summaries + UUIDv7 (W6.3). Trajectory
  compressor (W6.4).

## v0.6 — providers (W7, from DISTILLATION §R/§F/§P)

- Model capability registry + compat rules as TOML data; adaptive thinking;
  thinking replay fingerprint ledger; Gemini thought signatures (W7.1).
- Cache-TTL plumbing: prompt_cache_retention, Anthropic TTL-reject ladder,
  Gemini cachedContent lifecycle, cache-aware idle compaction (W7.2).
- Aux failover ladder (parameter-strip rungs, credential refresh, quarantine
  walk) + health TTLs 600s/60s + capability-scoped recovery (W7.3).
- OAuth2 PKCE + refresh pools via CredentialSource (W7.4).
- Wires: Bedrock (SigV4), Vertex, Mistral native, custom grammar tools —
  cassette-tested (W7.5).

## v0.7 — context (W8, from DISTILLATION §C/§R2/§CC)

- Cut-point never at a toolResult + split-turn prefix summary; defrag
  counter ≥2000 + 3-failure skip; skill-prune markers; shake artifact refs
  (protected 40k, ≥20k savings); snapcompact (opt-in); context promotion;
  async/background compaction with persisted checkpoints; ordered strategy
  list; system-prompt snapshot-once.

## v0.8 — permissions/safety (W9, from DISTILLATION §A/§G)

- Starlark execpolicy (feature `starlark`) replacing the deny-list;
  amendment-capable approvals (W9.1). Network enforcement proxy (W9.2).
  Deterministic inspector chain + LLM read-only judge + guardian smart
  approval on the W7.3 aux budget (W9.3). Hardline Windows tier,
  deny-and-steer, coalesced approvals with request_id/settle, task-scoped
  secret scoping, staged writes, observe mode, seccomp backend,
  managed-settings tier + identity() (W9.4). Browser URL/SSRF floors + CDP
  tools (W9.5).

## v0.9 — learning/autonomy (W10, from DISTILLATION §S/§G)

- Memory caps + background review fork + learning loop; skills telemetry/
  curator/audit; instincts; portable memory vault (ecc.memory.v1, treated
  as untrusted data) (W10.1). Mandates, effect queue + ordered outbox,
  source plugins, cron (5 NL forms, catch-up, per-job pinning), async
  delegation ledger, PR-burst-guard as repo automation (W10.2).

## v1.x — surfaces + evidence (W11/W12, from DISTILLATION §H/§CC/§G)

- LSP client diagnostics with staleness counters; DAP tool; ACP completion;
  TUI themes + interrupt-and-redirect + transcript divider; parametric YAML
  recipes; testkit HTTP cassettes; MCP legacy-handshake pinning + task
  backgrounding (opt-in); remaining wire breadth (W11).
- Evidence records (confidence/coverage), obligation ledger as
  definition-of-done gate, unknowns ledger, REA bridge (W12).

## Continuous

- Adopt every FORMER-REJECTS row as a default-off flag (see
  docs/DISTILLATION.md §FORMER-REJECTS).
- Keep every "already held" DISTILLATION claim verified by tests, not
  convention.
