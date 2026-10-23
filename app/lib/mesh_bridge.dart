import 'dart:async';
import 'package:flutter/services.dart';
import 'core_ffi.dart';

/// Data model representing a verified mesh chat message.
class ChatMessage {
  final String msgId;
  final String sender;
  final String? recipient;
  final String text;
  final int hopCount;
  final bool isBroadcast;
  final DateTime timestamp;

  ChatMessage({
    required this.msgId,
    required this.sender,
    this.recipient,
    required this.text,
    required this.hopCount,
    required this.isBroadcast,
    DateTime? timestamp,
  }) : timestamp = timestamp ?? DateTime.now();
}

/// Enterprise mesh bridge coordinator linking Native BLE GATT and Rust bitchat2_core.
class MeshBridge {
  static const MethodChannel _channel = MethodChannel('com.bitchat.mesh/ble');

  final _messageController = StreamController<ChatMessage>.broadcast();
  final _peersController = StreamController<List<String>>.broadcast();

  final Set<String> _connectedPeers = {};
  final Set<String> _seenMsgIds = {};

  String myShortId = "Initializing...";
  String myPeerId = "Generating...";

  Stream<ChatMessage> get messages => _messageController.stream;
  Stream<List<String>> get peers => _peersController.stream;
  List<String> get currentPeers => _connectedPeers.toList();

  int transmissions = 0;
  int deliveries = 0;
  int duplicatesDropped = 0;
  int relaysSuppressed = 0;

  Timer? _pumpTimer;
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

  Future<void> startMesh() async {
    // 1. Initialize Rust bitchat2_core with Android storage path
    String storagePath = ".";
    try {
      final appPath = await _channel.invokeMethod<String>('getAppStoragePath');
      if (appPath != null && appPath.isNotEmpty) {
        storagePath = appPath;
      }
    } catch (_) {}

    final shortId = CoreFfi.instance.initCore(storagePath);
    if (shortId != null && shortId.isNotEmpty) {
      myShortId = shortId;
      myPeerId = CoreFfi.instance.getMyPeerId() ?? "Unknown";
    }

    // 2. Start native BLE Mesh Service with verified ShortId
    try {
      await _channel.invokeMethod('startBle', {'shortId': myShortId});
    } catch (e) {
      // Running on emulator or test environment without BLE
    }

    // 3. Start high-frequency discrete event pump for Rust outbound frames & inbound messages
    _pumpTimer?.cancel();
    _pumpTimer = Timer.periodic(const Duration(milliseconds: 50), (_) {
      _drainOutbound();
      _drainInbound();
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

  Future<void> sendDirect(String recipientShortId, String text) async {
    final msgId = CoreFfi.instance.sendTo(recipientShortId, text);
    if (msgId == null) {
      throw Exception("E2E session not ready. Noise handshake required before direct send.");
    }

    transmissions++;
    _seenMsgIds.add(msgId);
    _drainOutbound();

    if (!_isDisposed && !_messageController.isClosed) {
      _messageController.add(ChatMessage(
        msgId: msgId,
        sender: myShortId,
        recipient: recipientShortId,
        text: text,
        hopCount: 0,
        isBroadcast: false,
      ));
    }
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
          sender: m.sender,
          recipient: m.recipient,
          text: m.text,
          hopCount: m.hopCount,
          isBroadcast: m.isBroadcast,
        ));
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
        break;
    }
  }

  void dispose() {
    _isDisposed = true;
    _pumpTimer?.cancel();
    _channel.setMethodCallHandler(null);
    _messageController.close();
    _peersController.close();
  }
}
