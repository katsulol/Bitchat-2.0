import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:bitchat2/core_ffi.dart';
import 'package:bitchat2/delivery_explanation.dart';
import 'package:bitchat2/main.dart';

void main() {
  test('DeliveryExplanation maps all states and typed reasons accurately', () {
    expect(
      DeliveryExplanation.explain(
        state: 'Queued',
        reason: 'WaitingForApprovedGateway',
      ),
      'Waiting for an approved gateway.',
    );
    expect(
      DeliveryExplanation.explain(
        state: 'Queued',
        reason: 'SessionHandshakeRequired',
      ),
      'Establishing a secure session.',
    );
    expect(
      DeliveryExplanation.explain(
        state: 'Queued',
        reason: 'RetryingAlternateGateway',
      ),
      'Retrying through another gateway.',
    );
    expect(
      DeliveryExplanation.explain(
        state: 'Queued',
        reason: 'ResourcePolicyPaused',
      ),
      'Delivery paused by resource policy.',
    );
    expect(
      DeliveryExplanation.explain(
        state: 'InFlight',
        reason: 'IrohGatewayAvailable',
        transport: 'IrohDirect',
        attemptCount: 1,
      ),
      'Waiting for recipient acknowledgment.',
    );
    // Lost ACK wording must NEVER assert recipient is offline
    expect(
      DeliveryExplanation.explain(
        state: 'InFlight',
        reason: '',
        attemptCount: 2,
      ).toLowerCase(),
      isNot(contains('offline')),
    );
    expect(
      DeliveryExplanation.explain(
        state: 'Delivered',
        reason: 'Delivered',
        transport: 'IrohDirect',
      ),
      'Delivered via Iroh Direct (acknowledged).',
    );
    expect(
      DeliveryExplanation.explain(
        state: 'Expired',
        reason: 'Expired',
      ),
      'Expired before confirmed delivery.',
    );
    expect(
      DeliveryExplanation.explain(
        state: 'Cancelled',
        reason: 'Cancelled',
      ),
      'Cancelled before delivery.',
    );
  });

  test('Deadline preset compatibility and RustDeliveryEvent enriched JSON parsing', () {
    expect(
      DeliveryExplanation.isDeadlineCompatibleWithMode(120, isPrivacy: true),
      isTrue,
    );
    expect(
      DeliveryExplanation.isDeadlineCompatibleWithMode(3600, isPrivacy: true),
      isTrue,
    );
    expect(
      DeliveryExplanation.isDeadlineCompatibleWithMode(7200, isPrivacy: true),
      isFalse,
    );
    expect(
      DeliveryExplanation.isDeadlineCompatibleWithMode(7200, isPrivacy: false),
      isTrue,
    );
    expect(
      DeliveryExplanation.isDeadlineCompatibleWithMode(0, isPrivacy: false),
      isFalse,
    );

    final ev = RustDeliveryEvent.fromJson({
      'logical_id': '0123456789abcdef0123456789abcdef',
      'state': 'InFlight',
      'recipient': 'aa' * 32,
      'timestamp': 1700000000,
      'reason': 'RetryingAlternateGateway',
      'transport': 'IrohRelay',
      'attempt_count': 2,
      'expires_at': 1700000900,
    });
    expect(ev.reason, 'RetryingAlternateGateway');
    expect(ev.transport, 'IrohRelay');
    expect(ev.attemptCount, 2);
    expect(ev.expiresAt, 1700000900);
  });

  testWidgets(
    'Narrow 320x640 screen, large text scale, and unmount/dispose/recreate lifecycle',
    (WidgetTester tester) async {
      tester.view.physicalSize = const Size(320, 640);
      tester.view.devicePixelRatio = 1.0;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);

      await tester.pumpWidget(
        const MediaQuery(
          data: MediaQueryData(
            size: Size(320, 640),
            textScaler: TextScaler.linear(1.8),
          ),
          child: BitchatApp(),
        ),
      );
      await tester.pump();
      expect(find.text('Bitchat 2.0 Mesh'), findsOneWidget);
      expect(tester.takeException(), isNull);

      // Unmount and recreate cleanly
      await tester.pumpWidget(const SizedBox.shrink());
      await tester.pump();
      await tester.pumpWidget(const BitchatApp());
      await tester.pump();
      expect(find.text('Bitchat 2.0 Mesh'), findsOneWidget);
      expect(tester.takeException(), isNull);
    },
  );
}
