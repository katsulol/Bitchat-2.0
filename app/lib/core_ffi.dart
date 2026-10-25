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

class RustGatewayStats {
  final bool isRunning;
  final String? endpointId;
  final int activeConnections;
  final int forwardsImported;
  final int forwardsExported;
  final int loopsPrevented;
  final int forwardsRejected;
  final String? lastError;

  RustGatewayStats({
    required this.isRunning,
    this.endpointId,
    required this.activeConnections,
    required this.forwardsImported,
    required this.forwardsExported,
    required this.loopsPrevented,
    required this.forwardsRejected,
    this.lastError,
  });

  factory RustGatewayStats.fromJson(Map<String, dynamic> json) {
    return RustGatewayStats(
      isRunning: json['is_running'] as bool? ?? false,
      endpointId: json['endpoint_id'] as String?,
      activeConnections: json['active_connections'] as int? ?? 0,
      forwardsImported: json['messages_imported'] as int? ?? 0,
      forwardsExported: json['messages_exported'] as int? ?? 0,
      loopsPrevented: json['loops_prevented'] as int? ?? 0,
      forwardsRejected: json['messages_rejected'] as int? ?? 0,
      lastError: json['last_error'] as String?,
    );
  }
}

class RustDeliveryEvent {
  final String logicalId;
  final String state;
  final String recipient;
  final int timestamp;

  RustDeliveryEvent({
    required this.logicalId,
    required this.state,
    required this.recipient,
    required this.timestamp,
  });

  factory RustDeliveryEvent.fromJson(Map<String, dynamic> json) {
    return RustDeliveryEvent(
      logicalId: json['logical_id'] as String? ?? '',
      state: json['state'] as String? ?? '',
      recipient: json['recipient'] as String? ?? '',
      timestamp: json['timestamp'] as int? ?? 0,
    );
  }
}

class RustDiagnostics {
  final String myShortId;
  final String myPeerId;
  final int connectedPeersCount;
  final int outboxQueuedCount;
  final int outboxInflightCount;
  final int deliveriesCount;
  final bool gatewayActive;
  final bool gatewayRelayDisabled;
  final String? lastRouteCandidate;

  RustDiagnostics({
    required this.myShortId,
    required this.myPeerId,
    required this.connectedPeersCount,
    required this.outboxQueuedCount,
    required this.outboxInflightCount,
    required this.deliveriesCount,
    required this.gatewayActive,
    required this.gatewayRelayDisabled,
    this.lastRouteCandidate,
  });

  factory RustDiagnostics.fromJson(Map<String, dynamic> json) {
    return RustDiagnostics(
      myShortId: json['my_short_id'] as String? ?? '',
      myPeerId: json['my_peer_id'] as String? ?? '',
      connectedPeersCount: json['connected_peers_count'] as int? ?? 0,
      outboxQueuedCount: json['outbox_queued_count'] as int? ?? 0,
      outboxInflightCount: json['outbox_inflight_count'] as int? ?? 0,
      deliveriesCount: json['deliveries_count'] as int? ?? 0,
      gatewayActive: json['gateway_active'] as bool? ?? false,
      gatewayRelayDisabled: json['gateway_relay_disabled'] as bool? ?? true,
      lastRouteCandidate: json['last_route_candidate'] as String?,
    );
  }
}

class CoreFfi {
  static final CoreFfi instance = CoreFfi._();
  CoreFfi._();

  DynamicLibrary? _lib;
  bool _initialized = false;

  late final BitchatInitDart _init;
  late final BitchatInitWithKeyDart _initWithKey;
  late final BitchatStringDart _myShortId;
  late final BitchatStringDart _myPeerId;
  late final BitchatSendBroadcastDart _sendBroadcast;
  late final BitchatSendToDart _sendTo;
  late final BitchatSendMessageDart _sendMessage;
  late final BitchatGetDeliveryStateDart _getDeliveryState;
  late final BitchatStringDart _pollDeliveryEvents;
  late final BitchatStringDart _getDiagnostics;
  late final BitchatPumpOutboxDart _pumpOutbox;
  late final BitchatPeerDart _initiateHandshake;
  late final BitchatPeerDart _isSessionReady;
  late final BitchatPeerDart _onPeerConnected;
  late final BitchatPeerDart _onPeerDisconnected;
  late final BitchatFrameRecvDart _onFrameReceived;
  late final BitchatPollOutboundDart _pollOutbound;
  late final BitchatPollInboundDart _pollInbound;
  late final BitchatFreeStringDart _freeString;
  late final BitchatFreeBytesDart _freeBytes;
  late final BitchatGatewayStartDart _gatewayStart;
  late final BitchatGatewayStopDart _gatewayStop;
  late final BitchatStringDart _gatewayExportContact;
  late final BitchatGatewayImportDart _gatewayImportAndApproveContact;
  late final BitchatGatewayConnectDart _gatewayConnectToContact;
  late final BitchatGatewaySetRouteDart _gatewaySetRoute;
  late final BitchatStringDart _gatewayGetStats;
  late final BitchatStringDart _gatewayGetRoutes;

  bool loadLibrary() {
    if (_initialized) return true;
    try {
      if (Platform.isAndroid) {
        _lib = DynamicLibrary.open('libbitchat2_core.so');
      } else {
        _lib = DynamicLibrary.process();
      }

      _init = _lib!.lookupFunction<BitchatInitC, BitchatInitDart>('bitchat_init');
      _initWithKey = _lib!.lookupFunction<BitchatInitWithKeyC, BitchatInitWithKeyDart>('bitchat_init_with_key');
      _myShortId = _lib!.lookupFunction<BitchatStringC, BitchatStringDart>('bitchat_my_short_id');
      _myPeerId = _lib!.lookupFunction<BitchatStringC, BitchatStringDart>('bitchat_my_peer_id');
      _sendBroadcast = _lib!.lookupFunction<BitchatSendBroadcastC, BitchatSendBroadcastDart>('bitchat_send_broadcast');
      _sendTo = _lib!.lookupFunction<BitchatSendToC, BitchatSendToDart>('bitchat_send_to');
      _sendMessage = _lib!.lookupFunction<BitchatSendMessageC, BitchatSendMessageDart>('bitchat_send_message');
      _getDeliveryState = _lib!.lookupFunction<BitchatGetDeliveryStateC, BitchatGetDeliveryStateDart>('bitchat_get_delivery_state');
      _pollDeliveryEvents = _lib!.lookupFunction<BitchatStringC, BitchatStringDart>('bitchat_poll_delivery_events');
      _getDiagnostics = _lib!.lookupFunction<BitchatStringC, BitchatStringDart>('bitchat_get_diagnostics');
      _pumpOutbox = _lib!.lookupFunction<BitchatPumpOutboxC, BitchatPumpOutboxDart>('bitchat_pump_outbox');
      _initiateHandshake = _lib!.lookupFunction<BitchatPeerC, BitchatPeerDart>('bitchat_initiate_handshake');
      _isSessionReady = _lib!.lookupFunction<BitchatPeerC, BitchatPeerDart>('bitchat_is_session_ready');
      _onPeerConnected = _lib!.lookupFunction<BitchatPeerC, BitchatPeerDart>('bitchat_on_peer_connected');
      _onPeerDisconnected = _lib!.lookupFunction<BitchatPeerC, BitchatPeerDart>('bitchat_on_peer_disconnected');
      _onFrameReceived = _lib!.lookupFunction<BitchatFrameRecvC, BitchatFrameRecvDart>('bitchat_on_frame_received');
      _pollOutbound = _lib!.lookupFunction<BitchatPollOutboundC, BitchatPollOutboundDart>('bitchat_poll_outbound');
      _pollInbound = _lib!.lookupFunction<BitchatPollInboundC, BitchatPollInboundDart>('bitchat_poll_inbound');
      _freeString = _lib!.lookupFunction<BitchatFreeStringC, BitchatFreeStringDart>('bitchat_free_string');
      _freeBytes = _lib!.lookupFunction<BitchatFreeBytesC, BitchatFreeBytesDart>('bitchat_free_bytes');
      _gatewayStart = _lib!.lookupFunction<BitchatGatewayStartC, BitchatGatewayStartDart>('bitchat_gateway_start');
      _gatewayStop = _lib!.lookupFunction<BitchatGatewayStopC, BitchatGatewayStopDart>('bitchat_gateway_stop');
      _gatewayExportContact = _lib!.lookupFunction<BitchatStringC, BitchatStringDart>('bitchat_gateway_export_contact');
      _gatewayImportAndApproveContact = _lib!.lookupFunction<BitchatGatewayImportC, BitchatGatewayImportDart>('bitchat_gateway_import_and_approve_contact');
      _gatewayConnectToContact = _lib!.lookupFunction<BitchatGatewayConnectC, BitchatGatewayConnectDart>('bitchat_gateway_connect_to_contact');
      _gatewaySetRoute = _lib!.lookupFunction<BitchatGatewaySetRouteC, BitchatGatewaySetRouteDart>('bitchat_gateway_set_route');
      _gatewayGetStats = _lib!.lookupFunction<BitchatStringC, BitchatStringDart>('bitchat_gateway_get_stats');
      _gatewayGetRoutes = _lib!.lookupFunction<BitchatStringC, BitchatStringDart>('bitchat_gateway_get_routes');

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

  bool initiateHandshake(String recipientShortId) {
    if (!_initialized) return false;
    final recipPtr = recipientShortId.toNativeUtf8();
    try {
      return _initiateHandshake(recipPtr) == 0;
    } finally {
      calloc.free(recipPtr);
    }
  }

  bool isSessionReady(String recipientShortId) {
    if (!_initialized) return false;
    final recipPtr = recipientShortId.toNativeUtf8();
    try {
      return _isSessionReady(recipPtr) == 1;
    } finally {
      calloc.free(recipPtr);
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

  String? gatewayStart([String? relayMode]) {
    if (!_initialized) return null;
    final modePtr = relayMode != null ? relayMode.toNativeUtf8() : nullptr;
    try {
      final ptr = _gatewayStart(modePtr);
      if (ptr == nullptr) return null;
      final epId = ptr.toDartString();
      _freeString(ptr);
      return epId;
    } finally {
      if (modePtr != nullptr) calloc.free(modePtr);
    }
  }

  bool gatewayStop() {
    if (!_initialized) return false;
    return _gatewayStop() == 0;
  }

  String? gatewayExportContact() {
    if (!_initialized) return null;
    final ptr = _gatewayExportContact();
    if (ptr == nullptr) return null;
    final uri = ptr.toDartString();
    _freeString(ptr);
    return uri;
  }

  String? gatewayImportAndApproveContact(String contactStr) {
    if (!_initialized) return null;
    final contactPtr = contactStr.toNativeUtf8();
    try {
      final ptr = _gatewayImportAndApproveContact(contactPtr);
      if (ptr == nullptr) return null;
      final pk = ptr.toDartString();
      _freeString(ptr);
      return pk;
    } finally {
      calloc.free(contactPtr);
    }
  }

  bool gatewayConnectToContact(String contactStr) {
    if (!_initialized) return false;
    final contactPtr = contactStr.toNativeUtf8();
    try {
      return _gatewayConnectToContact(contactPtr) == 0;
    } finally {
      calloc.free(contactPtr);
    }
  }

  bool gatewaySetRoute(String recipientShortId, String targetEndpointIdHex) {
    if (!_initialized) return false;
    final recipPtr = recipientShortId.toNativeUtf8();
    final targetPtr = targetEndpointIdHex.toNativeUtf8();
    try {
      return _gatewaySetRoute(recipPtr, targetPtr) == 0;
    } finally {
      calloc.free(recipPtr);
      calloc.free(targetPtr);
    }
  }

  RustGatewayStats? gatewayGetStats() {
    if (!_initialized) return null;
    final ptr = _gatewayGetStats();
    if (ptr == nullptr) return null;
    final jsonStr = ptr.toDartString();
    _freeString(ptr);
    try {
      final Map<String, dynamic> parsed = jsonDecode(jsonStr);
      return RustGatewayStats.fromJson(parsed);
    } catch (_) {
      return null;
    }
  }

  List<String> gatewayGetRoutes() {
    if (!_initialized) return [];
    final ptr = _gatewayGetRoutes();
    if (ptr == nullptr) return [];
    final raw = ptr.toDartString();
    _freeString(ptr);
    if (raw.isEmpty) return [];
    return raw.split(',').where((s) => s.isNotEmpty).toList();
  }

  String? initWithKey(String storagePath, Uint8List? dek, [int version = 1]) {
    if (!_initialized && !loadLibrary()) return null;
    final pathPtr = storagePath.toNativeUtf8();
    Pointer<Uint8> keyPtr = nullptr;
    if (dek != null && dek.length == 32) {
      keyPtr = calloc<Uint8>(32);
      final keyList = keyPtr.asTypedList(32);
      keyList.setAll(0, dek);
    }
    try {
      final resPtr = _initWithKey(pathPtr, keyPtr, version);
      if (resPtr == nullptr) return null;
      final shortId = resPtr.toDartString();
      _freeString(resPtr);
      return shortId;
    } finally {
      calloc.free(pathPtr);
      if (keyPtr != nullptr) calloc.free(keyPtr);
    }
  }

  String? sendMessage(String recipient, String text, int mode) {
    if (!_initialized) return null;
    final recipPtr = recipient.toNativeUtf8();
    final textPtr = text.toNativeUtf8();
    try {
      final ptr = _sendMessage(recipPtr, textPtr, mode);
      if (ptr == nullptr) return null;
      final logicalId = ptr.toDartString();
      _freeString(ptr);
      return logicalId;
    } finally {
      calloc.free(recipPtr);
      calloc.free(textPtr);
    }
  }

  String? getDeliveryState(String logicalId) {
    if (!_initialized) return null;
    final idPtr = logicalId.toNativeUtf8();
    try {
      final ptr = _getDeliveryState(idPtr);
      if (ptr == nullptr) return null;
      final state = ptr.toDartString();
      _freeString(ptr);
      return state;
    } finally {
      calloc.free(idPtr);
    }
  }

  List<RustDeliveryEvent> pollDeliveryEvents() {
    if (!_initialized) return [];
    final ptr = _pollDeliveryEvents();
    if (ptr == nullptr) return [];
    final jsonStr = ptr.toDartString();
    _freeString(ptr);
    try {
      final List<dynamic> list = jsonDecode(jsonStr);
      return list.map((e) => RustDeliveryEvent.fromJson(e as Map<String, dynamic>)).toList();
    } catch (_) {
      return [];
    }
  }

  RustDiagnostics? getDiagnostics() {
    if (!_initialized) return null;
    final ptr = _getDiagnostics();
    if (ptr == nullptr) return null;
    final jsonStr = ptr.toDartString();
    _freeString(ptr);
    try {
      final Map<String, dynamic> map = jsonDecode(jsonStr);
      return RustDiagnostics.fromJson(map);
    } catch (_) {
      return null;
    }
  }

  int pumpOutbox() {
    if (!_initialized) return -1;
    return _pumpOutbox();
  }
}

// FFI native signatures
typedef BitchatInitC = Pointer<Utf8> Function(Pointer<Utf8> storagePath);
typedef BitchatInitDart = Pointer<Utf8> Function(Pointer<Utf8> storagePath);

typedef BitchatInitWithKeyC = Pointer<Utf8> Function(Pointer<Utf8> storagePath, Pointer<Uint8> keyPtr, Uint32 version);
typedef BitchatInitWithKeyDart = Pointer<Utf8> Function(Pointer<Utf8> storagePath, Pointer<Uint8> keyPtr, int version);

typedef BitchatSendMessageC = Pointer<Utf8> Function(Pointer<Utf8> recipient, Pointer<Utf8> text, Uint8 mode);
typedef BitchatSendMessageDart = Pointer<Utf8> Function(Pointer<Utf8> recipient, Pointer<Utf8> text, int mode);

typedef BitchatGetDeliveryStateC = Pointer<Utf8> Function(Pointer<Utf8> logicalId);
typedef BitchatGetDeliveryStateDart = Pointer<Utf8> Function(Pointer<Utf8> logicalId);

typedef BitchatPumpOutboxC = Int32 Function();
typedef BitchatPumpOutboxDart = int Function();

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

typedef BitchatGatewayStartC = Pointer<Utf8> Function(Pointer<Utf8> relayMode);
typedef BitchatGatewayStartDart = Pointer<Utf8> Function(Pointer<Utf8> relayMode);

typedef BitchatGatewayStopC = Int32 Function();
typedef BitchatGatewayStopDart = int Function();

typedef BitchatGatewayImportC = Pointer<Utf8> Function(Pointer<Utf8> contactStr);
typedef BitchatGatewayImportDart = Pointer<Utf8> Function(Pointer<Utf8> contactStr);

typedef BitchatGatewayConnectC = Int32 Function(Pointer<Utf8> contactStr);
typedef BitchatGatewayConnectDart = int Function(Pointer<Utf8> contactStr);

typedef BitchatGatewaySetRouteC = Int32 Function(Pointer<Utf8> recipient, Pointer<Utf8> target);
typedef BitchatGatewaySetRouteDart = int Function(Pointer<Utf8> recipient, Pointer<Utf8> target);
