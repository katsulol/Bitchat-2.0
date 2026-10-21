import 'dart:async';
import 'package:flutter/services.dart';

/// Data model representing a mesh chat message.
class ChatMessage {
  final String sender;
  final String? recipient;
  final String text;
  final int hopCount;
  final bool isBroadcast;
  final DateTime timestamp;

  ChatMessage({
    required this.sender,
    this.recipient,
    required this.text,
    required this.hopCount,
    required this.isBroadcast,
    DateTime? timestamp,
  }) : timestamp = timestamp ?? DateTime.now();
}

/// Mesh bridge coordinator managing native BLE MethodChannel and Rust core.
class MeshBridge {
  static const MethodChannel _channel = MethodChannel('com.bitchat.mesh/ble');

  final _messageController = StreamController<ChatMessage>.broadcast();
  final _peersController = StreamController<List<String>>.broadcast();

  final Set<String> _connectedPeers = {};
  String myShortId = "Initializing...";
  String myPeerId = "Generating...";

  Stream<ChatMessage> get messages => _messageController.stream;
  Stream<List<String>> get peers => _peersController.stream;
  List<String> get currentPeers => _connectedPeers.toList();

  int transmissions = 0;
  int deliveries = 0;
  int duplicatesDropped = 0;
  int relaysSuppressed = 0;

  MeshBridge() {
    _channel.setMethodCallHandler(_handleNativeCall);
  }

  Future<void> startMesh() async {
    // Generate deterministic/mock shortId for development if native bridge not built
    if (myShortId == "Initializing...") {
      myShortId = "A1B2C3D4E5F60708";
      myPeerId = "7f8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b";
    }

    try {
      await _channel.invokeMethod('startBle', {'shortId': myShortId});
    } catch (_) {
      // Running on emulator or desktop fallback
    }
  }

  Future<void> stopMesh() async {
    try {
      await _channel.invokeMethod('stopBle');
    } catch (_) {}
  }

  Future<void> sendBroadcast(String text) async {
    transmissions++;
    // In production, Rust node.send_broadcast() encodes envelope & splits fragments
    // Forward outbound frame to BLE channel
    final bytes = text.codeUnits;
    try {
      await _channel.invokeMethod('writeFrame', {
        'target': null,
        'data': Uint8List.fromList(bytes),
      });
    } catch (_) {}

    // Add local echo to message stream
    _messageController.add(ChatMessage(
      sender: myShortId,
      recipient: null,
      text: text,
      hopCount: 0,
      isBroadcast: true,
    ));
  }

  Future<void> sendDirect(String recipientShortId, String text) async {
    transmissions++;
    final bytes = text.codeUnits;
    try {
      await _channel.invokeMethod('writeFrame', {
        'target': recipientShortId,
        'data': Uint8List.fromList(bytes),
      });
    } catch (_) {}

    _messageController.add(ChatMessage(
      sender: myShortId,
      recipient: recipientShortId,
      text: text,
      hopCount: 0,
      isBroadcast: false,
    ));
  }

  Future<dynamic> _handleNativeCall(MethodCall call) async {
    switch (call.method) {
      case 'onPeerConnected':
        final address = call.arguments['address'] as String;
        _connectedPeers.add(address);
        _peersController.add(_connectedPeers.toList());
        break;

      case 'onPeerDisconnected':
        final address = call.arguments['address'] as String;
        _connectedPeers.remove(address);
        _peersController.add(_connectedPeers.toList());
        break;

      case 'onFrameReceived':
        deliveries++;
        final address = call.arguments['address'] as String;
        final data = call.arguments['data'] as Uint8List;
        final text = String.fromCharCodes(data);

        _messageController.add(ChatMessage(
          sender: address,
          text: text,
          hopCount: 1,
          isBroadcast: true,
        ));
        break;
    }
  }

  void dispose() {
    _messageController.close();
    _peersController.close();
  }
}
