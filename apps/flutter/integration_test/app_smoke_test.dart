// Integration smoke test (runs on an emulator/device in the W4.4 CI job,
// gate G13). Asserts the honest surfaces: the app boots and the engine
// state renders one of the two truthful outcomes — never a placeholder.
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

import 'package:marfim/main.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  testWidgets('diagnostics screen renders an honest engine state',
      (tester) async {
    await tester.pumpWidget(const MarfimApp());
    await tester.pumpAndSettle();

    expect(find.text('Marfim — diagnostics'), findsOneWidget);
    // Exactly one of the two truthful engine outcomes is on screen.
    final loaded = find.text('Engine loaded');
    final unavailable = find.text('Engine unavailable');
    expect(loaded.evaluate().isNotEmpty || unavailable.evaluate().isNotEmpty,
        isTrue);
    expect(
        loaded.evaluate().isNotEmpty && unavailable.evaluate().isNotEmpty,
        isFalse);
    // The session transport note is present (honest scoping, not a stub).
    expect(find.text('Session transport'), findsOneWidget);
  });
}
