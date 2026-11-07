# Progress ledger

**Single source of truth** for work-package status and the live test count.
Statuses: TODO / WIP / DONE / BLOCKED. Each row's commit is the commit that
landed the work (the in-flight row says `this commit` and is back-filled by
the next ledger update).

**Live test count: 603** (`cargo test --workspace`, 0 failed — measured at
dc980ef re-run; every committed change re-lands it green). Historical counts
in older CHANGELOG entries (475/491/578) were true at their commits.

Owner inputs required: see docs/OWNER-INPUT.md. A row is BLOCKED only when
an owner input is missing; the reason and the exact ask live in that file.

| ID | Status | Commit | Evidence | Notes |
|---|---|---|---|---|
| W0.1 | DONE | this commit | docs/PROGRESS.md + OWNER-INPUT.md created; ROADMAP rewritten from DISTILLATION; README/AGENTS count fixed to ledger-source; FEATURE-MATRIX dup paragraph removed, worktree row ✅, §0/§7 contradiction fixed, stale PARITY ref fixed; DISTILLATION dangling §M ref fixed | every "already held" DISTILLATION claim re-verified against code (retry-before-first-byte, deny-before-mode, headless fail-closed, trust store, worktree isolation) |
| W0.2 | DONE | this commit | `git ls-remote --tags origin` → only v1.0.0; no `archive/*` tag or Node tree reachable | W13 BLOCKED (owner input: oracle tag); PORT-LEDGER carries the not-regenerable note |
| W0.3 | DONE | this commit | scripts/gates.sh gains G11/G12/G13/G14 with honest SKIP when artifacts absent; G1–G10 unchanged | G11 protocol-fixture contract activates at W2.2; G12/G13 at W4; G14 at W4.1 |
| W1.1 | DONE | this commit | cargo test --workspace 603 green; clippy -p marfim-app -p marfim -D warnings clean; bin main.rs 1596→1013 lines, lib extracted (settings + factory) | zero behavior change; bin keeps clap+tui+slash; reqwest/futures-util dead deps dropped from the bin |
| W1.2 | TODO | — | — | core::platform trait |
| W1.3 | TODO | — | — | CredentialSource trait + precedence chain |
| W1.4 | TODO | — | — | SandboxLevel android arms + marfim-exec bin |
| W1.5 | TODO | — | — | GitBackend trait (CliGit + GixGit) |
| W2.1 | TODO | — | — | gateway protocol routes + SSE envelope |
| W2.2 | TODO | — | — | docs/PROTOCOL.md + fixtures/protocol/*.json + Dart contract tests |
| W2.3 | TODO | — | — | unix-socket + optional TLS (rustls) |
| W3.1 | TODO | — | — | marfim-term: PTY + Rust terminal emulator + routes |
| W3.2 | TODO | — | — | Terminal tool; monitor migration |
| W4.1 | TODO | — | — | marfim-ffi C ABI (G14 activates) |
| W4.2 | TODO | — | — | apps/flutter android project + gradle cargo-ndk |
| W4.3 | TODO | — | — | Flutter app modules + docs/DESIGN.md |
| W4.4 | TODO | — | — | .github/workflows/mobile.yml + docs/ANDROID-FINDINGS.md |
| W4.5 | BLOCKED | — | — | alpha gate needs: emulator run (CI), signing secrets (owner), live API key (owner) — see OWNER-INPUT.md; ships when W4.1–W4.4 land and CI runs |
| W5.1 | TODO | — | — | hashline edit anchors |
| W5.2 | TODO | — | — | read summarization via selector grammar |
| W5.3 | TODO | — | — | tree-sitter frontends (feature treesitter) |
| W5.4 | TODO | — | — | generic tool-output bounding |
| W5.5 | TODO | — | — | mangled tool-name recovery, identical-call cap, turn budgets |
| W5.6 | TODO | — | — | per-turn tool-spec plan + approval tiers |
| W5.7 | TODO | — | — | chunk-fed grep, dir-listing cache, bounded exec pool |
| W5.8 | TODO | — | — | CoW isolation ladder probe |
| W5.9 | TODO | — | — | CodeMode (opt-in `Code` tool, restricted interpreter) |
| W6.1 | TODO | — | — | marfim-state (feature sqlite): WAL policy, write core, schema, projections, FTS5 |
| W6.2 | TODO | — | — | durable inbox sessions + receipts + leases |
| W6.3 | TODO | — | — | session tree (parentId, leaf, UUIDv7) |
| W6.4 | TODO | — | — | trajectory compressor |
| W7.1 | TODO | — | — | model capability registry TOML + adaptive thinking + thought signatures |
| W7.2 | TODO | — | — | cache-TTL plumbing (retention, TTL-reject ladder, cachedContent, warming) |
| W7.3 | TODO | — | — | aux failover ladder + health TTLs |
| W7.4 | TODO | — | — | OAuth2 PKCE + credential pools |
| W7.5 | TODO | — | — | wires: Bedrock/Vertex/Mistral/custom-grammar (cassettes) |
| W8 | TODO | — | — | context: cut-point rules, defrag counter, skill-prune markers, shake, snapcompact (opt-in), promotion, async compaction, ordered strategies, snapshot-once |
| W9.1 | TODO | — | — | Starlark execpolicy (feature starlark) + amendment rules |
| W9.2 | TODO | — | — | network enforcement proxy |
| W9.3 | TODO | — | — | inspector chain + LLM judge + guardian (7.3 budget) |
| W9.4 | TODO | — | — | hardline Windows tier, deny-and-steer, coalesced approvals, secret scoping, staged writes, observe mode, seccomp, managed tier |
| W9.5 | TODO | — | — | browser SSRF floors + CDP tools (feature gated) |
| W10.1 | TODO | — | — | memory caps + review fork + curator + instincts + vault |
| W10.2 | TODO | — | — | mandates, effect queue/outbox, source plugins, cron, delegation ledger |
| W11 | TODO | — | — | surfaces: LSP diagnostics, DAP, ACP completion, TUI upgrades, recipes, testkit cassettes, MCP pinning/backgrounding, wire breadth |
| W12 | TODO | — | — | evidence records, obligation ledger, unknowns ledger, REA bridge |
| W13 | BLOCKED | — | — | oracle (~40k LOC LLM-coupled intelligence) not reachable: no archive tag on origin (see W0.2); BLOCKED on owner input ORACLE_TAG |
| W14 | TODO | — | — | embedded shell (ShellBackend trait, in-process interpreter, obash-style differential suite) |
| FORMER-REJECTS | TODO | — | — | every DISTILLATION REJECTED row ships as a default-off flag per the mission adoption forms (list maintained in DISTILLATION.md FORMER-REJECTS section) |

## Phase-end gate record

| Phase | fmt | clippy -D warnings | test --workspace | gates.sh | deny/audit | Recorded |
|---|---|---|---|---|---|---|
| baseline (dc980ef) | ✓ | ✓ | 603 green | G1–G10 PASS | ✓ (CI) | W0.1 |
