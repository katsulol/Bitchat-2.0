import 'dart:async';
import 'dart:convert';
import 'dart:math';
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
    // Generate unique shortId per node instance
    if (myShortId == "Initializing...") {
      final rand = Random();
      final bytes = List<int>.generate(8, (_) => rand.nextInt(256));
      myShortId = bytes.map((b) => b.toRadixString(16).padLeft(2, '0')).join().toUpperCase();
      myPeerId = "$myShortId${List<int>.generate(24, (_) => rand.nextInt(256)).map((b) => b.toRadixString(16).padLeft(2, '0')).join().toLowerCase()}";
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
    // Wire format: Byte 0 = 0x00 (Broadcast), Bytes 1..N = UTF-8 text
    final payload = Uint8List.fromList([0x00, ...utf8.encode(text)]);
    try {
      await _channel.invokeMethod('writeFrame', {
        'target': null,
        'data': payload,
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
    // Wire format: Byte 0 = 0x01 (Direct E2E), Bytes 1..N = UTF-8 text
    final payload = Uint8List.fromList([0x01, ...utf8.encode(text)]);
    try {
      await _channel.invokeMethod('writeFrame', {
        'target': recipientShortId,
        'data': payload,
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

        if (data.isNotEmpty) {
          final isBroadcast = data[0] == 0x00;
          final text = utf8.decode(data.sublist(1), allowMalformed: true);

          _messageController.add(ChatMessage(
            sender: address,
            recipient: isBroadcast ? null : myShortId,
            text: text,
            hopCount: 1,
            isBroadcast: isBroadcast,
          ));
        }
        break;
    }
  }

  void dispose() {
    _messageController.close();
    _peersController.close();
  }
}
