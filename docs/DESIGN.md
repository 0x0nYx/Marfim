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
