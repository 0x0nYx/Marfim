# AGENTS.md — instructions for coding agents working on Marfim

## Project
Marfim 1.0.0 is a clean-room unified coding-agent harness in Rust (a
Cargo workspace of six crates, `#![forbid(unsafe_code)]` everywhere,
MIT). Do not copy code from the projects analyzed in docs/forensic/ —
implement from the behavioral specs only.

## Commands
- Run tests: `cargo test --workspace` (475 green; keep it that way)
- Lint: `cargo clippy --workspace --all-targets -- -D warnings`
- Format: `cargo fmt --all`
- One binary: `cargo build --release` (size budget 25 MB, CI-enforced)

## Conventions
- G1 hygiene is TEST-ENFORCED: no unwrap/expect/panic!/todo!/
  unimplemented! in non-test source across every crate. Fallible paths
  return Result/Option; telemetry paths fail visible, never panic.
- Determinism doctrine: seeded RNG only (marfim-brain::util::SeededRng),
  no wall-clock in output-affecting paths, BTreeMap not HashMap where
  iteration order could leak.
- Differential ports: a marfim-brain module counts as ported ONLY when
  node-generated JSONL fixtures replay through it
  (docs/PORT-LEDGER.md). Regenerate fixtures with
  /home/z scripts pattern: scripts/fixture-*.js against the preserved
  oracle tree; never hand-write oracle outputs.
- Sealed eval sets are never used for tuning.
- Every mutation flows through the PermissionEngine; the sandbox is
  fail-closed (an unenforceable sandbox REFUSES to spawn).
- Faculty hooks are observational except pre_tool (deny with reason),
  pre_compaction (bounded protection) and idle_propose (propose-approve).
- Session state lives in the journal; --resume rebuilds history from it.
- Keep the memory of this file: version/license/CI/docs must stay
  consistent in the same commit that changes any of them.
