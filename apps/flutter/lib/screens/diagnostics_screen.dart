// The diagnostics screen (W4.3): the honest surface of the W4 waves. It
// renders exactly what the engine answered — version, config-probe
// verdict, and the failure reason when the artifact is absent. There is
// deliberately no chat surface here yet: sessions ride the W2 gateway
// protocol and are the NEXT FFI/gateway increment (docs/DESIGN.md §next).
library;

import 'package:flutter/material.dart';

import '../engine.dart';

class DiagnosticsScreen extends StatefulWidget {
  const DiagnosticsScreen({super.key});

  @override
  State<DiagnosticsScreen> createState() => _DiagnosticsScreenState();
}

class _DiagnosticsScreenState extends State<DiagnosticsScreen> {
  late final ({EngineState state, ProbeReport probe}) _report;

  @override
  void initState() {
    super.initState();
    _report = _diagnose();
  }

  ({EngineState state, ProbeReport probe}) _diagnose() {
    try {
      return MarfimEngine.open().diagnose();
    } catch (e) {
      return (
        state: EngineUnavailable(e.toString()),
        probe: const ProbeReport(ok: false, error: 'engine unavailable'),
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    final state = _report.state;
    return Scaffold(
      appBar: AppBar(title: const Text('Marfim — diagnostics')),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          switch (state) {
            EngineReady(:final version) => ListTile(
                leading: const Icon(Icons.verified, color: Colors.green),
                title: const Text('Engine loaded'),
                subtitle: Text('marfim $version'),
              ),
            EngineUnavailable(:final reason) => ListTile(
                leading: const Icon(Icons.error_outline, color: Colors.red),
                title: const Text('Engine unavailable'),
                subtitle: Text(reason, maxLines: 4, overflow: TextOverflow.fade),
              ),
          },
          const Divider(),
          ListTile(
            leading: Icon(
              _report.probe.ok ? Icons.check_circle : Icons.block,
              color: _report.probe.ok ? Colors.green : Colors.red,
            ),
            title: const Text('Config probe'),
            subtitle: _report.probe.ok
                ? Text('sections: ${_report.probe.keys.join(', ')}')
                : Text(_report.probe.error ?? 'refused'),
          ),
          const Divider(),
          const ListTile(
            title: Text('Session transport'),
            subtitle: Text(
                'Sessions ride the W2 gateway protocol (contracts/dart). '
                'This build ships the FFI boundary and diagnostics; the '
                'session surface lands with the gateway transport module.'),
          ),
        ],
      ),
    );
  }
}
