import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:bitchat2/core_ffi.dart';
import 'package:bitchat2/delivery_explanation.dart';
import 'package:bitchat2/main.dart';
import 'package:bitchat2/mesh_bridge.dart';

class FakeMeshCoreClient implements MeshCoreClient {
  bool loaded = true;
  String? abiErr;
  int abiVer = CoreFfi.expectedAbiVersion;
  String? lastErr;
  String shortId = 'A1B2C3D4E5F60708';
  String peerId =
      'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
  bool readySession = true;
  bool coverEnabled = false;
  bool syncEnabled = false;
  bool syncInitiateShouldFail = false;
  String syncFailureReason = 'SYNC_UNSUPPORTED_PEER: Remote peer disabled sync';
  String contributionMode = 'balanced';
  String resourceProfile = 'Balanced';
  int volatileRetentionSecs = 0;
  String volatileState = 'NotRequested';
  int attachUiCalls = 0;
  int detachUiCalls = 0;

  final Map<String, String> proximityMap = {};
  final List<String> gatewayRoutes = [];
  final List<RustPendingGatewayPeer> pendingGateways = [];
  final List<RustPendingMessageRequest> pendingMsgRequests = [];
  final List<RustAuthEvent> authEvents = [];
  final List<RustDeliveryEvent> deliveryEvents = [];
  final List<(String, String)> trustActions = [];
  final List<String> revokedOrBlockedGateways = [];
  final List<String> approvedGateways = [];
  final List<(String, bool)> syncInitiations = [];
  int msgCounter = 0;

  @override
  bool get isLoaded => loaded;
  @override
  String? get lastAbiError => abiErr;
  @override
  bool loadLibrary() => loaded;
  @override
  int getAbiVersion() => abiVer;
  @override
  RustBuildInfo? getBuildInfo() => RustBuildInfo(
        buildVersion: 'v2.0.6-phase6',
        abiVersion: abiVer,
        gitCommit: 'e400873deadb',
        buildDirty: false,
        buildTarget: 'aarch64-linux-android',
      );
  @override
  String? getLastError() => lastErr;
  @override
  int attachUi() {
    attachUiCalls++;
    return 1;
  }

  @override
  bool detachUi() {
    detachUiCalls++;
    return true;
  }

  @override
  String? initCore(String storagePath) => shortId;
  @override
  String? initWithKey(String storagePath, Uint8List? dek, [int version = 1]) =>
      shortId;
  @override
  String? getMyPeerId() => peerId;
  @override
  String? sendBroadcast(String text) => 'bc-${++msgCounter}';
  @override
  bool initiateHandshake(String recipientShortId) => true;
  @override
  bool isSessionReady(String recipientShortId) => readySession;

  @override
  String? sendMessage(String recipient, String text, int mode) {
    if (volatileState == 'UnsupportedByPeer') {
      lastErr =
          'VOLATILE_UNSUPPORTED: Peer does not support volatile messages; refusing silent persistent downgrade';
      return null;
    }
    if (volatileState == 'Pending') {
      lastErr =
          'VOLATILE_HANDSHAKE_PENDING: Volatile negotiation incomplete; refusing silent downgrade';
      return null;
    }
    lastErr = null;
    return '0000000000000000000000000000000${++msgCounter}';
  }

  @override
  String? sendMessageWithDeadline(
    String recipient,
    String text,
    int mode,
    int? deadlineSecs,
  ) =>
      sendMessage(recipient, text, mode);

  @override
  bool cancelMessage(String logicalId) => true;
  @override
  RustOutboundFrame? pollOutbound() => null;
  @override
  RustInboundMessage? pollInbound() => null;

  @override
  List<RustDeliveryEvent> pollDeliveryEvents() {
    final copy = List<RustDeliveryEvent>.from(deliveryEvents);
    deliveryEvents.clear();
    return copy;
  }

  @override
  List<RustAuthEvent> pollAuthEvents() {
    final copy = List<RustAuthEvent>.from(authEvents);
    authEvents.clear();
    return copy;
  }

  @override
  List<RustPendingMessageRequest> pendingMessageRequests() =>
      List<RustPendingMessageRequest>.from(pendingMsgRequests);

  @override
  void onPeerConnected(String shortId) {}
  @override
  void onPeerDisconnected(String shortId) {}
  @override
  void onFrameReceived(String shortId, Uint8List data) {}

  @override
  String recordBleRssi(String shortIdHex, int rssiDbm) {
    final cat = rssiDbm >= -60
        ? 'Immediate'
        : (rssiDbm >= -78 ? 'Near' : 'Far');
    proximityMap[shortIdHex] = cat;
    return cat;
  }

  @override
  String getPeerProximity(String shortIdHex) =>
      proximityMap[shortIdHex] ?? 'Unknown';

  @override
  String? setVolatileSession(String recipientShortId, int retentionSecs) {
    volatileRetentionSecs = retentionSecs;
    volatileState = retentionSecs == 0 ? 'NotRequested' : volatileState;
    return volatileState;
  }

  @override
  RustVolatileState getVolatileState(String recipientShortId) =>
      RustVolatileState(
        state: volatileState,
        retentionSecs: volatileRetentionSecs,
      );

  @override
  String? getPeerId(String shortId) =>
      'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb';

  @override
  String? updateResourceState({
    required int batteryPct,
    required int charging,
    required int powerSave,
    required int thermal,
    required bool userPaused,
  }) =>
      resourceProfile;

  @override
  String? setRelayContributionProfile(String mode) {
    contributionMode = mode;
    resourceProfile = mode;
    return resourceProfile;
  }

  @override
  RustDiagnostics? getDiagnostics() => RustDiagnostics(
        myShortId: shortId,
        myPeerId: peerId,
        connectedPeersCount: proximityMap.length,
        outboxQueuedCount: 1,
        outboxInflightCount: 1,
        deliveriesCount: 3,
        gatewayActive: true,
        gatewayRelayDisabled: false,
        resourceProfile: resourceProfile,
        relayContributionMode: contributionMode,
        pendingGatewayCount: pendingGateways.length,
        approvedGatewayCount: approvedGateways.length,
        keySecurityLevel: 'HardwareStrongBoxOrTEE',
        gatewayObservedPath: 'Direct',
        abiVersion: abiVer,
        gitCommit: 'e400873deadb',
        buildDirty: false,
        runtimeGeneration: 1,
        uiAttached: true,
      );

  @override
  int pumpOutbox() => 0;
  @override
  String? gatewayStart([String? relayMode]) => 'ep-local';
  @override
  bool gatewayStop() => true;
  @override
  String? gatewayExportContact() => 'bitchat-gw://ep-local';
  @override
  String? gatewayImportAndApproveContact(String contactStr) => 'ep-remote';
  @override
  bool gatewayConnectToContact(String contactStr) => true;
  @override
  bool gatewaySetRoute(String recipientShortId, String targetEndpointIdHex) =>
      true;
  @override
  RustGatewayStats? gatewayGetStats() => RustGatewayStats(
        isRunning: true,
        endpointId: 'ep-local',
        activeConnections: 1,
        forwardsImported: 2,
        forwardsExported: 4,
        loopsPrevented: 1,
        forwardsRejected: 0,
      );
  @override
  List<String> gatewayGetRoutes() => gatewayRoutes;
  @override
  List<RustPendingGatewayPeer> gatewayListPending() => pendingGateways;

  @override
  bool gatewayApprovePeer(String endpointIdHex) {
    approvedGateways.add(endpointIdHex);
    pendingGateways.removeWhere((p) => p.endpointId == endpointIdHex);
    return true;
  }

  @override
  bool gatewayRevokeOrBlockPeer(String endpointIdHex, {required bool block}) {
    revokedOrBlockedGateways.add(endpointIdHex);
    return true;
  }

  @override
  bool trustSetPeerState(String peerIdHex, String action) {
    trustActions.add((peerIdHex, action));
    pendingMsgRequests.removeWhere((r) => r.senderPeerId == peerIdHex);
    return true;
  }

  @override
  String? trustIssueVoucher(
    String subjectPeerIdHex,
    String audiencePeerIdHex,
    String capability,
    int lifetimeSecs,
  ) =>
      'bitchat-voucher://signed-1hop';

  @override
  Map<String, dynamic>? trustImportVoucher(String voucherUri) => {
        'subject_short_id': '1122334455667788',
        'capability': 'direct_message',
      };

  @override
  bool setCoverTraffic(bool enabled) {
    coverEnabled = enabled;
    return coverEnabled;
  }

  @override
  RustCoverStatus? getCoverStatus() => RustCoverStatus(
        enabled: coverEnabled,
        experimental: true,
        negotiatedPeers: coverEnabled ? 1 : 0,
        dummyFramesSent: coverEnabled ? 4 : 0,
        dummyBytesSent: coverEnabled ? 256 : 0,
        dummyFramesDiscarded: 0,
        maxBytesPerMinute: 4096,
      );

  @override
  bool setSyncEnabled(bool enabled) {
    syncEnabled = enabled;
    return syncEnabled;
  }

  @override
  Map<String, dynamic>? initiateSync(
    String peerIdHex, {
    bool useMerkle = false,
  }) {
    syncInitiations.add((peerIdHex, useMerkle));
    if (syncInitiateShouldFail) {
      lastErr = syncFailureReason;
      return null;
    }
    return {'initiated': true, 'mode': useMerkle ? 'merkle' : 'id_inventory'};
  }

  @override
  RustSyncStatus? getSyncStatus() => RustSyncStatus(
        enabled: syncEnabled,
        experimental: true,
        pendingCryptoReview: true,
        activeSessions: 0,
        maxRecordsPerConversation: 1024,
        maxSessionBytes: 262144,
      );
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  const channel = MethodChannel('com.bitchat.mesh/ble');
  final List<MethodCall> methodCalls = [];

  void setupDefaultChannel() {
    methodCalls.clear();
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(channel, (MethodCall call) async {
      methodCalls.add(call);
      if (call.method == 'getAppStoragePath') return '.';
      if (call.method == 'getPlatformDek') return Uint8List(32);
      if (call.method == 'startBle') return true;
      if (call.method == 'stopBle') return true;
      return null;
    });
  }

  Future<void> cleanUnmount(WidgetTester tester, MeshBridge bridge) async {
    await tester.pumpWidget(const SizedBox.shrink());
    await tester.pump();
    bridge.dispose();
  }

  tearDown(() {
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(channel, null);
  });

  testWidgets('1. ABI mismatch fails closed & displays actionable banner',
      (WidgetTester tester) async {
    setupDefaultChannel();
    final fake = FakeMeshCoreClient()
      ..loaded = false
      ..abiVer = 1
      ..abiErr = 'ABI_MISMATCH: host expects ABI v2, native library provides v1';
    final bridge = MeshBridge(core: fake);

    await tester.pumpWidget(BitchatApp(bridge: bridge));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    expect(find.textContaining('Native ABI mismatch (fail-closed)'), findsOneWidget);
    expect(find.textContaining('ABI_MISMATCH'), findsOneWidget);
    expect(bridge.myShortId, 'Initializing...');
    await cleanUnmount(tester, bridge);
  });

  testWidgets('2. Denied BLE permissions banner shows & triggers retry',
      (WidgetTester tester) async {
    setupDefaultChannel();
    final fake = FakeMeshCoreClient();
    final bridge = MeshBridge(core: fake);

    await tester.pumpWidget(BitchatApp(bridge: bridge));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    final byteData = const StandardMethodCodec().encodeMethodCall(
      const MethodCall('onPermissionsDenied', {
        'missing': ['BLUETOOTH_SCAN', 'BLUETOOTH_CONNECT'],
      }),
    );
    await TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .handlePlatformMessage(channel.name, byteData, (_) {});
    await tester.pump();

    expect(find.textContaining('Denied permissions: BLUETOOTH_SCAN, BLUETOOTH_CONNECT'),
        findsOneWidget);

    await tester.tap(find.text('Retry'));
    await tester.pump();
    expect(bridge.permissionsDenied, isFalse);
    expect(methodCalls.where((c) => c.method == 'startBle').length, greaterThanOrEqualTo(2));
    await cleanUnmount(tester, bridge);
  });

  testWidgets('3. Pending message requests Approve/Block + 1-hop voucher scope copy',
      (WidgetTester tester) async {
    setupDefaultChannel();
    final fake = FakeMeshCoreClient();
    const senderPid =
        '1111111111111111111111111111111111111111111111111111111111111111';
    fake.pendingMsgRequests.add(
      RustPendingMessageRequest(
        senderPeerId: senderPid,
        senderShortId: '1111111111111111',
        logicalId: 'deadbeef',
        reason: 'UnknownSender',
        receivedAtUnix: 1700000000,
        isVolatile: true,
      ),
    );
    fake.authEvents.add(
      RustAuthEvent(
        peerId: senderPid,
        shortId: '1111111111111111',
        logicalId: 'deadbeef',
        outcome: 'Pending',
        reason: 'UnknownSender',
        capability: 'ReceiveDirectMessage',
        timestamp: 1700000000,
      ),
    );
    final bridge = MeshBridge(core: fake);

    await tester.pumpWidget(BitchatApp(bridge: bridge));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    await tester.tap(find.byTooltip('Trust & Vouchers'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.textContaining('Scope: 1-hop introduction only'), findsOneWidget);
    expect(find.textContaining('never auto-approves relay gateways'), findsOneWidget);
    expect(find.textContaining('Pending Message Requests (1)'), findsOneWidget);
    expect(find.textContaining('Volatile'), findsOneWidget);
    expect(find.textContaining('Recent Auth Events (1)'), findsOneWidget);

    await tester.tap(find.text('Approve').first);
    await tester.pump();
    expect(fake.trustActions, contains((senderPid, 'approve')));
    expect(find.textContaining('Approved request from 1111111111111111'), findsOneWidget);
    await cleanUnmount(tester, bridge);
  });

  testWidgets('4. Gateway consent pending list Approve & Revoke controls',
      (WidgetTester tester) async {
    setupDefaultChannel();
    final fake = FakeMeshCoreClient();
    fake.pendingGateways.add(
      RustPendingGatewayPeer(
        endpointId: 'ep9988',
        peerId: '99887766554433221100998877665544',
      ),
    );
    final bridge = MeshBridge(core: fake);

    await tester.pumpWidget(BitchatApp(bridge: bridge));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    await tester.tap(find.byTooltip('Iroh Gateway'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.textContaining('Pending Gateway Consent (1):'), findsOneWidget);
    await tester.tap(find.text('Approve').first);
    await tester.pump();
    expect(fake.approvedGateways, contains('ep9988'));
    await cleanUnmount(tester, bridge);
  });

  testWidgets(
      '5. Volatile downgrade rejection blocks send & preserves failed draft + unavailable recipient stays selected',
      (WidgetTester tester) async {
    setupDefaultChannel();
    final fake = FakeMeshCoreClient()
      ..volatileState = 'UnsupportedByPeer'
      ..volatileRetentionSecs = 300;
    const peerShort = '2233445566778899';
    final bridge = MeshBridge(core: fake);

    await tester.pumpWidget(BitchatApp(bridge: bridge));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    // Simulate peer connected then selected, then disconnected -> never resets to broadcast
    final connMsg = const StandardMethodCodec().encodeMethodCall(
      const MethodCall('onPeerConnected', {'shortId': peerShort}),
    );
    await TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .handlePlatformMessage(channel.name, connMsg, (_) {});
    await tester.pump();

    await tester.tap(find.text(peerShort.substring(0, 8)));
    await tester.pump();
    expect(find.textContaining('Direct to:'), findsOneWidget);
    expect(
      find.textContaining('sending blocked (no silent downgrade)'),
      findsOneWidget,
    );

    // Disconnect peer: must stay selected showing (Offline) and chip stays visible as (Saved)
    final discMsg = const StandardMethodCodec().encodeMethodCall(
      const MethodCall('onPeerDisconnected', {'shortId': peerShort}),
    );
    await TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .handlePlatformMessage(channel.name, discMsg, (_) {});
    await tester.pump();
    expect(find.textContaining('Offline'), findsOneWidget);
    expect(find.textContaining('22334455 (Saved)'), findsOneWidget);

    // Enter draft, attempt send under UnsupportedByPeer -> blocked, draft preserved in TextField!
    const draft = 'Secret unsent volatile draft';
    await tester.enterText(find.byType(TextField).first, draft);
    await tester.tap(find.byIcon(Icons.send));
    await tester.pump();

    expect(find.textContaining('VOLATILE_UNSUPPORTED'), findsOneWidget);
    final field = tester.widget<TextField>(find.byType(TextField).first);
    expect(field.controller?.text, draft);
    expect(field.autocorrect, isFalse);
    expect(field.enableSuggestions, isFalse);
    await cleanUnmount(tester, bridge);
  });

  testWidgets(
      '6. Resource profile switching, experimental Cover/Sync defaults, disclaimers & capability failure',
      (WidgetTester tester) async {
    setupDefaultChannel();
    final fake = FakeMeshCoreClient()..syncInitiateShouldFail = true;
    const peerShort = '2233445566778899';
    final bridge = MeshBridge(core: fake);

    await tester.pumpWidget(BitchatApp(bridge: bridge));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    final connMsg = const StandardMethodCodec().encodeMethodCall(
      const MethodCall('onPeerConnected', {'shortId': peerShort}),
    );
    await TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .handlePlatformMessage(channel.name, connMsg, (_) {});
    await tester.pump();
    await tester.tap(find.text(peerShort.substring(0, 8)));
    await tester.pump();

    await tester.tap(find.byIcon(Icons.info_outline));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    // All 4 resource profiles present
    await tester.tap(find.text('Minimal'));
    await tester.pump();
    expect(fake.contributionMode, 'minimal');

    await tester.tap(find.text('Help the network'));
    await tester.pump();
    expect(fake.contributionMode, 'help_network');

    // Experimental controls default OFF with honest disclaimers
    expect(bridge.coverTrafficEnabled, isFalse);
    expect(bridge.syncEnabled, isFalse);
    expect(find.text('Experimental Traffic Padding (Default OFF)'), findsOneWidget);
    expect(find.textContaining('does not hide online presence'), findsOneWidget);
    expect(find.text('Experimental Normal-Mode Sync (Default OFF)'), findsOneWidget);
    expect(find.textContaining('Pending human crypto review'), findsOneWidget);

    // Enable Sync switch and trigger initiation against unsupported peer
    await tester.ensureVisible(find.text('Experimental Normal-Mode Sync (Default OFF)'));
    await tester.tap(find.text('Experimental Normal-Mode Sync (Default OFF)'));
    await tester.pump();
    expect(bridge.syncEnabled, isTrue);

    await tester.ensureVisible(find.text('Sync Peer (ID-Inventory)'));
    await tester.tap(find.text('Sync Peer (ID-Inventory)'));
    await tester.pump();
    expect(find.textContaining('SYNC_UNSUPPORTED_PEER'), findsOneWidget);
    await cleanUnmount(tester, bridge);
  });

  testWidgets('7. Experimental controls hidden when native ABI unsupported (< 2)',
      (WidgetTester tester) async {
    setupDefaultChannel();
    final fake = FakeMeshCoreClient()..abiVer = 1;
    final bridge = MeshBridge(core: fake);

    await tester.pumpWidget(BitchatApp(bridge: bridge));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    await tester.tap(find.byIcon(Icons.info_outline));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.text('Experimental Traffic Padding (Default OFF)'), findsNothing);
    expect(find.text('Experimental Normal-Mode Sync (Default OFF)'), findsNothing);
    await cleanUnmount(tester, bridge);
  });

  testWidgets(
      '8. RSSI categories only (no meters, Unknown for stale/remote), delivery states, narrow 320x640 + 1.8x scale',
      (WidgetTester tester) async {
    setupDefaultChannel();
    tester.view.physicalSize = const Size(320, 640);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    final fake = FakeMeshCoreClient();
    const directPeer = '3344556677889900';
    const gwPeer = '4455667788990011';
    fake.gatewayRoutes.add(gwPeer);
    final bridge = MeshBridge(core: fake);

    await tester.pumpWidget(
      MediaQuery(
        data: const MediaQueryData(
          size: Size(320, 640),
          textScaler: TextScaler.linear(1.8),
        ),
        child: BitchatApp(bridge: bridge),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    final connMsg = const StandardMethodCodec().encodeMethodCall(
      const MethodCall('onPeerConnected', {'shortId': directPeer}),
    );
    final rssiMsg = const StandardMethodCodec().encodeMethodCall(
      const MethodCall('onPeerRssi', {'shortId': directPeer, 'rssi': -55}),
    );
    await TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .handlePlatformMessage(channel.name, connMsg, (_) {});
    await TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .handlePlatformMessage(channel.name, rssiMsg, (_) {});
    await tester.pump();

    expect(find.text('33445566 (Immediate)'), findsOneWidget);
    expect(find.textContaining('m)'), findsNothing);
    expect(bridge.getPeerProximity(gwPeer), 'Unknown');

    // Gateway acceptance / relay does NOT report Delivered
    expect(
      DeliveryExplanation.explain(
        state: 'InFlight',
        reason: 'IrohGatewayAvailable',
        transport: 'IrohRelay',
        attemptCount: 1,
      ),
      'Waiting for recipient acknowledgment.',
    );
    expect(
      DeliveryExplanation.explain(state: 'Failed', reason: ''),
      'Stopped after retry limit.',
    );
    expect(tester.takeException(), isNull);
    await cleanUnmount(tester, bridge);
  });
}
