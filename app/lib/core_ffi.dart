import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
import 'dart:typed_data';
import 'package:ffi/ffi.dart';

class RustOutboundFrame {
  final String? target;
  final Uint8List bytes;
  RustOutboundFrame({this.target, required this.bytes});
}

class RustInboundMessage {
  final String msgId;
  final String sender;
  final String? recipient;
  final String text;
  final int hopCount;
  final bool isBroadcast;

  RustInboundMessage({
    required this.msgId,
    required this.sender,
    this.recipient,
    required this.text,
    required this.hopCount,
    required this.isBroadcast,
  });

  factory RustInboundMessage.fromJson(Map<String, dynamic> json) {
    return RustInboundMessage(
      msgId: json['msg_id'] as String,
      sender: json['sender'] as String,
      recipient: json['recipient'] as String?,
      text: json['text'] as String,
      hopCount: json['hop_count'] as int,
      isBroadcast: json['is_broadcast'] as bool,
    );
  }
}

class CoreFfi {
  static final CoreFfi instance = CoreFfi._();
  CoreFfi._();

  DynamicLibrary? _lib;
  bool _initialized = false;

  late final BitchatInitDart _init;
  late final BitchatStringDart _myShortId;
  late final BitchatStringDart _myPeerId;
  late final BitchatSendBroadcastDart _sendBroadcast;
  late final BitchatSendToDart _sendTo;
  late final BitchatPeerDart _onPeerConnected;
  late final BitchatPeerDart _onPeerDisconnected;
  late final BitchatFrameRecvDart _onFrameReceived;
  late final BitchatPollOutboundDart _pollOutbound;
  late final BitchatPollInboundDart _pollInbound;
  late final BitchatFreeStringDart _freeString;
  late final BitchatFreeBytesDart _freeBytes;

  bool loadLibrary() {
    if (_initialized) return true;
    try {
      if (Platform.isAndroid) {
        _lib = DynamicLibrary.open('libbitchat2_core.so');
      } else {
        _lib = DynamicLibrary.process();
      }

      _init = _lib!.lookupFunction<BitchatInitC, BitchatInitDart>('bitchat_init');
      _myShortId = _lib!.lookupFunction<BitchatStringC, BitchatStringDart>('bitchat_my_short_id');
      _myPeerId = _lib!.lookupFunction<BitchatStringC, BitchatStringDart>('bitchat_my_peer_id');
      _sendBroadcast = _lib!.lookupFunction<BitchatSendBroadcastC, BitchatSendBroadcastDart>('bitchat_send_broadcast');
      _sendTo = _lib!.lookupFunction<BitchatSendToC, BitchatSendToDart>('bitchat_send_to');
      _onPeerConnected = _lib!.lookupFunction<BitchatPeerC, BitchatPeerDart>('bitchat_on_peer_connected');
      _onPeerDisconnected = _lib!.lookupFunction<BitchatPeerC, BitchatPeerDart>('bitchat_on_peer_disconnected');
      _onFrameReceived = _lib!.lookupFunction<BitchatFrameRecvC, BitchatFrameRecvDart>('bitchat_on_frame_received');
      _pollOutbound = _lib!.lookupFunction<BitchatPollOutboundC, BitchatPollOutboundDart>('bitchat_poll_outbound');
      _pollInbound = _lib!.lookupFunction<BitchatPollInboundC, BitchatPollInboundDart>('bitchat_poll_inbound');
      _freeString = _lib!.lookupFunction<BitchatFreeStringC, BitchatFreeStringDart>('bitchat_free_string');
      _freeBytes = _lib!.lookupFunction<BitchatFreeBytesC, BitchatFreeBytesDart>('bitchat_free_bytes');

      _initialized = true;
      return true;
    } catch (e) {
      return false;
    }
  }

  String? initCore(String storagePath) {
    if (!_initialized && !loadLibrary()) return null;
    final pathPtr = storagePath.toNativeUtf8();
    try {
      final resPtr = _init(pathPtr);
      if (resPtr == nullptr) return null;
      final shortId = resPtr.toDartString();
      _freeString(resPtr);
      return shortId;
    } finally {
      calloc.free(pathPtr);
    }
  }

  String? getMyShortId() {
    if (!_initialized) return null;
    final ptr = _myShortId();
    if (ptr == nullptr) return null;
    final id = ptr.toDartString();
    _freeString(ptr);
    return id;
  }

  String? getMyPeerId() {
    if (!_initialized) return null;
    final ptr = _myPeerId();
    if (ptr == nullptr) return null;
    final pk = ptr.toDartString();
    _freeString(ptr);
    return pk;
  }

  String? sendBroadcast(String text) {
    if (!_initialized) return null;
    final textPtr = text.toNativeUtf8();
    try {
      final ptr = _sendBroadcast(textPtr);
      if (ptr == nullptr) return null;
      final msgId = ptr.toDartString();
      _freeString(ptr);
      return msgId;
    } finally {
      calloc.free(textPtr);
    }
  }

  String? sendTo(String recipient, String text) {
    if (!_initialized) return null;
    final recipPtr = recipient.toNativeUtf8();
    final textPtr = text.toNativeUtf8();
    try {
      final ptr = _sendTo(recipPtr, textPtr);
      if (ptr == nullptr) return null;
      final msgId = ptr.toDartString();
      _freeString(ptr);
      return msgId;
    } finally {
      calloc.free(recipPtr);
      calloc.free(textPtr);
    }
  }

  void onPeerConnected(String shortId) {
    if (!_initialized) return;
    final idPtr = shortId.toNativeUtf8();
    try {
      _onPeerConnected(idPtr);
    } finally {
      calloc.free(idPtr);
    }
  }

  void onPeerDisconnected(String shortId) {
    if (!_initialized) return;
    final idPtr = shortId.toNativeUtf8();
    try {
      _onPeerDisconnected(idPtr);
    } finally {
      calloc.free(idPtr);
    }
  }

  void onFrameReceived(String shortId, Uint8List data) {
    if (!_initialized || data.isEmpty) return;
    final idPtr = shortId.toNativeUtf8();
    final dataPtr = calloc<Uint8>(data.length);
    dataPtr.asTypedList(data.length).setAll(0, data);
    try {
      _onFrameReceived(idPtr, dataPtr, data.length);
    } finally {
      calloc.free(idPtr);
      calloc.free(dataPtr);
    }
  }

  RustOutboundFrame? pollOutbound() {
    if (!_initialized) return null;
    final outTarget = calloc<Pointer<Utf8>>();
    final outData = calloc<Pointer<Uint8>>();
    final outLen = calloc<IntPtr>();

    try {
      final status = _pollOutbound(outTarget, outData, outLen);
      if (status == 1) {
        String? target;
        if (outTarget.value != nullptr) {
          target = outTarget.value.toDartString();
          _freeString(outTarget.value);
        }

        final len = outLen.value;
        final rawData = outData.value.asTypedList(len);
        final bytes = Uint8List.fromList(rawData);
        _freeBytes(outData.value, len);

        return RustOutboundFrame(target: target, bytes: bytes);
      }
      return null;
    } finally {
      calloc.free(outTarget);
      calloc.free(outData);
      calloc.free(outLen);
    }
  }

  RustInboundMessage? pollInbound() {
    if (!_initialized) return null;
    final outJson = calloc<Pointer<Utf8>>();
    try {
      final status = _pollInbound(outJson);
      if (status == 1 && outJson.value != nullptr) {
        final jsonStr = outJson.value.toDartString();
        _freeString(outJson.value);
        try {
          final Map<String, dynamic> parsed = jsonDecode(jsonStr);
          return RustInboundMessage.fromJson(parsed);
        } catch (_) {
          return null;
        }
      }
      return null;
    } finally {
      calloc.free(outJson);
    }
  }
}

// FFI native signatures
typedef BitchatInitC = Pointer<Utf8> Function(Pointer<Utf8> storagePath);
typedef BitchatInitDart = Pointer<Utf8> Function(Pointer<Utf8> storagePath);

typedef BitchatStringC = Pointer<Utf8> Function();
typedef BitchatStringDart = Pointer<Utf8> Function();

typedef BitchatSendBroadcastC = Pointer<Utf8> Function(Pointer<Utf8> text);
typedef BitchatSendBroadcastDart = Pointer<Utf8> Function(Pointer<Utf8> text);

typedef BitchatSendToC = Pointer<Utf8> Function(Pointer<Utf8> recipient, Pointer<Utf8> text);
typedef BitchatSendToDart = Pointer<Utf8> Function(Pointer<Utf8> recipient, Pointer<Utf8> text);

typedef BitchatPeerC = Int32 Function(Pointer<Utf8> shortId);
typedef BitchatPeerDart = int Function(Pointer<Utf8> shortId);

typedef BitchatFrameRecvC = Int32 Function(Pointer<Utf8> shortId, Pointer<Uint8> data, IntPtr len);
typedef BitchatFrameRecvDart = int Function(Pointer<Utf8> shortId, Pointer<Uint8> data, int len);

typedef BitchatPollOutboundC = Int32 Function(Pointer<Pointer<Utf8>> outTarget, Pointer<Pointer<Uint8>> outData, Pointer<IntPtr> outLen);
typedef BitchatPollOutboundDart = int Function(Pointer<Pointer<Utf8>> outTarget, Pointer<Pointer<Uint8>> outData, Pointer<IntPtr> outLen);

typedef BitchatPollInboundC = Int32 Function(Pointer<Pointer<Utf8>> outJson);
typedef BitchatPollInboundDart = int Function(Pointer<Pointer<Utf8>> outJson);

typedef BitchatFreeStringC = Void Function(Pointer<Utf8> ptr);
typedef BitchatFreeStringDart = void Function(Pointer<Utf8> ptr);

typedef BitchatFreeBytesC = Void Function(Pointer<Uint8> ptr, IntPtr len);
typedef BitchatFreeBytesDart = void Function(Pointer<Uint8> ptr, int len);
