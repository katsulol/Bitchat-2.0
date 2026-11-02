import 'dart:async';
import 'package:flutter/services.dart';
import 'core_ffi.dart';

/// Data model representing a verified mesh chat message.
class ChatMessage {
  final String msgId;
  final String? logicalId;
  final String sender;
  final String? recipient;
  final String text;
  final int hopCount;
  final bool isBroadcast;
  String deliveryState; // 'Queued', 'InFlight', 'Delivered', 'Failed', 'Expired'
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
    this.isPrivacy = false,
    DateTime? timestamp,
  }) : timestamp = timestamp ?? DateTime.now();
}

/// Enterprise mesh bridge coordinator linking Native BLE GATT and Rust bitchat2_core.
class MeshBridge {
  static const MethodChannel _channel = MethodChannel('com.bitchat.mesh/ble');

  final _messageController = StreamController<ChatMessage>.broadcast();
  final _peersController = StreamController<List<String>>.broadcast();
  final _deliveryEventController = StreamController<RustDeliveryEvent>.broadcast();

  final Set<String> _connectedPeers = {};
  final Set<String> _seenMsgIds = {};

  String myShortId = "Initializing...";
  String myPeerId = "Generating...";

  Stream<ChatMessage> get messages => _messageController.stream;
  Stream<List<String>> get peers => _peersController.stream;
  Stream<RustDeliveryEvent> get deliveryEvents => _deliveryEventController.stream;
  List<String> get currentPeers => _connectedPeers.toList();

  int transmissions = 0;
  int deliveries = 0;
  int duplicatesDropped = 0;
  int relaysSuppressed = 0;

  bool userPausedRelaying = false;
  int _lastBatteryPct = -1;
  int _lastCharging = -1;
  int _lastPowerSave = -1;
  int _lastThermal = 0;

  Timer? _pumpTimer;
  bool _isPumping = false;
  bool _isDisposed = false;

  MeshBridge() {
    _channel.setMethodCallHandler(_handleNativeCall);
  }

  Future<String?> startGateway([String? relayMode]) async {
    return CoreFfi.instance.gatewayStart(relayMode);
  }

  Future<bool> stopGateway() async {
    return CoreFfi.instance.gatewayStop();
  }

  String? exportGatewayContact() {
    return CoreFfi.instance.gatewayExportContact();
  }

  String? importAndApproveGatewayContact(String contactStr) {
    return CoreFfi.instance.gatewayImportAndApproveContact(contactStr);
  }

  bool connectToGatewayContact(String contactStr) {
    return CoreFfi.instance.gatewayConnectToContact(contactStr);
  }

  bool setGatewayRoute(String recipientShortId, String targetEndpointIdHex) {
    return CoreFfi.instance.gatewaySetRoute(recipientShortId, targetEndpointIdHex);
  }

  RustGatewayStats? getGatewayStats() {
    return CoreFfi.instance.gatewayGetStats();
  }

  List<String> getGatewayRoutes() {
    return CoreFfi.instance.gatewayGetRoutes();
  }

  List<RustPendingGatewayPeer> getPendingGatewayPeers() {
    return CoreFfi.instance.gatewayListPending();
  }

  bool approveGatewayPeer(String endpointIdHex) {
    return CoreFfi.instance.gatewayApprovePeer(endpointIdHex);
  }

  bool revokeOrBlockGatewayPeer(String endpointIdHex, {required bool block}) {
    return CoreFfi.instance.gatewayRevokeOrBlockPeer(endpointIdHex, block: block);
  }

  bool setPeerTrustState(String peerIdHex, String action) {
    return CoreFfi.instance.trustSetPeerState(peerIdHex, action);
  }

  String? issueVoucher({
    required String subjectPeerIdHex,
    required String audiencePeerIdHex,
    String capability = 'direct_message',
    int lifetimeSecs = 3600,
  }) {
    return CoreFfi.instance.trustIssueVoucher(
      subjectPeerIdHex,
      audiencePeerIdHex,
      capability,
      lifetimeSecs,
    );
  }

  Map<String, dynamic>? importVoucher(String voucherUri) {
    return CoreFfi.instance.trustImportVoucher(voucherUri);
  }

  String getPeerProximity(String shortIdHex) {
    return CoreFfi.instance.getPeerProximity(shortIdHex);
  }

  String? setVolatileSession(String recipientShortId, int retentionSecs) {
    final res = CoreFfi.instance.setVolatileSession(recipientShortId, retentionSecs);
    _drainOutbound();
    return res;
  }

  RustVolatileState getVolatileState(String recipientShortId) {
    return CoreFfi.instance.getVolatileState(recipientShortId);
  }

  void setUserPausedRelaying(bool paused) {
    userPausedRelaying = paused;
    CoreFfi.instance.updateResourceState(
      batteryPct: _lastBatteryPct,
      charging: _lastCharging,
      powerSave: _lastPowerSave,
      thermal: _lastThermal,
      userPaused: userPausedRelaying,
    );
  }

  Future<void> setSecureScreen(bool enabled) async {
    try {
      await _channel.invokeMethod('setSecureScreen', {'enabled': enabled});
    } catch (_) {}
  }

  RustDiagnostics? getDiagnostics() {
    return CoreFfi.instance.getDiagnostics();
  }

  Future<void> shareText(String text, {String title = "Share via"}) async {
    try {
      await _channel.invokeMethod('shareText', {'text': text, 'title': title});
    } catch (_) {}
  }

  Future<void> startMesh() async {
    // 1. Obtain Android storage path
    String storagePath = ".";
    try {
      final appPath = await _channel.invokeMethod<String>('getAppStoragePath');
      if (appPath != null && appPath.isNotEmpty) {
        storagePath = appPath;
      }
    } catch (_) {}

    // 2. Obtain hardware-backed DEK from AndroidKeystoreProvider
    Uint8List? dek;
    try {
      dek = await _channel.invokeMethod<Uint8List>('getPlatformDek');
    } catch (_) {}

    // 3. Initialize Rust bitchat2_core with encrypted SQLite outbox
    String? shortId;
    if (dek != null && dek.length == 32) {
      shortId = CoreFfi.instance.initWithKey(storagePath, dek);
    }
    if (shortId == null || shortId.isEmpty) {
      // Fallback to legacy init if Keystore unavailable
      shortId = CoreFfi.instance.initCore(storagePath);
    }

    if (shortId != null && shortId.isNotEmpty) {
      myShortId = shortId;
      myPeerId = CoreFfi.instance.getMyPeerId() ?? "Unknown";
    }

    // 4. Start native BLE Mesh Service with verified ShortId
    try {
      await _channel.invokeMethod('startBle', {'shortId': myShortId});
      final resState = await _channel.invokeMapMethod<String, int>('getDeviceResourceState');
      if (resState != null) {
        _lastBatteryPct = resState['batteryPct'] ?? -1;
        _lastCharging = resState['charging'] ?? -1;
        _lastPowerSave = resState['powerSave'] ?? -1;
        _lastThermal = resState['thermal'] ?? 0;
        CoreFfi.instance.updateResourceState(
          batteryPct: _lastBatteryPct,
          charging: _lastCharging,
          powerSave: _lastPowerSave,
          thermal: _lastThermal,
          userPaused: userPausedRelaying,
        );
      }
    } catch (e) {
      // Running on emulator or test environment without BLE
    }

    // 5. Start discrete event pump for Rust outbound frames, inbound messages, and outbox pump
    _pumpTimer?.cancel();
    _pumpTimer = Timer.periodic(const Duration(milliseconds: 50), (_) {
      if (_isPumping || _isDisposed) return;
      _isPumping = true;
      try {
        _drainOutbound();
        _drainInbound();
        _drainDeliveryEvents();
        CoreFfi.instance.pumpOutbox();
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
    final msgId = CoreFfi.instance.sendBroadcast(text);
    if (msgId == null) {
      throw Exception("Rust core failed to serialize broadcast envelope");
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
    final ok = CoreFfi.instance.initiateHandshake(recipientShortId);
    _drainOutbound();
    return ok;
  }

  bool isSessionReady(String recipientShortId) {
    return CoreFfi.instance.isSessionReady(recipientShortId);
  }

  Future<String?> sendMessage(String recipientShortId, String text, {bool isPrivacy = false}) async {
    final mode = isPrivacy ? 1 : 0;
    final logicalId = CoreFfi.instance.sendMessage(recipientShortId, text, mode);
    if (logicalId == null) {
      // If session not ready, initiate handshake
      final isReady = CoreFfi.instance.isSessionReady(recipientShortId);
      if (!isReady) {
        CoreFfi.instance.initiateHandshake(recipientShortId);
        _drainOutbound();
        throw Exception("Securing E2E channel with peer... Handshake started. Tap send again in a moment.");
      }
      throw Exception("Failed to queue message in outbox");
    }

    transmissions++;
    _seenMsgIds.add(logicalId);
    _drainOutbound();
    CoreFfi.instance.pumpOutbox();
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
        isPrivacy: isPrivacy,
      ));
    }

    return logicalId;
  }

  Future<void> sendDirect(String recipientShortId, String text, {bool isPrivacy = false}) async {
    await sendMessage(recipientShortId, text, isPrivacy: isPrivacy);
  }

  void _drainOutbound() {
    if (_isDisposed) return;
    RustOutboundFrame? frame;
    while ((frame = CoreFfi.instance.pollOutbound()) != null) {
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
    while ((msg = CoreFfi.instance.pollInbound()) != null) {
      final m = msg!;
      if (_seenMsgIds.contains(m.msgId)) {
        duplicatesDropped++;
        continue; // Suppress duplicate
      }
      _seenMsgIds.add(m.msgId);
      deliveries++;

      if (!_isDisposed && !_messageController.isClosed) {
        _messageController.add(ChatMessage(
          msgId: m.msgId,
          logicalId: null,
          sender: m.sender,
          recipient: m.recipient,
          text: m.text,
          hopCount: m.hopCount,
          isBroadcast: m.isBroadcast,
          deliveryState: 'Delivered',
        ));
      }
    }
  }

  void _drainDeliveryEvents() {
    if (_isDisposed) return;
    final events = CoreFfi.instance.pollDeliveryEvents();
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
          CoreFfi.instance.onPeerConnected(shortId);
          if (!_peersController.isClosed) {
            _peersController.add(_connectedPeers.toList());
          }
        }
        break;

      case 'onPeerDisconnected':
        final shortId = call.arguments['shortId'] as String;
        _connectedPeers.remove(shortId);
        CoreFfi.instance.onPeerDisconnected(shortId);
        if (!_peersController.isClosed) {
          _peersController.add(_connectedPeers.toList());
        }
        break;

      case 'onFrameReceived':
        final sender = call.arguments['sender'] as String;
        final data = call.arguments['data'] as Uint8List;
        CoreFfi.instance.onFrameReceived(sender, data);
        _drainInbound();
        _drainOutbound();
        _drainDeliveryEvents();
        break;

      case 'onPeerRssi':
        final shortId = call.arguments['shortId'] as String?;
        final rssi = call.arguments['rssi'] as int?;
        if (shortId != null && rssi != null) {
          CoreFfi.instance.recordBleRssi(shortId, rssi);
        }
        break;

      case 'onResourceStateChanged':
        _lastBatteryPct = (call.arguments['batteryPct'] as int?) ?? -1;
        _lastCharging = (call.arguments['charging'] as int?) ?? -1;
        _lastPowerSave = (call.arguments['powerSave'] as int?) ?? -1;
        _lastThermal = (call.arguments['thermal'] as int?) ?? 0;
        CoreFfi.instance.updateResourceState(
          batteryPct: _lastBatteryPct,
          charging: _lastCharging,
          powerSave: _lastPowerSave,
          thermal: _lastThermal,
          userPaused: userPausedRelaying,
        );
        break;
    }
  }

  void dispose() {
    _isDisposed = true;
    _pumpTimer?.cancel();
    _channel.setMethodCallHandler(null);
    _messageController.close();
    _peersController.close();
    _deliveryEventController.close();
  }
}
