# Android findings (W4.4)

Everything learnable about the mobile build without an Android SDK on
the build machine — recorded honestly, with the unknowns named. This
file is the W4.4 ledger note; G12/G13 stay SKIP until the artifacts
below exist.

## What is pinned by construction

- **Toolchain**: Rust 1.99.1 (rust-toolchain.toml), target
  `aarch64-linux-android`, built through `cargo-ndk` exactly as gate G12
  spells it: `cargo ndk -t arm64-v8a -o
  apps/flutter/android/app/src/main/jniLibs build --release -p
  marfim-ffi`.
- **AGP 8.5.2** — chosen because it packages 16 KiB-page-aligned .so
  payloads (Play requirement for new targets since Nov 2025); the pin
  lives in `android/settings.gradle`. NDK 27.x is the matching default
  of AGP 8.5; the exact NDK download happens in CI.
- **Java-only shell**: `MainActivity` is Java (no Kotlin plugin in the
  graph) — the shell has no logic worth a second language.
- **minSdk 26 / targetSdk 35 / compileSdk 35** — the floor is set by the
  vector-only adaptive-icon pipeline (no binary assets tracked) and the
  modern .so packaging story; raised from the usual Flutter 21 floor
  deliberately and documented here.
- **abiFilters `arm64-v8a` only** — the W4.3 DESIGN budget table is
  per-ABI; adding armeabi-v7a/x86_64 is a one-line change plus a G12
  budget re-check, not an architectural one.
- **Rust side compiles for android**: W1.2 already proved
  `rustc --target aarch64-linux-android` compiles the `android` cfg arms
  of `core::platform` (no HOME assumption; `MARFIM_*` env contract).
  The FFI crate is core+app only — no new android surface to verify.

## What CI must prove the first time (W4.4 workflow)

1. `cargo ndk` produces `libmarfim_ffi.so` ≤ 35 MiB (G12 gate).
2. `flutter analyze` + `flutter test` green on the shell.
3. `dart test` in `contracts/dart` green against the protocol fixtures
   (the W2.2 promise: "they run where a Dart SDK exists and in the W4.4
   CI job").
4. Emulator instrumented suite (G13): `flutter test integration_test`
   against the honest-diagnostics smoke test.

## Honest unknowns (no SDK on this machine)

- The exact release `.so` size (desktop release binary is 14.2 MB with
  the provider stack; the FFI crate is strictly smaller, budget has
  ~2.5× headroom — real number lands with the first CI run).
- Emulator cold-start timing (OWNER-INPUT #5 asks for a device profile
  or accepts the x86_64 emulator baseline).
- Gradle wrapper: NOT committed (no gradle locally to generate a wrapper
  jar honestly); CI invokes a pinned Gradle version via the setup-gradle
  action. Local builds need `flutter build apk` (which materializes
  local.properties) — documented, not stubbed.
- `flutter pub get` lockfile: `pubspec.lock` is generated on the first
  CI pub get and committed from that run forward (Flutter convention
  for apps; there was no SDK here to produce it truthfully).
- Idle drain for the device-control increment (DESIGN.md "Later
  increment"): a foreground service plus accessibility-tree polling is
  the profile mid-tier-phone users already complain about. No battery or
  thermal budget exists and none is invented here; it needs an
  idle-suspend design and a measured row in DESIGN's budget table once
  the 6 GB device profile (OWNER-INPUT #5) exists.

## Deliberate non-goals recorded

- No provider keys in the app process (W1.3 chain stays server-side).
- No session state in Dart: the journal is the durable form (W6), the
  app renders gateway-protocol state only.
- No externalNativeBuild in gradle: cargo owns the Rust build, gradle
  packages the artifact — one build system per language.
