// Pure-Dart tests (no native library needed): the envelope contract the
// app relies on, pinned independently of libmarfim_ffi.so. The Rust-side
// shape is proven by crates/marfim-ffi tests; these tests pin the DART
// consumption of that shape.
import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:marfim/engine.dart';

void main() {
  group('engine envelope contract', () {
    test('ok envelope parses into a ProbeReport with keys', () {
      const raw = '{"ok":true,"result":{"sections":2,"keys":["model","permissions"]}}';
      final envelope = jsonDecode(raw) as Map<String, dynamic>;
      final report = envelope['ok'] == true
          ? ProbeReport(
              ok: true,
              keys: ((envelope['result'] as Map<String, dynamic>)['keys']
                      as List<dynamic>)
                  .cast<String>())
          : const ProbeReport(ok: false);
      expect(report.ok, isTrue);
      expect(report.keys, ['model', 'permissions']);
    });

    test('error envelope parses into a ProbeReport with the error text', () {
      const raw =
          '{"ok":false,"error":"config invalid: unknown key `bogusKey` in `permissions`"}';
      final envelope = jsonDecode(raw) as Map<String, dynamic>;
      final report = ProbeReport(ok: false, error: envelope['error'] as String?);
      expect(report.ok, isFalse);
      expect(report.error, contains('bogusKey'));
    });

    test('ProbeReport defaults are honest (no fabricated keys)', () {
      const report = ProbeReport(ok: false, error: 'engine unavailable');
      expect(report.keys, isEmpty);
      expect(report.error, isNotNull);
    });
  });
}
