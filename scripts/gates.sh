#!/bin/sh
# P8 gate battery: run from the repo root. Exit non-zero on the first
# failed gate; every gate prints evidence lines. Each gate maps to the
# phase spec (G1 hygiene, G2 no-silent-mocks, G3 loop invariants, G4 wire
# compat, G5 sandbox fail-closed, G6 determinism, G7 no placeholders,
# G8 no Node/Python, G9 version/license/docs consistency, G10 release
# artifacts).
set -u
export CARGO_INCREMENTAL=0
PASS=0; FAIL=0
ok()  { echo "PASS G$1: $2"; PASS=$((PASS+1)); }
bad() { echo "FAIL G$1: $2"; FAIL=$((FAIL+1)); }

# G1 + G7 live as the enforced hygiene test; run it explicitly.
if cargo test -p marfim-core --test source_hygiene 2>/dev/null | grep -q "test result: ok"; then
  ok "1/G7" "no unwrap/expect/panic/todo/unimplemented in non-test src (source_hygiene)"
else
  bad "1/G7" "hygiene test failed"
fi

# G2: the mock wire is reachable ONLY through the explicit `mock` preset.
if rg -n '"mock" =>' crates/marfim/src/settings.rs >/dev/null 2>&1; then
  if [ -z "$(rg -n "fallback.*mock|default_provider.*mock" crates/ -g '*.rs' 2>/dev/null | grep -v test || true)" ]; then
    ok 2 "mock wire only via explicit selection (D11); no silent fallback path"
  else
    bad 2 "possible silent mock fallback found"
  fi
else
  bad 2 "mock preset missing"
fi

# G3: the loop-invariant suite (INV-01..10, D1..D24)
if cargo test -p marfim-core --lib loop_ 2>/dev/null | grep -q "test result: ok"; then
  ok 3 "loop invariant + D-suite green"
else
  bad 3 "loop tests failed"
fi

# G4: wire-golden differentials
if cargo test -p marfim-core --test differential 2>/dev/null | grep -q "test result: ok"; then
  ok 4 "anthropic/openai wire goldens replay"
else
  bad 4 "wire differential failed"
fi

# G5: sandbox fail-closed + policy battery
if cargo test -p marfim-core sandbox 2>/dev/null | grep -q "test result: ok"; then
  ok 5 "sandbox refusal + containment + policy battery green"
else
  bad 5 "sandbox tests failed"
fi

# G6: determinism — every differential fixture suite replays
if cargo test -p marfim-brain 2>/dev/null | grep -qE "test result: ok"; then
  ok 6 "oracle differential fixtures replay (byte-exact/<=1ulp)"
else
  bad 6 "differential suite failed"
fi

# G8: zero Node/Python in the tree
STRAYS=$(git ls-files | grep -E '\.(py|js|ts)$|package\.json$' || true)
if [ -z "$STRAYS" ]; then
  ok 8 "no .py/.js/.ts/package.json tracked"
else
  bad 8 "stray files: $STRAYS"
fi

# G9: version/license/docs consistency
V=$(grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2)
L=$(grep -m1 '^license = ' Cargo.toml | cut -d'"' -f2)
BINVER=$(./target/debug/marfim --version 2>/dev/null || echo "build-first")
HITS=$(rg -l "\(PARITY\.md\)|see PARITY|python3 -m unittest" README.md AGENTS.md docs/ 2>/dev/null | grep -v PORT-LEDGER.md || true)
if [ "$V" = "1.0.0" ] && [ "$L" = "MIT" ] && echo "$BINVER" | grep -q "marfim 1.0.0" && [ -z "$HITS" ]; then
  ok 9 "workspace 1.0.0 + MIT + binary agrees + no stale doc refs"
else
  bad 9 "V=$V L=$L BIN=$BINVER stale=$HITS"
fi

# G10: release artifact
cargo build --release -p marfim 2>/dev/null
if [ -f target/release/marfim ]; then
  SIZE=$(stat -c%s target/release/marfim)
  VREL=$(./target/release/marfim --version)
  if [ "$SIZE" -lt 26214400 ] && echo "$VREL" | grep -q "1.0.0"; then
    ok 10 "release binary ${SIZE} bytes (<25MB), version agrees"
  else
    bad 10 "size=$SIZE version=$VREL"
  fi
else
  bad 10 "release binary missing"
fi

echo "----"
echo "gates: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
