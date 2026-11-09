// marfim-ffi Dart bindings (W4.3). One file per ABI contract: the four
// exported symbols of libmarfim_ffi.so, exactly as crates/marfim-ffi
// exports them (gate G14 hygiene holds on the Rust side; the Dart side
// mirrors the same names and adds no logic).
//
// Ownership contract (mirrors the Rust docs):
// - `marfimVersion` returns a STATIC pointer — never freed;
// - `marfimConfigProbe`/`marfimLastError` return heap pointers that MUST
//   be released with `marfimStringFree`;
// - null means "no error recorded" for lastError, and "panic escaped the
//   catch_unwind wrapper (recorded as last error)" for configProbe.
library;

import 'dart:convert';
import 'dart:ffi';
import 'dart:io';

import 'package:ffi/ffi.dart';

/// The dynamic library name as packaged by cargo-ndk into jniLibs
/// (apps/flutter/android/app/src/main/jniLibs/<abi>/libmarfim_ffi.so).
const String kMarfimFfiSoName = 'libmarfim_ffi.so';

/// Resolved native library. Android loads it from the APK's jniLibs;
/// Linux/macOS hosts load it from the process path. Kept internal:
/// consumers go through [MarfimFfi].
DynamicLibrary _openLibrary() {
  if (Platform.isAndroid || Platform.isLinux || Platform.isMacOS) {
    return DynamicLibrary.open(kMarfimFfiSoName);
  }
  // Windows hosts would need marfim_ffi.dll shipped next to the exe; not
  // a target of the W4 waves — fail honestly instead of guessing.
  throw UnsupportedError(
      'marfim-ffi: platform ${Platform.operatingSystem} has no shipped library');
}

/// Signature typedefs — the C ABI spelled out in Dart.
typedef _VersionC = Pointer<Utf8> Function();
typedef _VersionDart = Pointer<Utf8> Function();
typedef _ConfigProbeC = Pointer<Utf8> Function(Pointer<Utf8>);
typedef _ConfigProbeDart = Pointer<Utf8> Function(Pointer<Utf8>);
typedef _LastErrorC = Pointer<Utf8> Function();
typedef _LastErrorDart = Pointer<Utf8> Function();
typedef _StringFreeC = Void Function(Pointer<Utf8>);
typedef _StringFreeDart = void Function(Pointer<Utf8>);

/// Typed façade over the four symbols. Construct once per isolate; every
/// call is synchronous (the ABI is synchronous by design — the session
/// transport is the W2 gateway protocol, not the FFI).
class MarfimFfi {
  MarfimFfi._(DynamicLibrary lib)
      : _version =
            lib.lookupFunction<_VersionC, _VersionDart>('marfim_version'),
        _configProbe = lib.lookupFunction<_ConfigProbeC, _ConfigProbeDart>(
            'marfim_config_probe'),
        _lastError = lib
            .lookupFunction<_LastErrorC, _LastErrorDart>('marfim_last_error'),
        _stringFree = lib.lookupFunction<_StringFreeC, _StringFreeDart>(
            'marfim_string_free');

  /// Opens the shipped library and resolves the ABI. Throws honestly on
  /// any platform where the artifact is absent — never silently degrades.
  factory MarfimFfi.open() => MarfimFfi._(_openLibrary());

  final _VersionDart _version;
  final _ConfigProbeDart _configProbe;
  final _LastErrorDart _lastError;
  final _StringFreeDart _stringFree;

  /// The canonical engine version (single version source on the Rust side).
  String version() => _version().toDartString();

  /// Validates a config TOML document through the engine's own loader.
  /// Returns the parsed envelope: `{"ok":true,"result":…}` or
  /// `{"ok":false,"error":"…"}`.
  Map<String, dynamic> configProbe(String toml) {
    final inPtr = toml.toNativeUtf8();
    Pointer<Utf8> outPtr = Pointer.fromAddress(0);
    try {
      outPtr = _configProbe(inPtr);
      if (outPtr == Pointer.fromAddress(0)) {
        // Documented panic outcome; the reason is in the last error.
        return {
          'ok': false,
          'error': lastError() ?? 'marfim-ffi: panic at the boundary',
        };
      }
      return jsonDecode(outPtr.toDartString()) as Map<String, dynamic>;
    } finally {
      // Ownership: the input copy and (when non-null) the output envelope
      // are freed on every path — success, error, panic.
      calloc.free(inPtr);
      if (outPtr != Pointer.fromAddress(0)) {
        _stringFree(outPtr);
      }
    }
  }

  /// The calling isolate's last FFI error, or null when none recorded.
  String? lastError() {
    final p = _lastError();
    if (p == Pointer.fromAddress(0)) return null;
    final s = p.toDartString();
    _stringFree(p);
    return s;
  }
}
