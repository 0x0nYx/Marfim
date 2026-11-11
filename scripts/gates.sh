#!/bin/sh
# P8 gate battery: run from the repo root. Exit non-zero on the first
# failed gate; every gate prints evidence lines. Each gate maps to the
# phase spec (G1 hygiene, G2 no-silent-mocks, G3 loop invariants, G4 wire
# compat, G5 sandbox fail-closed, G6 determinism, G7 no placeholders,
# G8 no Node/Python, G9 version/license/docs consistency, G10 release
# artifacts; W0.3 adds G11 protocol fixtures, G12 mobile build+size,
# G13 emulator suite, G14 FFI hygiene). A gate whose artifact does not
# exist yet SKIPs with the activating ledger row — a SKIP is never a PASS.
set -u
export CARGO_INCREMENTAL=0
PASS=0; FAIL=0; SKIP=0
ok()  { echo "PASS G$1: $2"; PASS=$((PASS+1)); }
bad() { echo "FAIL G$1: $2"; FAIL=$((FAIL+1)); }
skip(){ echo "SKIP G$1: $2"; SKIP=$((SKIP+1)); }

# G1 + G7 live as the enforced hygiene test; run it explicitly.
if cargo test -p marfim-core --test source_hygiene 2>/dev/null | grep -q "test result: ok"; then
  ok "1/G7" "no unwrap/expect/panic/todo/unimplemented in non-test src (source_hygiene)"
else
  bad "1/G7" "hygiene test failed"
fi

# G2: the mock wire is reachable ONLY through the explicit `mock` preset.
if rg -n '"mock" =>' crates/marfim-app/src/settings.rs >/dev/null 2>&1; then
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

# G4: wire-golden differentials (anthropic, openai, gemini, responses)
if cargo test -p marfim-core --test differential 2>/dev/null | grep -q "test result: ok"; then
  ok 4 "anthropic/openai/gemini/responses wire goldens replay"
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

# G11: protocol-fixture contract — Rust regen must be byte-identical to
# fixtures/protocol/*.json (activates at W2.2; SKIP until then).
if [ -d fixtures/protocol ] && ls fixtures/protocol/*.json >/dev/null 2>&1; then
  if cargo test -p marfim-gateway --test protocol_fixtures 2>/dev/null | grep -q "test result: ok"; then
    ok 11 "protocol fixtures regenerate byte-identical ($(ls fixtures/protocol/*.json | wc -l | tr -d ' ') files)"
  else
    bad 11 "protocol fixture regen test failed"
  fi
else
  skip 11 "fixtures/protocol/ not shipped yet (W2.2)"
fi

# G12: mobile build + size budget — needs cargo-ndk + the android target
# (activates at W4.2; SKIP when the toolchain is absent, e.g. desktop CI).
if command -v cargo-ndk >/dev/null 2>&1 && rustup target list --installed 2>/dev/null | grep -q aarch64-linux-android && [ -d apps/flutter ]; then
  if cargo ndk -t arm64-v8a -o apps/flutter/android/app/src/main/jniLibs build --release -p marfim-ffi 2>/dev/null; then
    SO=apps/flutter/android/app/src/main/jniLibs/arm64-v8a/libmarfim_ffi.so
    SIZE=$(stat -c%s "$SO" 2>/dev/null || echo 0)
    if [ "$SIZE" -gt 0 ] && [ "$SIZE" -le 36700160 ]; then
      ok 12 "libmarfim_ffi.so ${SIZE} bytes (<=35MB budget)"
    else
      bad 12 "libmarfim_ffi.so size ${SIZE} over budget"
    fi
  else
    bad 12 "cargo-ndk build failed"
  fi
else
  skip 12 "android toolchain or apps/flutter absent (W4.2)"
fi

# G13: emulator instrumented suite (activates at W4.4; SKIP without adb+device).
if command -v adb >/dev/null 2>&1 && adb devices 2>/dev/null | grep -q "emulator\|device$"; then
  if [ -d apps/flutter ]; then
    (cd apps/flutter && flutter test integration_test 2>/dev/null | grep -q "All tests passed") && ok 13 "emulator instrumented suite green" || bad 13 "emulator suite failed"
  else
    skip 13 "apps/flutter absent (W4.4)"
  fi
else
  skip 13 "no adb device/emulator attached (W4.4)"
fi

# G14: FFI hygiene — marfim-ffi is the ONLY crate allowed unsafe: every
# unsafe block carries a SAFETY comment, every extern fn wraps
# catch_unwind, forbid(unsafe) stays everywhere else (activates at W4.1).
if [ -d crates/marfim-ffi ]; then
  if cargo test -p marfim-ffi --test ffi_hygiene 2>/dev/null | grep -q "test result: ok"; then
    ok 14 "FFI hygiene: SAFETY comments + catch_unwind + forbid-elsewhere green"
  else
    bad 14 "FFI hygiene test failed"
  fi
else
  skip 14 "crates/marfim-ffi not shipped yet (W4.1)"
fi

# G16: release-truth consistency -- workspace version == CHANGELOG top
# entry == README banner == the git tag. The tag leg SKIPs on a tree
# with no exact-match tag (local development); after the purge there is
# exactly one tag, v<version>, and the gate must be a full PASS.
# (G15 was specified for a standalone marfim-intel crate; the final
# architecture keeps the intelligence modules in marfim-brain, where G6
# enforces their fixture replay -- so the battery runs 1-14 + 16.)
V=$(grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2)
if grep -q "^## $V " CHANGELOG.md && grep -q "\*\*$V:" README.md; then
  TAG=$(git describe --tags --exact-match 2>/dev/null || echo "")
  if [ -z "$TAG" ]; then
    skip 16 "no exact-match tag on this tree (version/CHANGELOG/README agree at $V)"
  elif [ "$TAG" = "v$V" ]; then
    ok 16 "tag $TAG == workspace $V == CHANGELOG top == README banner"
  else
    bad 16 "tag $TAG != v$V"
  fi
else
  bad 16 "CHANGELOG top or README banner does not carry version $V"
fi

echo "----"
echo "gates: $PASS passed, $FAIL failed, $SKIP skipped"
[ "$FAIL" -eq 0 ]
