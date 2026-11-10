# Marfim mobile — design (W4.3)

The mobile surface is a thin, honest Dart shell over the SAME engine the
CLI runs. It invents no second brain: engine facts cross the W4.1 FFI
boundary; sessions ride the W2 gateway protocol. One version, one
permission engine, one journal — surfaces differ, truth does not.

## Layers

```
┌─────────────────────────────────────────────────────────┐
│ screens/       Material 3 widgets; render EngineState    │
│ engine.dart    façade: EngineState | ProbeReport         │
│ ffi/           dart:ffi bindings over marfim-ffi (W4.1)  │
│    libmarfim_ffi.so  ← cargo-ndk ← crates/marfim-ffi     │
│    (G14: catch_unwind boundary, forbid(unsafe) core)     │
└─────────────────────────────────────────────────────────┘
         sessions (next increment): W2 gateway protocol
         contracts/dart pins the SAME fixtures as gate G11
```

## FFI contract (as shipped)

| Symbol | Dart call | Ownership |
|---|---|---|
| `marfim_version` | `MarfimFfi.version()` | static, never freed |
| `marfim_config_probe` | `MarfimFfi.configProbe(toml)` | envelope freed via `marfim_string_free` |
| `marfim_last_error` | `MarfimFfi.lastError()` | freed via `marfim_string_free` |
| `marfim_string_free` | `MarfimFfi._stringFree` | caller-releases only entry |

Panic containment: a Rust panic inside a call is caught at the boundary
(`catch_unwind`, G14-enforced) and surfaces to Dart as a null pointer +
a readable last error — never a crashed app. Null and non-UTF8 inputs
are honest `ok:false` envelopes.

## What ships today

The diagnostics surface: engine loaded (version) or unavailable
(reason), and the config-probe verdict through the engine's own loader.
That is exactly what the FFI boundary can answer — no placeholder chat
surface, no fake session list (G7 doctrine).

## Next increment: the session surface

Sessions do NOT go through the FFI. They ride the gateway protocol
(`docs/PROTOCOL.md`): the app speaks `sessions/*` routes + the SSE
envelope to a locally-running `marfim serve` (or a user-designated
remote), with the token auth + TLS story from W2.3. The Dart contract
tests in `contracts/dart` are the drift guard. This split keeps the
mobile .so small (no provider stack in the app process) and the
permission surface authoritative on one side.

## Later increment: device-control hands (Android)

Unscheduled — recorded so the shell is not designed into a corner.
Origin: the owner's pre-Marfim on-device-agent plan; platform facts
re-checked 2026-11-10 (sources at the end of this section).

The agent acts for the user across other apps (the "employee" model).
OS tuning is one gated tool it can call, never the product.

- **Control layer: Android AccessibilityService**, hosted by the Java
  shell. Works unrooted on stock and custom ROMs; the Rust core stays
  platform-agnostic and `forbid(unsafe)`. The platform-native route is
  closed to us: `ACCESS_COMPUTER_CONTROL` (virtual-display Computer
  Control) is described as going only to allowlisted, Google/OEM-signed
  apps, and the QPR3 "Screen automation" special-access toggle was
  reported on Pixel 10 only. AppFunctions (Android 16) is the
  structured alternative where target apps publish functions: prefer
  it when present, fall back to the accessibility tree.
- **Hands are tools.** Each device action is a `ToolDef` call through
  the PermissionEngine (every mutation flows through it); the shell
  never acts on its own initiative. The engine-to-shell transport is
  chosen when the row is scheduled; it must not move a provider stack
  or keys into the app process.
- **Two consent gates, never conflated.** Gate 1, "I want the agent to
  operate apps": enabling the accessibility service, unprivileged.
  Gate 2, "I want it to touch system settings": separate, root-gated
  (Magisk/KernelSU module, or a privileged-app signature on ROMs that
  allow it), optional, shipped after Gate 1 is proven. Gate-2 actions
  are explicit per invocation — never cron- or mandate-driven, never
  automatic.
- **Invocation model (our choice, stricter than the platform's):**
  explicit start, visible running state, stoppable at any moment,
  never scheduled. The platform's own permission text allows work
  while apps are in the background; we do not inherit that.
- **No on-device model in this surface.** The 35 MiB `.so` budget and
  the no-provider-stack-in-the-app-process rule stand; a local model,
  if ever used, is a provider behind the gateway, not linked into
  `libmarfim_ffi`.
- **Prior art:** DroidRun/Mobilerun Portal (MIT per its READMEs) is an
  accessibility-service APK with a local HTTP/WebSocket API, driven
  from a host over ADB. Adoption form is clean-room per
  docs/CLEAN-ROOM.md unless a vendoring decision is logged in
  docs/PORT-LEDGER.md.

Sources (secondary reporting, partly from pre-release code analysis —
verify against AOSP before relying):
9to5google.com/2026/01/15/android-16-qpr3-screen-automation ;
helentech.jp/news-75642 ; aospbooks.github.io/aosp-internal-book/
50-ai-appfunctions.

## Performance targets

Budgets that gate CI (G12 mirrors the .so budget):

| Target | Budget | Measured |
|---|---|---|
| `libmarfim_ffi.so` (arm64-v8a, release) | ≤ 35 MiB | pending first android build in CI (G12) |
| cold start to first diagnostics frame | ≤ 2 s | emulator x86_64 baseline pending — OWNER-INPUT #5 |
| config probe round-trip | ≤ 5 ms p50 (debug) | local desktop cdylib: sub-ms |

The desktop release binary is 14.2 MB with TLS+PTY compiled in; the FFI
crate adds the config loader only, so the 35 MiB budget has an order of
magnitude of headroom. Real numbers land here when the W4.4 CI job
first runs — recorded honestly, not estimated twice.

## Ownership + trust

- applicationId: `dev.marfim.app` (OWNER-INPUT #1 default; one-line
  change in `android/app/build.gradle` when the owner confirms).
- Signing: keystore secrets via CI env; absent → debug-sign with the
  fallback echoed in the workflow log (documented, not silent).
- The app holds no provider keys. Credentials stay server-side behind
  the gateway token; the W1.3 credential chain never reaches Dart.
- No reporting channel by default (device-control increment): anything
  shared moves only per session, after the user sees the exact diff of
  what would leave the device. Trust, not features, decides adoption in
  the ROM/F-Droid audience; one vague opt-in toggle is enough to sink it.
