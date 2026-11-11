// The engine façade (W4.3): the app's single point of contact with
// marfim-ffi. State is an honest discriminated result — the UI can only
// render what the engine actually answered (no placeholder text, G7).
library;

import 'ffi/bindings.dart';

/// What the engine layer knows. `ready` carries the version; `unavailable`
/// carries the honest failure reason (library absent, symbol missing,
/// platform unsupported).
sealed class EngineState {
  const EngineState();
}

class EngineReady extends EngineState {
  const EngineReady(this.version);
  final String version;
}

class EngineUnavailable extends EngineState {
  const EngineUnavailable(this.reason);
  final String reason;
}

/// One probe outcome as the diagnostics screen renders it.
class ProbeReport {
  const ProbeReport({required this.ok, this.keys = const [], this.error});
  final bool ok;
  final List<String> keys;
  final String? error;
}

/// Lazily-initialised façade. All calls are synchronous and cheap; the
/// façade exists so screens never touch dart:ffi directly.
class MarfimEngine {
  MarfimEngine._(this._ffi);
  final MarfimFfi _ffi;

  static MarfimEngine? _instance;

  /// Opens the native library. Throws on absent artifacts — callers turn
  /// that into [EngineUnavailable], never into a fake answer.
  static MarfimEngine open() {
    final existing = _instance;
    if (existing != null) return existing;
    _instance = MarfimEngine._(MarfimFfi.open());
    return _instance!;
  }

  /// Version + probe in one call, always answering a [EngineState].
  /// The sample config is the smallest document the engine's loader will
  /// accept — it proves the boundary round-trips, it configures nothing.
  ({EngineState state, ProbeReport probe}) diagnose() {
    try {
      final version = _ffi.version();
      final envelope = _ffi.configProbe('model = "claude-sonnet-4"\n');
      final report = envelope['ok'] == true
          ? ProbeReport(
              ok: true,
              keys: ((envelope['result'] as Map<String, dynamic>)['keys']
                      as List<dynamic>)
                  .cast<String>())
          : ProbeReport(ok: false, error: envelope['error'] as String?);
      return ((state: EngineReady(version), probe: report));
    } catch (e) {
      return ((state: EngineUnavailable(e.toString()), probe: const ProbeReport(ok: false, error: 'engine unavailable')));
    }
  }
}
