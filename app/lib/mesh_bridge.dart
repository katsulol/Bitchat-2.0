import 'dart:async';
import 'package:flutter/services.dart';
import 'core_ffi.dart';
import 'delivery_explanation.dart';

/// Data model representing a verified mesh chat message.
class ChatMessage {
  final String msgId;
  final String? logicalId;
  final String sender;
  final String? recipient;
  final String text;
  final int hopCount;
  final bool isBroadcast;
  String deliveryState; // 'Queued', 'InFlight', 'Delivered', 'Failed', 'Expired', 'Cancelled'
  String deliveryReason;
  String observedTransport;
  int attemptCount;
  int expiresAtUnix;
  final int? requestedDeadlineSecs;
  final bool isPrivacy;
  final DateTime timestamp;

  ChatMessage({
    required this.msgId,
    this.logicalId,
    required this.sender,
    this.recipient,
    required this.text,
    required this.hopCount,
    required this.isBroadcast,
    this.deliveryState = 'Delivered',
    this.deliveryReason = '',
    this.observedTransport = 'Unknown',
    this.attemptCount = 0,
    this.expiresAtUnix = 0,
    this.requestedDeadlineSecs,
    this.isPrivacy = false,
    DateTime? timestamp,
  }) : timestamp = timestamp ?? DateTime.now();
}

/// Abstract backend interface wrapping native FFI calls so UI & bridge logic
/// can also be exercised deterministically in widget/unit tests.
abstract class MeshCoreClient {
  bool get isLoaded;
  String? get lastAbiError;
  bool loadLibrary();
  int getAbiVersion();
  RustBuildInfo? getBuildInfo();
  String? getLastError();
  int attachUi();
  bool detachUi();

  String? initCore(String storagePath);
  String? initWithKey(String storagePath, Uint8List? dek, [int version = 1]);
  String? getMyPeerId();
  String? sendBroadcast(String text);
  bool initiateHandshake(String recipientShortId);
  bool isSessionReady(String recipientShortId);
  String? sendMessage(String recipient, String text, int mode);
  String? sendMessageWithDeadline(
    String recipient,
    String text,
    int mode,
    int? deadlineSecs,
  );
  bool cancelMessage(String logicalId);
  RustOutboundFrame? pollOutbound();
  RustInboundMessage? pollInbound();
  List<RustDeliveryEvent> pollDeliveryEvents();
  List<RustAuthEvent> pollAuthEvents();
  List<RustPendingMessageRequest> pendingMessageRequests();
  void onPeerConnected(String shortId);
  void onPeerDisconnected(String shortId);
  void onFrameReceived(String shortId, Uint8List data);
  String recordBleRssi(String shortIdHex, int rssiDbm);
  String getPeerProximity(String shortIdHex);
  String? setVolatileSession(String recipientShortId, int retentionSecs);
  RustVolatileState getVolatileState(String recipientShortId);
  String? getPeerId(String shortId);
  String? updateResourceState({
    required int batteryPct,
    required int charging,
    required int powerSave,
    required int thermal,
    required bool userPaused,
  });
  String? setRelayContributionProfile(String mode);
  RustDiagnostics? getDiagnostics();
  int pumpOutbox();

  String? gatewayStart([String? relayMode]);
  bool gatewayStop();
  String? gatewayExportContact();
  String? gatewayImportAndApproveContact(String contactStr);
  bool gatewayConnectToContact(String contactStr);
  bool gatewaySetRoute(String recipientShortId, String targetEndpointIdHex);
  RustGatewayStats? gatewayGetStats();
  List<String> gatewayGetRoutes();
  List<RustPendingGatewayPeer> gatewayListPending();
  bool gatewayApprovePeer(String endpointIdHex);
  bool gatewayRevokeOrBlockPeer(String endpointIdHex, {required bool block});
  bool trustSetPeerState(String peerIdHex, String action);
  String? trustIssueVoucher(
    String subjectPeerIdHex,
    String audiencePeerIdHex,
    String capability,
    int lifetimeSecs,
  );
  Map<String, dynamic>? trustImportVoucher(String voucherUri);

  bool setCoverTraffic(bool enabled);
  RustCoverStatus? getCoverStatus();
  bool setSyncEnabled(bool enabled);
  Map<String, dynamic>? initiateSync(String peerIdHex, {bool useMerkle = false});
  RustSyncStatus? getSyncStatus();
}

/// Default production backend delegating directly to `CoreFfi.instance`.
class CoreFfiClient implements MeshCoreClient {
  final CoreFfi _ffi = CoreFfi.instance;

  @override
  bool get isLoaded => _ffi.isLoaded;
  @override
  String? get lastAbiError => _ffi.lastAbiError;
  @override
  bool loadLibrary() => _ffi.loadLibrary();
  @override
  int getAbiVersion() => _ffi.getAbiVersion();
  @override
  RustBuildInfo? getBuildInfo() => _ffi.getBuildInfo();
  @override
  String? getLastError() => _ffi.getLastError();
  @override
  int attachUi() => _ffi.attachUi();
  @override
  bool detachUi() => _ffi.detachUi();
  @override
  String? initCore(String storagePath) => _ffi.initCore(storagePath);
  @override
  String? initWithKey(String storagePath, Uint8List? dek, [int version = 1]) =>
      _ffi.initWithKey(storagePath, dek, version);
  @override
  String? getMyPeerId() => _ffi.getMyPeerId();
  @override
  String? sendBroadcast(String text) => _ffi.sendBroadcast(text);
  @override
  bool initiateHandshake(String recipientShortId) =>
      _ffi.initiateHandshake(recipientShortId);
  @override
  bool isSessionReady(String recipientShortId) =>
      _ffi.isSessionReady(recipientShortId);
  @override
  String? sendMessage(String recipient, String text, int mode) =>
      _ffi.sendMessage(recipient, text, mode);
  @override
  String? sendMessageWithDeadline(
    String recipient,
    String text,
    int mode,
    int? deadlineSecs,
  ) =>
      _ffi.sendMessageWithDeadline(recipient, text, mode, deadlineSecs);
  @override
  bool cancelMessage(String logicalId) => _ffi.cancelMessage(logicalId);
  @override
  RustOutboundFrame? pollOutbound() => _ffi.pollOutbound();
  @override
  RustInboundMessage? pollInbound() => _ffi.pollInbound();
  @override
  List<RustDeliveryEvent> pollDeliveryEvents() => _ffi.pollDeliveryEvents();
  @override
  List<RustAuthEvent> pollAuthEvents() => _ffi.pollAuthEvents();
  @override
  List<RustPendingMessageRequest> pendingMessageRequests() =>
      _ffi.pendingMessageRequests();
  @override
  void onPeerConnected(String shortId) => _ffi.onPeerConnected(shortId);
  @override
  void onPeerDisconnected(String shortId) => _ffi.onPeerDisconnected(shortId);
  @override
  void onFrameReceived(String shortId, Uint8List data) =>
      _ffi.onFrameReceived(shortId, data);
  @override
  String recordBleRssi(String shortIdHex, int rssiDbm) =>
      _ffi.recordBleRssi(shortIdHex, rssiDbm);
  @override
  String getPeerProximity(String shortIdHex) =>
      _ffi.getPeerProximity(shortIdHex);
  @override
  String? setVolatileSession(String recipientShortId, int retentionSecs) =>
      _ffi.setVolatileSession(recipientShortId, retentionSecs);
  @override
  RustVolatileState getVolatileState(String recipientShortId) =>
      _ffi.getVolatileState(recipientShortId);
  @override
  String? getPeerId(String shortId) => _ffi.getPeerId(shortId);
  @override
  String? updateResourceState({
    required int batteryPct,
    required int charging,
    required int powerSave,
    required int thermal,
    required bool userPaused,
  }) =>
      _ffi.updateResourceState(
        batteryPct: batteryPct,
        charging: charging,
        powerSave: powerSave,
        thermal: thermal,
        userPaused: userPaused,
      );
  @override
  String? setRelayContributionProfile(String mode) =>
      _ffi.setRelayContributionProfile(mode);
  @override
  RustDiagnostics? getDiagnostics() => _ffi.getDiagnostics();
  @override
  int pumpOutbox() => _ffi.pumpOutbox();
  @override
  String? gatewayStart([String? relayMode]) => _ffi.gatewayStart(relayMode);
  @override
  bool gatewayStop() => _ffi.gatewayStop();
  @override
  String? gatewayExportContact() => _ffi.gatewayExportContact();
  @override
  String? gatewayImportAndApproveContact(String contactStr) =>
      _ffi.gatewayImportAndApproveContact(contactStr);
  @override
  bool gatewayConnectToContact(String contactStr) =>
      _ffi.gatewayConnectToContact(contactStr);
  @override
  bool gatewaySetRoute(String recipientShortId, String targetEndpointIdHex) =>
      _ffi.gatewaySetRoute(recipientShortId, targetEndpointIdHex);
  @override
  RustGatewayStats? gatewayGetStats() => _ffi.gatewayGetStats();
  @override
  List<String> gatewayGetRoutes() => _ffi.gatewayGetRoutes();
  @override
  List<RustPendingGatewayPeer> gatewayListPending() =>
      _ffi.gatewayListPending();
  @override
  bool gatewayApprovePeer(String endpointIdHex) =>
      _ffi.gatewayApprovePeer(endpointIdHex);
  @override
  bool gatewayRevokeOrBlockPeer(String endpointIdHex, {required bool block}) =>
      _ffi.gatewayRevokeOrBlockPeer(endpointIdHex, block: block);
  @override
  bool trustSetPeerState(String peerIdHex, String action) =>
      _ffi.trustSetPeerState(peerIdHex, action);
  @override
  String? trustIssueVoucher(
    String subjectPeerIdHex,
    String audiencePeerIdHex,
    String capability,
    int lifetimeSecs,
  ) =>
      _ffi.trustIssueVoucher(
        subjectPeerIdHex,
        audiencePeerIdHex,
        capability,
        lifetimeSecs,
      );
  @override
  Map<String, dynamic>? trustImportVoucher(String voucherUri) =>
      _ffi.trustImportVoucher(voucherUri);
  @override
  bool setCoverTraffic(bool enabled) => _ffi.setCoverTraffic(enabled);
  @override
  RustCoverStatus? getCoverStatus() => _ffi.getCoverStatus();
  @override
  bool setSyncEnabled(bool enabled) => _ffi.setSyncEnabled(enabled);
  @override
  Map<String, dynamic>? initiateSync(
    String peerIdHex, {
    bool useMerkle = false,
  }) =>
      _ffi.initiateSync(peerIdHex, useMerkle: useMerkle);
  @override
  RustSyncStatus? getSyncStatus() => _ffi.getSyncStatus();
}

/// Enterprise mesh bridge coordinator linking Native BLE GATT and Rust bitchat2_core.
class MeshBridge {
  static const MethodChannel _defaultChannel = MethodChannel('com.bitchat.mesh/ble');

  final MeshCoreClient _core;
  final MethodChannel _channel;

  final _messageController = StreamController<ChatMessage>.broadcast();
  final _peersController = StreamController<List<String>>.broadcast();
  final _deliveryEventController = StreamController<RustDeliveryEvent>.broadcast();
  final _stateController = StreamController<void>.broadcast();

  final Set<String> _connectedPeers = {};
  final Set<String> _knownPeers = {};
  final Set<String> _seenMsgIds = {};
  final List<RustAuthEvent> recentAuthEvents = [];
  final List<ChatMessage> _quarantinedMessages = [];
  final Map<String, String> peerTrustStates = {}; // peerIdHex -> 'approve' | 'introducer' | 'block' | 'revoke'
  final Set<String> approvedGatewayContacts = {}; // endpointId / URI summary

  String myShortId = "Initializing...";
  String myPeerId = "Generating...";
  String? lastKeystoreError;
  String? lastAbiError;
  String? lastPermissionError;
  RustBuildInfo? buildInfo;

  bool get hasKeystoreError => lastKeystoreError != null;
  bool get hasAbiError => lastAbiError != null;
  bool get permissionsDenied => lastPermissionError != null;
  bool get supportsExperimentalFeatures =>
      _core.isLoaded && _core.getAbiVersion() >= CoreFfi.expectedAbiVersion;

  bool coverTrafficEnabled = false;
  bool syncEnabled = false;

  Stream<ChatMessage> get messages => _messageController.stream;
  Stream<List<String>> get peers => _peersController.stream;
  Stream<RustDeliveryEvent> get deliveryEvents => _deliveryEventController.stream;
  Stream<void> get stateChanges => _stateController.stream;
  List<String> get currentPeers => _connectedPeers.toList();
  List<String> get knownPeers => _knownPeers.toList();
  List<ChatMessage> get quarantinedMessages => List.unmodifiable(_quarantinedMessages);

  int transmissions = 0;
  int deliveries = 0;
  int duplicatesDropped = 0;
  int relaysSuppressed = 0;

  bool userPausedRelaying = false;
  String relayContributionMode = 'balanced';
  int _lastBatteryPct = -1;
  int _lastCharging = -1;
  int _lastPowerSave = -1;
  int _lastThermal = 0;

  Timer? _pumpTimer;
  bool _isPumping = false;
  bool _isDisposed = false;

  MeshBridge({MeshCoreClient? core, MethodChannel? channel})
      : _core = core ?? CoreFfiClient(),
        _channel = channel ?? _defaultChannel {
    _channel.setMethodCallHandler(_handleNativeCall);
  }

  void _notifyState() {
    if (!_isDisposed && !_stateController.isClosed) {
      _stateController.add(null);
    }
  }

  String? getLastError() => _core.getLastError();

  RustBuildInfo? getBuildInfo() {
    buildInfo ??= _core.getBuildInfo();
    return buildInfo;
  }

  List<RustAuthEvent> pollAuthEvents() {
    final polled = _core.pollAuthEvents();
    if (polled.isNotEmpty) {
      recentAuthEvents.addAll(polled);
      if (recentAuthEvents.length > 64) {
        recentAuthEvents.removeRange(0, recentAuthEvents.length - 64);
      }
    }
    return List.unmodifiable(recentAuthEvents.reversed);
  }

  List<RustPendingMessageRequest> getPendingMessageRequests() {
    return _core.pendingMessageRequests();
  }

  bool setCoverTraffic(bool enabled) {
    coverTrafficEnabled = _core.setCoverTraffic(enabled);
    _drainOutbound();
    _notifyState();
    return coverTrafficEnabled;
  }

  RustCoverStatus? getCoverStatus() => _core.getCoverStatus();

  bool setSyncEnabled(bool enabled) {
    syncEnabled = _core.setSyncEnabled(enabled);
    _notifyState();
    return syncEnabled;
  }

  Map<String, dynamic> initiateSync(
    String peerIdHex, {
    bool useMerkle = false,
  }) {
    final res = _core.initiateSync(peerIdHex, useMerkle: useMerkle);
    _drainOutbound();
    if (res == null) {
      throw Exception(_core.getLastError() ?? 'SYNC_UNAVAILABLE: Peer does not support sync or archive is disabled');
    }
    return res;
  }

  RustSyncStatus? getSyncStatus() => _core.getSyncStatus();

  Future<String?> startGateway([String? relayMode]) async {
    return _core.gatewayStart(relayMode);
  }

  Future<bool> stopGateway() async {
    return _core.gatewayStop();
  }

  String? exportGatewayContact() {
    return _core.gatewayExportContact();
  }

  String? importAndApproveGatewayContact(String contactStr) {
    final res = _core.gatewayImportAndApproveContact(contactStr);
    if (res != null && res.isNotEmpty) {
      approvedGatewayContacts.add(res);
      _notifyState();
    }
    return res;
  }

  bool connectToGatewayContact(String contactStr) {
    final ok = _core.gatewayConnectToContact(contactStr);
    _notifyState();
    return ok;
  }

  bool setGatewayRoute(String recipientShortId, String targetEndpointIdHex) {
    if (recipientShortId.isNotEmpty && recipientShortId != myShortId) {
      _knownPeers.add(recipientShortId.toUpperCase());
    }
    final ok = _core.gatewaySetRoute(recipientShortId, targetEndpointIdHex);
    _notifyState();
    return ok;
  }

  RustGatewayStats? getGatewayStats() {
    return _core.gatewayGetStats();
  }

  List<String> getGatewayRoutes() {
    final routes = _core.gatewayGetRoutes();
    for (final r in routes) {
      if (r != myShortId) {
        _knownPeers.add(r);
      }
    }
    return routes;
  }

  List<RustPendingGatewayPeer> getPendingGatewayPeers() {
    return _core.gatewayListPending();
  }

  bool approveGatewayPeer(String endpointIdHex) {
    final ok = _core.gatewayApprovePeer(endpointIdHex);
    if (ok) {
      approvedGatewayContacts.add(endpointIdHex);
      _notifyState();
    }
    return ok;
  }

  bool revokeOrBlockGatewayPeer(String endpointIdHex, {required bool block}) {
    final ok = _core.gatewayRevokeOrBlockPeer(endpointIdHex, block: block);
    if (ok) {
      approvedGatewayContacts.remove(endpointIdHex);
      _notifyState();
    }
    return ok;
  }

  bool setPeerTrustState(String peerIdHex, String action) {
    final ok = _core.trustSetPeerState(peerIdHex, action);
    if (ok) {
      peerTrustStates[peerIdHex.toLowerCase()] = action;
      if (action == 'approve' || action == 'introducer') {
        final released = <ChatMessage>[];
        _quarantinedMessages.removeWhere((m) {
          final senderFull = _core.getPeerId(m.sender)?.toLowerCase();
          final matches = senderFull == peerIdHex.toLowerCase() ||
              peerIdHex.toUpperCase().startsWith(m.sender.toUpperCase());
          if (matches) {
            released.add(m);
          }
          return matches;
        });
        for (final m in released) {
          if (!_isDisposed && !_messageController.isClosed) {
            _messageController.add(m);
          }
        }
      } else if (action == 'block' || action == 'revoke') {
        _quarantinedMessages.removeWhere((m) {
          final senderFull = _core.getPeerId(m.sender)?.toLowerCase();
          return senderFull == peerIdHex.toLowerCase() ||
              peerIdHex.toUpperCase().startsWith(m.sender.toUpperCase());
        });
      }
    }
    _notifyState();
    return ok;
  }

  String? issueVoucher({
    required String subjectPeerIdHex,
    required String audiencePeerIdHex,
    String capability = 'direct_message',
    int lifetimeSecs = 3600,
  }) {
    return _core.trustIssueVoucher(
      subjectPeerIdHex,
      audiencePeerIdHex,
      capability,
      lifetimeSecs,
    );
  }

  Map<String, dynamic>? importVoucher(String voucherUri) {
    return _core.trustImportVoucher(voucherUri);
  }

  String? get lastCoreError => _core.getLastError();

  String? getPeerFullId(String shortIdHex) {
    return _core.getPeerId(shortIdHex);
  }

  String getPeerProximity(String shortIdHex) {
    return _core.getPeerProximity(shortIdHex);
  }

  String? setVolatileSession(String recipientShortId, int retentionSecs) {
    final res = _core.setVolatileSession(recipientShortId, retentionSecs);
    _drainOutbound();
    _notifyState();
    return res;
  }

  RustVolatileState getVolatileState(String recipientShortId) {
    return _core.getVolatileState(recipientShortId);
  }

  void setUserPausedRelaying(bool paused) {
    userPausedRelaying = paused;
    if (paused) {
      relayContributionMode = 'receive_only';
    } else if (relayContributionMode == 'receive_only') {
      relayContributionMode = 'balanced';
    }
    _core.updateResourceState(
      batteryPct: _lastBatteryPct,
      charging: _lastCharging,
      powerSave: _lastPowerSave,
      thermal: _lastThermal,
      userPaused: userPausedRelaying,
    );
    _notifyState();
  }

  String cycleRelayContributionMode() {
    const order = ['balanced', 'help_network', 'minimal', 'receive_only'];
    final idx = order.indexOf(relayContributionMode);
    final next = order[(idx + 1) % order.length];
    setRelayContributionMode(next);
    return next;
  }

  void rememberPeer(String shortId) {
    if (shortId.isNotEmpty && shortId != myShortId) {
      _knownPeers.add(shortId.toUpperCase());
      _notifyState();
    }
  }

  String? setRelayContributionMode(String mode) {
    relayContributionMode = mode;
    userPausedRelaying = mode == 'receive_only' || mode == 'paused';
    final res = _core.setRelayContributionProfile(mode);
    _notifyState();
    return res;
  }

  Future<void> setSecureScreen(bool enabled) async {
    try {
      await _channel.invokeMethod('setSecureScreen', {'enabled': enabled});
    } catch (_) {}
  }

  RustDiagnostics? getDiagnostics() {
    return _core.getDiagnostics();
  }

  Future<void> shareText(String text, {String title = "Share via"}) async {
    try {
      await _channel.invokeMethod('shareText', {'text': text, 'title': title});
    } catch (_) {}
  }

  Future<void> retryPermissions() async {
    lastPermissionError = null;
    _notifyState();
    try {
      await _channel.invokeMethod('startBle', {'shortId': myShortId});
    } catch (_) {}
  }

  Future<void> startMesh() async {
    lastKeystoreError = null;
    lastAbiError = null;

    // 1. Obtain Android storage path
    String storagePath = ".";
    try {
      final appPath = await _channel.invokeMethod<String>('getAppStoragePath');
      if (appPath != null && appPath.isNotEmpty) {
        storagePath = appPath;
      }
    } catch (_) {}

    // 2. Obtain hardware-backed DEK from AndroidKeystoreProvider (fail closed on platform error)
    Uint8List? dek;
    bool pluginMissing = false;
    try {
      dek = await _channel.invokeMethod<Uint8List>('getPlatformDek');
    } on MissingPluginException {
      pluginMissing = true;
    } on PlatformException catch (e) {
      lastKeystoreError =
          '${e.code}: ${e.message ?? "Android Keystore unavailable"}';
      _notifyState();
      return;
    } catch (e) {
      lastKeystoreError = 'KEYSTORE_ERROR: $e';
      _notifyState();
      return;
    }

    // 3. Verify native library ABI before initializing encrypted SQLite databases
    final loaded = _core.loadLibrary();
    if (!loaded && _core.lastAbiError != null) {
      lastAbiError = _core.lastAbiError;
      _notifyState();
      return;
    }

    // 4. Initialize Rust bitchat2_core with encrypted SQLite databases
    String? shortId;
    if (!pluginMissing) {
      if (!loaded) {
        lastAbiError = _core.lastAbiError ??
            'ABI_LOAD_ERROR: Native library could not be loaded cleanly';
        _notifyState();
        return;
      }
      if (dek == null || dek.length != 32) {
        lastKeystoreError =
            'KEYSTORE_ERROR: Invalid DEK returned from platform keystore';
        _notifyState();
        return;
      }
      shortId = _core.initWithKey(storagePath, dek);
      if (shortId == null || shortId.isEmpty) {
        lastKeystoreError = _core.getLastError() ??
            'KEYSTORE_ERROR: Failed to unlock encrypted identity/outbox database';
        _notifyState();
        return;
      }
    } else if (loaded) {
      shortId = _core.initCore(storagePath);
    }

    if (shortId != null && shortId.isNotEmpty) {
      myShortId = shortId;
      myPeerId = _core.getMyPeerId() ?? "Unknown";
      _core.attachUi();
      buildInfo = _core.getBuildInfo();
      coverTrafficEnabled = _core.getCoverStatus()?.enabled ?? false;
      syncEnabled = _core.getSyncStatus()?.enabled ?? false;
    }
    _notifyState();

    // 5. Start native BLE Mesh Service with verified ShortId
    try {
      await _channel.invokeMethod('startBle', {'shortId': myShortId});
      final resState =
          await _channel.invokeMapMethod<String, int>('getDeviceResourceState');
      if (resState != null) {
        _lastBatteryPct = resState['batteryPct'] ?? -1;
        _lastCharging = resState['charging'] ?? -1;
        _lastPowerSave = resState['powerSave'] ?? -1;
        _lastThermal = resState['thermal'] ?? 0;
        _core.updateResourceState(
          batteryPct: _lastBatteryPct,
          charging: _lastCharging,
          powerSave: _lastPowerSave,
          thermal: _lastThermal,
          userPaused: userPausedRelaying,
        );
      }
    } catch (_) {
      // Running on unit test or environment without native BLE channel
    }

    // 6. Start discrete event pump for Rust outbound frames, inbound messages, and outbox pump
    _pumpTimer?.cancel();
    _pumpTimer = Timer.periodic(const Duration(milliseconds: 50), (_) {
      if (_isPumping || _isDisposed) return;
      _isPumping = true;
      try {
        _drainOutbound();
        _drainInbound();
        _drainDeliveryEvents();
        _core.pumpOutbox();
      } finally {
        _isPumping = false;
      }
    });
  }

  Future<void> stopMesh() async {
    _pumpTimer?.cancel();
    try {
      await _channel.invokeMethod('stopBle');
    } catch (_) {}
  }

  Future<void> sendBroadcast(String text) async {
    final msgId = _core.sendBroadcast(text);
    if (msgId == null) {
      throw Exception(
        _core.getLastError() ?? "Rust core failed to serialize broadcast envelope",
      );
    }

    transmissions++;
    _seenMsgIds.add(msgId);
    _drainOutbound();

    // Local echo
    if (!_isDisposed && !_messageController.isClosed) {
      _messageController.add(ChatMessage(
        msgId: msgId,
        sender: myShortId,
        recipient: null,
        text: text,
        hopCount: 0,
        isBroadcast: true,
      ));
    }
  }

  bool initiateHandshake(String recipientShortId) {
    final ok = _core.initiateHandshake(recipientShortId);
    _drainOutbound();
    return ok;
  }

  bool isSessionReady(String recipientShortId) {
    return _core.isSessionReady(recipientShortId);
  }

  Future<String?> sendMessage(
    String recipientShortId,
    String text, {
    bool isPrivacy = false,
    int? deadlineSecs,
  }) async {
    if (!DeliveryExplanation.isDeadlineCompatibleWithMode(
      deadlineSecs,
      isPrivacy: isPrivacy,
    )) {
      throw Exception(
        'Deadline ${DeliveryExplanation.formatDeadlineLabel(deadlineSecs)} exceeds ${isPrivacy ? "Privacy" : "Normal"} mode limit',
      );
    }

    final mode = isPrivacy ? 1 : 0;
    final logicalId = deadlineSecs == null
        ? _core.sendMessage(recipientShortId, text, mode)
        : _core.sendMessageWithDeadline(
            recipientShortId,
            text,
            mode,
            deadlineSecs,
          );
    if (logicalId == null) {
      final lastErr = _core.getLastError();
      if (lastErr != null &&
          (lastErr.contains('VOLATILE_UNSUPPORTED') ||
              lastErr.contains('VOLATILE_HANDSHAKE_PENDING') ||
              lastErr.contains('KEYSTORE') ||
              lastErr.contains('STORAGE') ||
              lastErr.contains('ABI_MISMATCH'))) {
        throw Exception(lastErr);
      }
      final isReady = _core.isSessionReady(recipientShortId);
      if (!isReady) {
        _core.initiateHandshake(recipientShortId);
        _drainOutbound();
        throw Exception(
          "Securing E2E channel with peer... Handshake started. Tap send again in a moment.",
        );
      }
      throw Exception(lastErr ?? "Failed to queue message in outbox");
    }

    transmissions++;
    _seenMsgIds.add(logicalId);
    _drainOutbound();
    _core.pumpOutbox();
    _drainDeliveryEvents();

    if (!_isDisposed && !_messageController.isClosed) {
      _messageController.add(ChatMessage(
        msgId: logicalId,
        logicalId: logicalId,
        sender: myShortId,
        recipient: recipientShortId,
        text: text,
        hopCount: 0,
        isBroadcast: false,
        deliveryState: 'Queued',
        deliveryReason: 'Queued',
        observedTransport: 'Unknown',
        attemptCount: 0,
        expiresAtUnix: deadlineSecs != null
            ? (DateTime.now().millisecondsSinceEpoch ~/ 1000) + deadlineSecs
            : 0,
        requestedDeadlineSecs: deadlineSecs,
        isPrivacy: isPrivacy,
      ));
    }

    return logicalId;
  }

  bool cancelMessage(String logicalId) {
    final cancelled = _core.cancelMessage(logicalId);
    _drainDeliveryEvents();
    return cancelled;
  }

  Future<void> sendDirect(
    String recipientShortId,
    String text, {
    bool isPrivacy = false,
  }) async {
    await sendMessage(recipientShortId, text, isPrivacy: isPrivacy);
  }

  void _drainOutbound() {
    if (_isDisposed) return;
    RustOutboundFrame? frame;
    while ((frame = _core.pollOutbound()) != null) {
      final f = frame!;
      try {
        _channel.invokeMethod('writeFrame', {
          'target': f.target,
          'data': f.bytes,
        });
      } catch (_) {}
    }
  }

  void _drainInbound() {
    if (_isDisposed) return;
    RustInboundMessage? msg;
    while ((msg = _core.pollInbound()) != null) {
      final m = msg!;
      if (_seenMsgIds.contains(m.msgId)) {
        duplicatesDropped++;
        continue; // Suppress duplicate
      }
      _seenMsgIds.add(m.msgId);
      if (m.sender.isNotEmpty && m.sender != myShortId) {
        _knownPeers.add(m.sender);
      }

      final chatMsg = ChatMessage(
        msgId: m.msgId,
        logicalId: null,
        sender: m.sender,
        recipient: m.recipient,
        text: m.text,
        hopCount: m.hopCount,
        isBroadcast: m.isBroadcast,
        deliveryState: 'Delivered',
      );

      // Hold PendingRequest messages in quarantine until explicitly approved by user
      if (!m.isBroadcast && m.authStatus == 'PendingRequest') {
        _quarantinedMessages.add(chatMsg);
        _notifyState();
        continue;
      }

      deliveries++;
      if (!_isDisposed && !_messageController.isClosed) {
        _messageController.add(chatMsg);
      }
    }
  }

  void _drainDeliveryEvents() {
    if (_isDisposed) return;
    final events = _core.pollDeliveryEvents();
    for (final ev in events) {
      if (!_isDisposed && !_deliveryEventController.isClosed) {
        _deliveryEventController.add(ev);
      }
    }
  }

  Future<dynamic> _handleNativeCall(MethodCall call) async {
    if (_isDisposed) return;
    switch (call.method) {
      case 'onPeerConnected':
        final shortId = call.arguments['shortId'] as String;
        if (shortId != myShortId) {
          _connectedPeers.add(shortId);
          _knownPeers.add(shortId);
          _core.onPeerConnected(shortId);
          _drainOutbound();
          if (!_peersController.isClosed) {
            _peersController.add(_connectedPeers.toList());
          }
        }
        break;

      case 'onPeerDisconnected':
        final shortId = call.arguments['shortId'] as String;
        _connectedPeers.remove(shortId);
        _core.onPeerDisconnected(shortId);
        if (!_peersController.isClosed) {
          _peersController.add(_connectedPeers.toList());
        }
        break;

      case 'onFrameReceived':
        final sender = call.arguments['sender'] as String;
        final data = call.arguments['data'] as Uint8List;
        _core.onFrameReceived(sender, data);
        _drainInbound();
        _drainOutbound();
        _drainDeliveryEvents();
        break;

      case 'onPeerRssi':
        final shortId = call.arguments['shortId'] as String?;
        final rssi = call.arguments['rssi'] as int?;
        if (shortId != null && rssi != null) {
          _core.recordBleRssi(shortId, rssi);
          _notifyState();
        }
        break;

      case 'onResourceStateChanged':
        _lastBatteryPct = (call.arguments['batteryPct'] as int?) ?? -1;
        _lastCharging = (call.arguments['charging'] as int?) ?? -1;
        _lastPowerSave = (call.arguments['powerSave'] as int?) ?? -1;
        _lastThermal = (call.arguments['thermal'] as int?) ?? 0;
        _core.updateResourceState(
          batteryPct: _lastBatteryPct,
          charging: _lastCharging,
          powerSave: _lastPowerSave,
          thermal: _lastThermal,
          userPaused: userPausedRelaying,
        );
        _notifyState();
        break;

      case 'onPermissionsDenied':
        final missing = (call.arguments?['missing'] as List?)
                ?.map((e) => e.toString())
                .toList() ??
            const <String>[];
        lastPermissionError = missing.isEmpty
            ? 'Required Bluetooth / Location permissions were denied.'
            : 'Denied permissions: ${missing.join(", ")}';
        _notifyState();
        break;
    }
  }

  void dispose() {
    _isDisposed = true;
    _pumpTimer?.cancel();
    _channel.setMethodCallHandler(null);
    _core.detachUi();
    _messageController.close();
    _peersController.close();
    _deliveryEventController.close();
    _stateController.close();
  }
}
