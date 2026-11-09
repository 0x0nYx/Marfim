// Marfim mobile entry point (W4.3). One screen today — diagnostics —
// because that is what the FFI boundary can answer honestly.
library;

import 'package:flutter/material.dart';

import 'screens/diagnostics_screen.dart';

void main() {
  runApp(const MarfimApp());
}

class MarfimApp extends StatelessWidget {
  const MarfimApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Marfim',
      theme: ThemeData(colorSchemeSeed: const Color(0xFF1B5E20), useMaterial3: true),
      darkTheme: ThemeData(
          colorSchemeSeed: const Color(0xFF1B5E20),
          brightness: Brightness.dark,
          useMaterial3: true),
      home: const DiagnosticsScreen(),
    );
  }
}
