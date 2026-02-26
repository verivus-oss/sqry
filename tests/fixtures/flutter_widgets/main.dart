// Fixture exercising InheritedWidget access patterns and multiple MethodChannel usage.
// Used by Phase 3 Sprint 2 Dart graph tests.

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

class ChannelRegistry extends InheritedWidget {
  const ChannelRegistry({
    super.key,
    required this.analyticsChannel,
    required this.paymentsChannel,
    required Widget child,
  }) : super(child: child);

  final MethodChannel analyticsChannel;
  final MethodChannel paymentsChannel;

  static ChannelRegistry of(BuildContext context) {
    final registry =
        context.dependOnInheritedWidgetOfExactType<ChannelRegistry>();
    assert(registry != null, 'ChannelRegistry not found in widget tree');
    return registry!;
  }

  @override
  bool updateShouldNotify(ChannelRegistry oldWidget) =>
      analyticsChannel != oldWidget.analyticsChannel ||
      paymentsChannel != oldWidget.paymentsChannel;
}

class AppShell extends StatefulWidget {
  const AppShell({super.key});

  @override
  State<AppShell> createState() => _AppShellState();
}

class _AppShellState extends State<AppShell> {
  static const MethodChannel analyticsChannel =
      MethodChannel('app/native/analytics');
  static const MethodChannel paymentsChannel =
      MethodChannel('app/native/payments');

  @override
  void initState() {
    super.initState();
    _warmUpChannels();
  }

  Future<void> _warmUpChannels() async {
    await analyticsChannel.invokeMethod('trackLaunch');
    await paymentsChannel.invokeMethod('preparePayments');
  }

  Future<void> _sendPurchaseEvent() async {
    final registry = ChannelRegistry.of(context);
    await registry.analyticsChannel.invokeMethod('trackPurchase');
    await registry.paymentsChannel.invokeMethod('commitPurchase');
  }

  @override
  Widget build(BuildContext context) {
    return ChannelRegistry(
      analyticsChannel: analyticsChannel,
      paymentsChannel: paymentsChannel,
      child: Builder(
        builder: (context) {
          final registry = ChannelRegistry.of(context);

          return Scaffold(
            appBar: AppBar(title: const Text('Storefront')),
            body: Center(
              child: Column(
                mainAxisSize: MainAxisSize.min,
                children: [
                  ElevatedButton(
                    onPressed: _sendPurchaseEvent,
                    child: const Text('Complete Purchase'),
                  ),
                  FutureBuilder<String>(
                    future: registry.paymentsChannel.invokeMethod<String>(
                      'currentPaymentProvider',
                    ),
                    builder: (context, snapshot) {
                      if (snapshot.connectionState == ConnectionState.waiting) {
                        return const CircularProgressIndicator.adaptive();
                      }
                      return Text('Provider: ${snapshot.data ?? 'unknown'}');
                    },
                  ),
                ],
              ),
            ),
          );
        },
      ),
    );
  }
}
