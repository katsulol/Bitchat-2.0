import 'dart:async';
import 'dart:math';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:mobile_scanner/mobile_scanner.dart';
import 'package:qr_flutter/qr_flutter.dart';
import 'mesh_bridge.dart';
import 'delivery_explanation.dart';

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  runApp(const BitchatApp());
}

class BitchatApp extends StatelessWidget {
  final MeshBridge? bridge;
  const BitchatApp({super.key, this.bridge});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Bitchat 2.0 Mesh',
      debugShowCheckedModeBanner: false,
      theme: ThemeData(
        brightness: Brightness.dark,
        scaffoldBackgroundColor: const Color(0xFF121212),
        colorScheme: const ColorScheme.dark(
          primary: Color(0xFF4CAF50),
          secondary: Color(0xFF81C784),
          surface: Color(0xFF1E1E1E),
        ),
        useMaterial3: true,
      ),
      home: MeshChatScreen(bridge: bridge),
    );
  }
}

class MeshChatScreen extends StatefulWidget {
  final MeshBridge? bridge;
  const MeshChatScreen({super.key, this.bridge});

  @override
  State<MeshChatScreen> createState() => _MeshChatScreenState();
}

class _MeshChatScreenState extends State<MeshChatScreen> {
  late final MeshBridge _bridge;
  late final bool _ownsBridge;
  final TextEditingController _inputController = TextEditingController();
  final List<ChatMessage> _messages = [];
  List<String> _peers = [];
  String? _selectedRecipient; // null = Broadcast
  bool _isPrivacyMode = false;
  int? _selectedDeadlineSecs;
  bool _lastSessionReady = false;
  Timer? _pollTimer;
  Timer? _gatewayDialogTimer;
  StreamSubscription<ChatMessage>? _msgSub;
  StreamSubscription<List<String>>? _peersSub;
  StreamSubscription<dynamic>? _deliverySub;
  StreamSubscription<void>? _stateSub;

  @override
  void initState() {
    super.initState();
    _ownsBridge = widget.bridge == null;
    _bridge = widget.bridge ?? MeshBridge();
    _bridge.startMesh().then((_) {
      if (mounted) {
        setState(() {});
      }
    });

    _stateSub = _bridge.stateChanges.listen((_) {
      if (mounted) {
        setState(() {});
      }
    });

    _msgSub = _bridge.messages.listen((msg) {
      if (!mounted) return;
      setState(() {
        _messages.add(msg);
        if (_messages.length > 500) {
          _messages.removeRange(0, _messages.length - 500);
        }
      });
    });

    _peersSub = _bridge.peers.listen((peers) {
      if (!mounted) return;
      setState(() {
        _peers = peers;
        // corrections.md: Never auto-reset selected recipient to broadcast on disconnect!
      });
    });

    _deliverySub = _bridge.deliveryEvents.listen((event) {
      if (!mounted) return;
      setState(() {
        for (final m in _messages) {
          if (m.logicalId == event.logicalId) {
            m.deliveryState = event.state;
            m.deliveryReason = event.reason;
            m.observedTransport = event.transport;
            m.attemptCount = event.attemptCount;
            if (event.expiresAt > 0) {
              m.expiresAtUnix = event.expiresAt;
            }
          }
        }
      });
    });

    _pollTimer = Timer.periodic(const Duration(milliseconds: 400), (_) {
      if (mounted) {
        final ready = _selectedRecipient != null ? _bridge.isSessionReady(_selectedRecipient!) : false;
        final gwPeersCount = _bridge.getGatewayRoutes().length;
        if (ready != _lastSessionReady || gwPeersCount > 0) {
          setState(() {
            _lastSessionReady = ready;
          });
        }
      }
    });
  }

  Future<void> _sendMessage() async {
    final text = _inputController.text.trim();
    if (text.isEmpty) return;

    try {
      if (_selectedRecipient == null) {
        await _bridge.sendBroadcast(text);
      } else {
        await _bridge.sendMessage(
          _selectedRecipient!,
          text,
          isPrivacy: _isPrivacyMode,
          deadlineSecs: _selectedDeadlineSecs,
        );
      }
      _inputController.clear();
    } catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(
            content: Text('Send failed: ${e.toString().replaceAll("Exception: ", "")}'),
            backgroundColor: Colors.red.shade900,
          ),
        );
      }
    }
  }

  @override
  void dispose() {
    _pollTimer?.cancel();
    _gatewayDialogTimer?.cancel();
    _gatewayDialogTimer = null;
    _msgSub?.cancel();
    _peersSub?.cancel();
    _deliverySub?.cancel();
    _stateSub?.cancel();
    if (_ownsBridge) {
      _bridge.stopMesh();
      _bridge.dispose();
    }
    _inputController.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        backgroundColor: const Color(0xFF1E1E1E),
        title: const Text(
          'Bitchat 2.0 Mesh',
          style: TextStyle(fontSize: 18, fontWeight: FontWeight.bold),
        ),
        actions: [
          IconButton(
            icon: const Icon(Icons.qr_code_scanner),
            tooltip: 'Scan QR Code',
            onPressed: _openQrScanner,
          ),
          IconButton(
            icon: const Icon(Icons.refresh),
            tooltip: 'Rescan Mesh',
            onPressed: () {
              _bridge.startMesh();
              ScaffoldMessenger.of(context).showSnackBar(
                const SnackBar(
                  content: Text('Rescanning BLE mesh...'),
                  duration: Duration(seconds: 1),
                ),
              );
            },
          ),
          IconButton(
            icon: const Icon(Icons.verified_user_outlined),
            tooltip: 'Trust & Vouchers',
            onPressed: _showTrustAndVouchersDialog,
          ),
          IconButton(
            icon: const Icon(Icons.cloud_sync_outlined),
            tooltip: 'Iroh Gateway',
            onPressed: _showGatewayDialog,
          ),
          IconButton(
            icon: const Icon(Icons.info_outline),
            onPressed: _showNodeDetails,
          ),
        ],
      ),
      body: Column(
        children: [
          // Body Header: Node Identity & Mesh Status
          Container(
            width: double.infinity,
            padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 8),
            color: const Color(0xFF1E1E1E),
            child: Wrap(
              spacing: 8,
              runSpacing: 4,
              alignment: WrapAlignment.spaceBetween,
              crossAxisAlignment: WrapCrossAlignment.center,
              children: [
                ConstrainedBox(
                  constraints: BoxConstraints(
                    maxWidth: (MediaQuery.sizeOf(context).width - 28).clamp(80.0, double.infinity),
                  ),
                  child: Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      const Text('My ID: ', style: TextStyle(fontSize: 12, color: Colors.grey)),
                      Flexible(
                        child: Text(
                          _bridge.myShortId,
                          style: const TextStyle(
                            fontSize: 13,
                            fontFamily: 'monospace',
                            fontWeight: FontWeight.bold,
                            color: Color(0xFF81C784),
                          ),
                          overflow: TextOverflow.ellipsis,
                        ),
                      ),
                      const SizedBox(width: 6),
                      InkWell(
                        onTap: () {
                          Clipboard.setData(ClipboardData(text: _bridge.myShortId));
                          ScaffoldMessenger.of(context).showSnackBar(
                            const SnackBar(content: Text('ShortId copied to clipboard')),
                          );
                        },
                        child: const Padding(
                          padding: EdgeInsets.all(2.0),
                          child: Icon(Icons.copy, size: 14, color: Colors.grey),
                        ),
                      ),
                      const SizedBox(width: 6),
                      InkWell(
                        onTap: _showMyIdQrDialog,
                        child: const Padding(
                          padding: EdgeInsets.all(2.0),
                          child: Icon(Icons.qr_code, size: 16, color: Color(0xFF81C784)),
                        ),
                      ),
                    ],
                  ),
                ),
                Container(
                  padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 3),
                  decoration: BoxDecoration(
                    color: const Color(0xFF2E7D32).withValues(alpha: 0.2),
                    borderRadius: BorderRadius.circular(12),
                    border: Border.all(color: const Color(0xFF4CAF50), width: 0.8),
                  ),
                  child: Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Container(
                        width: 6,
                        height: 6,
                        decoration: const BoxDecoration(
                          color: Color(0xFF4CAF50),
                          shape: BoxShape.circle,
                        ),
                      ),
                      const SizedBox(width: 5),
                      Text(
                        '${_peers.length} peers',
                        style: const TextStyle(fontSize: 11, color: Color(0xFF81C784), fontWeight: FontWeight.w600),
                      ),
                    ],
                  ),
                ),
              ],
            ),
          ),
          if (_bridge.hasAbiError)
            Container(
              width: double.infinity,
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
              color: Colors.red.shade900,
              child: Row(
                children: [
                  const Icon(Icons.error_outline, size: 16, color: Colors.white),
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(
                      'Native ABI mismatch (fail-closed): ${_bridge.lastAbiError}',
                      style: const TextStyle(fontSize: 12, color: Colors.white),
                    ),
                  ),
                ],
              ),
            ),
          if (_bridge.permissionsDenied)
            Container(
              width: double.infinity,
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
              color: Colors.orange.shade900.withValues(alpha: 0.9),
              child: Row(
                children: [
                  const Icon(Icons.bluetooth_disabled, size: 16, color: Colors.white),
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(
                      _bridge.lastPermissionError ??
                          'Bluetooth / Location permissions denied.',
                      style: const TextStyle(fontSize: 12, color: Colors.white),
                    ),
                  ),
                  TextButton(
                    onPressed: _bridge.retryPermissions,
                    child: const Text(
                      'Retry',
                      style: TextStyle(fontSize: 12, color: Colors.amberAccent),
                    ),
                  ),
                ],
              ),
            ),
          if (_bridge.hasKeystoreError)
            Container(
              width: double.infinity,
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
              color: Colors.red.shade900.withValues(alpha: 0.9),
              child: Row(
                children: [
                  const Icon(Icons.lock_outline, size: 16, color: Colors.white),
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(
                      'Encrypted storage locked (fail-closed): ${_bridge.lastKeystoreError}',
                      style: const TextStyle(fontSize: 12, color: Colors.white),
                    ),
                  ),
                ],
              ),
            ),
          // Neighbors bar
          Container(
            padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
            color: const Color(0xFF181818),
            child: Row(
              children: [
                Icon(
                  Icons.bluetooth,
                  size: 18,
                  color: _peers.isNotEmpty ? Colors.blue : Colors.grey,
                ),
                const SizedBox(width: 6),
                Flexible(
                  child: Text(
                    '${_peers.length} Neighbors:',
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: const TextStyle(fontSize: 13, fontWeight: FontWeight.bold),
                  ),
                ),
                const SizedBox(width: 6),
                Expanded(
                  child: _peers.isEmpty
                      ? const Text(
                          'Scanning for BLE mesh peers...',
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: TextStyle(fontSize: 12, color: Colors.grey, fontStyle: FontStyle.italic),
                        )
                      : SingleChildScrollView(
                          scrollDirection: Axis.horizontal,
                          child: Row(
                            children: _peers.map((peer) {
                              final isSelected = _selectedRecipient == peer;
                              final prox = _bridge.getPeerProximity(peer);
                              final shortLabel = peer.length > 8 ? peer.substring(0, 8) : peer;
                              final chipText = prox == 'Unknown' ? shortLabel : '$shortLabel ($prox)';
                              return Padding(
                                padding: const EdgeInsets.only(right: 6),
                                child: Tooltip(
                                  message: 'Signal category: $prox (coarse local BLE RSSI category, not distance in meters)',
                                  child: FilterChip(
                                    selected: isSelected,
                                    label: Text(
                                      chipText,
                                      style: const TextStyle(fontSize: 11, fontFamily: 'monospace'),
                                    ),
                                    onSelected: (selected) {
                                      setState(() {
                                        _selectedRecipient = selected ? peer : null;
                                      });
                                      if (selected) {
                                        _bridge.initiateHandshake(peer);
                                      }
                                    },
                                  ),
                                ),
                              );
                            }).toList(),
                          ),
                        ),
                ),
              ],
            ),
          ),

          // Gateway routes bar (peers reachable over Iroh QUIC tunnels)
          Builder(
            builder: (ctx) {
              final gwRoutes = _bridge.getGatewayRoutes();
              if (gwRoutes.isEmpty) return const SizedBox.shrink();
              return Container(
                padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
                color: const Color(0xFF141A18),
                child: Row(
                  children: [
                    const Icon(Icons.public, size: 16, color: Colors.tealAccent),
                    const SizedBox(width: 6),
                    Flexible(
                      child: Text(
                        '${gwRoutes.length} Gateway Peers:',
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: const TextStyle(fontSize: 12, fontWeight: FontWeight.bold, color: Colors.tealAccent),
                      ),
                    ),
                    const SizedBox(width: 8),
                    Expanded(
                      child: SingleChildScrollView(
                        scrollDirection: Axis.horizontal,
                        child: Row(
                          children: gwRoutes.map((peer) {
                            final isSelected = _selectedRecipient == peer;
                            return Padding(
                              padding: const EdgeInsets.only(right: 6),
                              child: FilterChip(
                                selected: isSelected,
                                label: Text(
                                  peer.length > 8 ? peer.substring(0, 8) : peer,
                                  style: const TextStyle(fontSize: 11, fontFamily: 'monospace'),
                                ),
                                onSelected: (selected) {
                                  setState(() {
                                    _selectedRecipient = selected ? peer : null;
                                  });
                                  if (selected) {
                                    _bridge.initiateHandshake(peer);
                                  }
                                },
                              ),
                            );
                          }).toList(),
                        ),
                      ),
                    ),
                  ],
                ),
              );
            },
          ),

          // Message recipient indicator banner (shows direct peer / gateway route / offline status + proximity + volatile state)
          if (_selectedRecipient != null)
            Builder(
              builder: (ctx) {
                final isDirect = _peers.contains(_selectedRecipient);
                final isGateway = _bridge.getGatewayRoutes().contains(_selectedRecipient);
                final color = isDirect
                    ? Colors.blueAccent
                    : (isGateway ? Colors.tealAccent : Colors.orangeAccent);
                final bgColor = isDirect
                    ? Colors.blue.shade900.withAlpha(80)
                    : (isGateway ? Colors.teal.shade900.withAlpha(80) : Colors.orange.shade900.withAlpha(80));
                final icon = isDirect
                    ? Icons.send
                    : (isGateway ? Icons.public : Icons.cloud_off);
                final isReady = _bridge.isSessionReady(_selectedRecipient!);
                final secBadge = isReady ? ' (E2E Encrypted)' : ' (Securing...)';
                final prox = isDirect ? _bridge.getPeerProximity(_selectedRecipient!) : 'Unknown';
                final proxBadge = (isDirect && prox != 'Unknown') ? ' [$prox]' : '';
                final volState = _bridge.getVolatileState(_selectedRecipient!);
                final volBadge = volState.state != 'NotRequested'
                    ? ' [Volatile: ${volState.state} (${volState.retentionSecs}s)]'
                    : '';
                final label = (isDirect
                    ? 'Direct to: ${_selectedRecipient!.length > 8 ? _selectedRecipient!.substring(0, 8) : _selectedRecipient}'
                    : (isGateway
                        ? 'Via Gateway: ${_selectedRecipient!.length > 8 ? _selectedRecipient!.substring(0, 8) : _selectedRecipient}'
                        : 'Peer ${_selectedRecipient!.length > 8 ? _selectedRecipient!.substring(0, 8) : _selectedRecipient} (Offline)')) + secBadge + proxBadge + volBadge;

                return Column(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Container(
                      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 6),
                      color: bgColor,
                      child: Row(
                        children: [
                          Icon(icon, size: 14, color: color),
                          const SizedBox(width: 6),
                          Expanded(
                            child: Text(
                              label,
                              style: TextStyle(fontSize: 11, color: color),
                              overflow: TextOverflow.ellipsis,
                            ),
                          ),
                          const SizedBox(width: 6),
                          GestureDetector(
                            onTap: () {
                              _bridge.setSecureScreen(false);
                              setState(() => _selectedRecipient = null);
                            },
                            child: const Text('Cancel', style: TextStyle(fontSize: 12, color: Colors.white70)),
                          ),
                        ],
                      ),
                    ),
                    if (volState.state != 'NotRequested')
                      Container(
                        width: double.infinity,
                        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 4),
                        color: Colors.deepOrange.shade900.withAlpha(110),
                        child: Text(
                          volState.state == 'UnsupportedByPeer'
                              ? 'Volatile chat unsupported by peer: sending blocked (no silent downgrade). Disable volatile mode explicitly to send persistent E2EE.'
                              : volState.state == 'Pending'
                                  ? 'Volatile chat negotiating: sending blocked until peer confirms support (no silent downgrade).'
                                  : 'Volatile Chat (${volState.retentionSecs}s): local RAM-only retention; remote deletion depends on peer compliance and cannot prevent screenshots or modified clients.',
                          style: const TextStyle(fontSize: 10, color: Colors.orangeAccent),
                        ),
                      ),
                  ],
                );
              },
            ),

          // Message feed
          Expanded(
            child: _messages.isEmpty
                ? Center(
                    child: SingleChildScrollView(
                      padding: const EdgeInsets.all(12),
                      child: Column(
                        mainAxisAlignment: MainAxisAlignment.center,
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          Icon(Icons.wifi_tethering, size: 48, color: Colors.grey.shade700),
                          const SizedBox(height: 12),
                          const Text(
                            'No mesh messages yet.\nBroadcast or select a neighbor to chat.',
                            textAlign: TextAlign.center,
                            style: TextStyle(color: Colors.grey),
                          ),
                        ],
                      ),
                    ),
                  )
                : ListView.builder(
                    padding: const EdgeInsets.all(12),
                    itemCount: _messages.length,
                    itemBuilder: (context, index) {
                      final msg = _messages[index];
                      final isMe = msg.sender == _bridge.myShortId;

                      return Align(
                        alignment: isMe ? Alignment.centerRight : Alignment.centerLeft,
                        child: Container(
                          margin: const EdgeInsets.only(bottom: 8),
                          padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
                          constraints: BoxConstraints(
                            maxWidth: MediaQuery.of(context).size.width * 0.78,
                          ),
                          decoration: BoxDecoration(
                            color: isMe ? const Color(0xFF2E7D32) : const Color(0xFF2C2C2C),
                            borderRadius: BorderRadius.circular(12),
                          ),
                          child: Column(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            children: [
                              Row(
                                mainAxisSize: MainAxisSize.min,
                                children: [
                                  Text(
                                    isMe ? 'You' : (msg.sender.length > 8 ? msg.sender.substring(0, 8) : msg.sender),
                                    style: TextStyle(
                                      fontSize: 11,
                                      fontWeight: FontWeight.bold,
                                      color: isMe ? Colors.green.shade200 : Colors.blue.shade200,
                                      fontFamily: 'monospace',
                                    ),
                                  ),
                                  const SizedBox(width: 6),
                                  Container(
                                    padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 1),
                                    decoration: BoxDecoration(
                                      color: msg.isBroadcast
                                          ? Colors.amber.shade900
                                          : (msg.isPrivacy ? Colors.teal.shade800 : Colors.purple.shade900),
                                      borderRadius: BorderRadius.circular(4),
                                    ),
                                    child: Text(
                                      msg.isBroadcast ? 'PUB' : (msg.isPrivacy ? 'PRIVACY' : 'DIRECT'),
                                      style: const TextStyle(fontSize: 9, fontWeight: FontWeight.bold),
                                    ),
                                  ),
                                  const SizedBox(width: 4),
                                  Text(
                                    '${msg.hopCount}h',
                                    style: const TextStyle(fontSize: 9, color: Colors.white54),
                                  ),
                                ],
                              ),
                              const SizedBox(height: 4),
                              Text(
                                msg.text,
                                style: const TextStyle(fontSize: 14),
                              ),
                              if (isMe && !msg.isBroadcast) ...[
                                const SizedBox(height: 6),
                                Builder(
                                  builder: (ctx) {
                                    final explanation = DeliveryExplanation.explain(
                                      state: msg.deliveryState,
                                      reason: msg.deliveryReason,
                                      transport: msg.observedTransport,
                                      attemptCount: msg.attemptCount,
                                    );
                                    final transportLabel =
                                        DeliveryExplanation.formatTransport(msg.observedTransport);
                                    final remLabel =
                                        DeliveryExplanation.formatRemainingDeadline(msg.expiresAtUnix);
                                    final canCancel = msg.logicalId != null &&
                                        (msg.deliveryState == 'Queued' ||
                                            msg.deliveryState == 'InFlight');

                                    return Column(
                                      crossAxisAlignment: CrossAxisAlignment.start,
                                      children: [
                                        Row(
                                          mainAxisSize: MainAxisSize.min,
                                          children: [
                                            if (msg.isPrivacy) ...[
                                              const Icon(Icons.shield,
                                                  size: 11, color: Colors.tealAccent),
                                              const SizedBox(width: 4),
                                            ],
                                            _buildDeliveryStatusIcon(msg.deliveryState),
                                            const SizedBox(width: 4),
                                            Flexible(
                                              child: Text(
                                                '${msg.deliveryState}: $explanation',
                                                softWrap: true,
                                                style: TextStyle(
                                                  fontSize: 10,
                                                  color: _deliveryStatusColor(msg.deliveryState),
                                                ),
                                              ),
                                            ),
                                          ],
                                        ),
                                        const SizedBox(height: 2),
                                        Wrap(
                                          spacing: 6,
                                          runSpacing: 2,
                                          crossAxisAlignment: WrapCrossAlignment.center,
                                          children: [
                                            Text(
                                              'Try ${msg.attemptCount} · $transportLabel${remLabel.isNotEmpty ? " · $remLabel" : ""}',
                                              style: const TextStyle(
                                                fontSize: 9,
                                                color: Colors.white60,
                                                fontFamily: 'monospace',
                                              ),
                                            ),
                                            if (canCancel)
                                              GestureDetector(
                                                onTap: () {
                                                  _bridge.cancelMessage(msg.logicalId!);
                                                },
                                                child: const Text(
                                                  'Cancel',
                                                  style: TextStyle(
                                                    fontSize: 10,
                                                    color: Colors.orangeAccent,
                                                    fontWeight: FontWeight.bold,
                                                  ),
                                                ),
                                              ),
                                          ],
                                        ),
                                      ],
                                    );
                                  },
                                ),
                              ],
                            ],
                          ),
                        ),
                      );
                    },
                  ),
          ),

          // Message input bar
          Container(
            padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 6),
            color: const Color(0xFF1E1E1E),
            child: SafeArea(
              child: Row(
                children: [
                  ActionChip(
                    visualDensity: VisualDensity.compact,
                    materialTapTargetSize: MaterialTapTargetSize.shrinkWrap,
                    padding: const EdgeInsets.symmetric(horizontal: 2),
                    avatar: Icon(
                      _selectedRecipient == null ? Icons.campaign : Icons.lock,
                      size: 15,
                      color: _selectedRecipient == null ? Colors.amber : Colors.purpleAccent,
                    ),
                    label: Text(
                      _selectedRecipient == null ? 'Broadcast' : 'Direct',
                      style: const TextStyle(fontSize: 12),
                    ),
                    onPressed: () {
                      if (_peers.isNotEmpty && _selectedRecipient == null) {
                        setState(() {
                          _selectedRecipient = _peers.first;
                        });
                      } else {
                        setState(() {
                          _selectedRecipient = null;
                        });
                      }
                    },
                  ),
                  if (_selectedRecipient != null) ...[
                    const SizedBox(width: 4),
                    IconButton(
                      padding: EdgeInsets.zero,
                      constraints: const BoxConstraints(minWidth: 32, minHeight: 32),
                      icon: Icon(
                        _isPrivacyMode ? Icons.security : Icons.security_outlined,
                        color: _isPrivacyMode ? Colors.tealAccent : Colors.grey,
                        size: 20,
                      ),
                      tooltip: _isPrivacyMode ? 'Privacy Mode (Direct transport only)' : 'Standard Mode',
                      onPressed: () {
                        setState(() {
                          _isPrivacyMode = !_isPrivacyMode;
                        });
                      },
                    ),
                    PopupMenuButton<int?>(
                      padding: EdgeInsets.zero,
                      constraints: const BoxConstraints(minWidth: 32, minHeight: 32),
                      tooltip: 'Delivery Deadline (${DeliveryExplanation.formatDeadlineLabel(_selectedDeadlineSecs)})',
                      icon: Icon(
                        Icons.hourglass_bottom,
                        color: _selectedDeadlineSecs != null ? Colors.lightGreenAccent : Colors.grey,
                        size: 20,
                      ),
                      onSelected: (secs) {
                        if (!DeliveryExplanation.isDeadlineCompatibleWithMode(
                          secs,
                          isPrivacy: _isPrivacyMode,
                        )) {
                          ScaffoldMessenger.of(context).showSnackBar(
                            const SnackBar(
                              content: Text('Selected deadline exceeds current mode maximum TTL'),
                            ),
                          );
                          return;
                        }
                        setState(() {
                          _selectedDeadlineSecs = secs;
                        });
                      },
                      itemBuilder: (ctx) => [
                        const PopupMenuItem(
                          value: null,
                          child: Text('Delivery Deadline: Mode Default (24h / 15m)'),
                        ),
                        const PopupMenuItem(
                          value: DeliveryExplanation.preset2mSecs,
                          child: Text('Delivery Deadline: 2 minutes (120s)'),
                        ),
                        const PopupMenuItem(
                          value: DeliveryExplanation.preset15mSecs,
                          child: Text('Delivery Deadline: 15 minutes (900s)'),
                        ),
                        const PopupMenuItem(
                          value: DeliveryExplanation.preset1hSecs,
                          child: Text('Delivery Deadline: 1 hour (3600s)'),
                        ),
                      ],
                    ),
                    PopupMenuButton<int>(
                      padding: EdgeInsets.zero,
                      constraints: const BoxConstraints(minWidth: 32, minHeight: 32),
                      tooltip: 'Volatile Chat Retention',
                      icon: Builder(
                        builder: (ctx) {
                          final vs = _bridge.getVolatileState(_selectedRecipient!);
                          final active = vs.state != 'NotRequested' && vs.state != 'UnsupportedByPeer';
                          return Icon(
                            active ? Icons.timer : Icons.timer_outlined,
                            color: active ? Colors.orangeAccent : Colors.grey,
                            size: 20,
                          );
                        },
                      ),
                      onSelected: (ttlSecs) {
                        final state = _bridge.setVolatileSession(_selectedRecipient!, ttlSecs);
                        _bridge.setSecureScreen(ttlSecs > 0);
                        setState(() {});
                        ScaffoldMessenger.of(context).showSnackBar(
                          SnackBar(
                            content: Text(
                              ttlSecs == 0
                                  ? 'Volatile chat disabled (Persistent E2EE)'
                                  : 'Volatile chat requested (${ttlSecs}s): ${state ?? "Unavailable (handshake required)"}',
                            ),
                          ),
                        );
                      },
                      itemBuilder: (ctx) => const [
                        PopupMenuItem(value: 0, child: Text('Persistent Chat (Default)')),
                        PopupMenuItem(value: 300, child: Text('Volatile Chat: 5 min (300s)')),
                        PopupMenuItem(value: 900, child: Text('Volatile Chat: 15 min (900s)')),
                      ],
                    ),
                  ],
                  const SizedBox(width: 6),
                  Expanded(
                    child: Builder(
                      builder: (ctx) {
                        final isVolatileInput = _selectedRecipient != null &&
                            _bridge.getVolatileState(_selectedRecipient!).state !=
                                'NotRequested';
                        return TextField(
                          controller: _inputController,
                          enableSuggestions: !isVolatileInput,
                          autocorrect: !isVolatileInput,
                          decoration: InputDecoration(
                            isDense: true,
                            hintText: _selectedRecipient == null
                                ? 'Broadcast...'
                                : 'Private message...',
                            border: OutlineInputBorder(
                              borderRadius: BorderRadius.circular(24),
                              borderSide: BorderSide.none,
                            ),
                            filled: true,
                            fillColor: const Color(0xFF2C2C2C),
                            contentPadding: const EdgeInsets.symmetric(
                              horizontal: 12,
                              vertical: 8,
                            ),
                          ),
                          onSubmitted: (_) => _sendMessage(),
                        );
                      },
                    ),
                  ),
                  const SizedBox(width: 4),
                  IconButton(
                    padding: EdgeInsets.zero,
                    constraints: const BoxConstraints(minWidth: 36, minHeight: 36),
                    icon: const Icon(Icons.send, color: Color(0xFF4CAF50)),
                    onPressed: _sendMessage,
                  ),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }

  Widget _buildDeliveryStatusIcon(String state) {
    switch (state) {
      case 'Queued':
        return const Icon(Icons.schedule, size: 12, color: Colors.amberAccent);
      case 'InFlight':
        return const Icon(Icons.check, size: 12, color: Colors.white70);
      case 'Delivered':
        return const Icon(Icons.done_all, size: 13, color: Colors.greenAccent);
      case 'Cancelled':
        return const Icon(Icons.cancel_outlined, size: 12, color: Colors.grey);
      case 'Failed':
      case 'Expired':
        return const Icon(Icons.error_outline, size: 12, color: Colors.redAccent);
      default:
        return const Icon(Icons.done, size: 12, color: Colors.white54);
    }
  }

  Color _deliveryStatusColor(String state) {
    switch (state) {
      case 'Queued':
        return Colors.amberAccent;
      case 'InFlight':
        return Colors.white70;
      case 'Delivered':
        return Colors.greenAccent;
      case 'Cancelled':
        return Colors.grey;
      case 'Failed':
      case 'Expired':
        return Colors.redAccent;
      default:
        return Colors.white54;
    }
  }

  void _showNodeDetails() {
    showModalBottomSheet(
      context: context,
      isScrollControlled: true,
      backgroundColor: const Color(0xFF1E1E1E),
      builder: (context) {
        return StatefulBuilder(
          builder: (ctx, setModalState) {
            final diag = _bridge.getDiagnostics();
            final bi = _bridge.getBuildInfo();
            final coverStatus = _bridge.supportsExperimentalFeatures
                ? _bridge.getCoverStatus()
                : null;
            final syncStatus = _bridge.supportsExperimentalFeatures
                ? _bridge.getSyncStatus()
                : null;
            return Padding(
              padding: const EdgeInsets.all(20),
              child: SingleChildScrollView(
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Row(
                      children: [
                        const Expanded(
                          child: Text(
                            'Node & Outbox Diagnostics',
                            style: TextStyle(fontSize: 16, fontWeight: FontWeight.bold),
                            overflow: TextOverflow.ellipsis,
                          ),
                        ),
                        const SizedBox(width: 8),
                        Container(
                          padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
                          decoration: BoxDecoration(
                            color: const Color(0xFF2E7D32),
                            borderRadius: BorderRadius.circular(4),
                          ),
                          child: Text(
                            diag?.buildVersion ?? 'v2.0.6-phase6',
                            style: const TextStyle(
                              fontSize: 10,
                              fontWeight: FontWeight.bold,
                              color: Colors.white,
                              fontFamily: 'monospace',
                            ),
                          ),
                        ),
                      ],
                    ),
                    const SizedBox(height: 6),
                    Text(
                      'ABI v${bi?.abiVersion ?? diag?.abiVersion ?? 0} | Commit: ${bi?.gitCommit ?? diag?.gitCommit ?? "unknown"} | ${(bi?.buildDirty ?? diag?.buildDirty ?? false) ? "dirty" : "clean"} | Key: ${diag?.keySecurityLevel ?? "Unknown"}',
                      style: const TextStyle(fontSize: 10, fontFamily: 'monospace', color: Colors.white70),
                    ),
                    const SizedBox(height: 12),
                    Text('ShortId: ${_bridge.myShortId}', style: const TextStyle(fontFamily: 'monospace')),
                    const SizedBox(height: 6),
                    const Text('Full Ed25519 PeerId:', style: TextStyle(fontSize: 12, color: Colors.grey)),
                    Text(_bridge.myPeerId, style: const TextStyle(fontSize: 10, fontFamily: 'monospace')),
                    const Divider(height: 24),
                    const Text('Outbox & Relay Engine Status', style: TextStyle(fontSize: 13, fontWeight: FontWeight.bold, color: Colors.greenAccent)),
                    const SizedBox(height: 8),
                    Row(
                      mainAxisAlignment: MainAxisAlignment.spaceAround,
                      children: [
                        _statItem('Queued', diag?.outboxQueuedCount ?? 0),
                        _statItem('In Flight', diag?.outboxInflightCount ?? 0),
                        _statItem('Delivered', diag?.deliveriesCount ?? _bridge.deliveries),
                        _statItem('Tx Total', _bridge.transmissions),
                      ],
                    ),
                    const SizedBox(height: 12),
                    Row(
                      mainAxisAlignment: MainAxisAlignment.spaceAround,
                      children: [
                        _statItem('Suppressed', _bridge.relaysSuppressed),
                        _statItem('Duplicates', _bridge.duplicatesDropped),
                        _statItem('Peers', diag?.connectedPeersCount ?? _peers.length),
                      ],
                    ),
                    const Divider(height: 24),
                    const Text('Resource-Budgeted Relaying Policy', style: TextStyle(fontSize: 13, fontWeight: FontWeight.bold, color: Colors.amberAccent)),
                    const SizedBox(height: 6),
                    Text(
                      'Active Profile: ${diag?.resourceProfile ?? "Balanced"} | Battery: ${diag?.batteryBand ?? "Unknown"} (${diag?.chargingState ?? "Unknown"}) | OS Power Save: ${diag?.powerSaveMode ?? "False"}',
                      style: const TextStyle(fontSize: 12),
                    ),
                    const SizedBox(height: 6),
                    Text(
                      'Transit Forwarded: ${diag?.transitPacketsForwarded ?? 0} pkts (${diag?.transitBytesForwarded ?? 0} B) | Dropped by policy: ${diag?.transitPacketsDroppedPolicy ?? 0}',
                      style: const TextStyle(fontSize: 11, color: Colors.white70, fontFamily: 'monospace'),
                    ),
                    const SizedBox(height: 8),
                    Wrap(
                      spacing: 6,
                      runSpacing: 6,
                      children: [
                        for (final entry in const [
                          ('receive_only', 'Receive only'),
                          ('minimal', 'Minimal'),
                          ('balanced', 'Balanced'),
                          ('help_network', 'Help the network'),
                        ])
                          ChoiceChip(
                            label: Text(entry.$2, style: const TextStyle(fontSize: 11)),
                            selected: (diag?.relayContributionMode ?? _bridge.relayContributionMode) == entry.$1,
                            onSelected: (_) {
                              _bridge.setRelayContributionMode(entry.$1);
                              setModalState(() {});
                              setState(() {});
                            },
                          ),
                      ],
                    ),
                    const SizedBox(height: 4),
                    const Text(
                      'Pausing stops new optional transit only (cannot recall already sent bytes and may partition BLE-only chains). Charging or Wi-Fi never auto-grants relay permission over your setting or thermal limits.',
                      style: TextStyle(fontSize: 10, color: Colors.grey),
                    ),
                    const Divider(height: 24),
                    const Text('Transport, Trust & Privacy Layer', style: TextStyle(fontSize: 13, fontWeight: FontWeight.bold, color: Colors.tealAccent)),
                    const SizedBox(height: 6),
                    Text('Gateway Active: ${diag?.gatewayActive ?? false ? "YES" : "NO"} | Observed Path: ${diag?.gatewayObservedPath ?? "Unknown"}', style: const TextStyle(fontSize: 12)),
                    Text(
                      'Gateway Direct/Relay Mode: ${diag == null ? "Unknown" : (diag.gatewayRelayDisabled ? "Relay Disabled (Direct-only)" : "Relay Enabled (Default)")}',
                      style: const TextStyle(fontSize: 12),
                    ),
                    Text(
                      'Gateway Consent: ${diag?.approvedGatewayCount ?? 0} approved, ${diag?.pendingGatewayCount ?? 0} pending | Active 1-Hop Vouchers: ${diag?.activeVouchersCount ?? 0}',
                      style: const TextStyle(fontSize: 12),
                    ),
                    if (diag?.lastRouteCandidate != null) ...[
                      const SizedBox(height: 4),
                      Text('Last Route Candidate: ${diag!.lastRouteCandidate}', style: const TextStyle(fontSize: 12, fontFamily: 'monospace')),
                    ],
                    if (_bridge.supportsExperimentalFeatures) ...[
                      const Divider(height: 24),
                      const Text(
                        'Experimental Controls (Default OFF)',
                        style: TextStyle(fontSize: 13, fontWeight: FontWeight.bold, color: Colors.deepPurpleAccent),
                      ),
                      const SizedBox(height: 4),
                      SwitchListTile(
                        dense: true,
                        contentPadding: EdgeInsets.zero,
                        title: const Text(
                          'Experimental Traffic Padding (Default OFF)',
                          style: TextStyle(fontSize: 12, fontWeight: FontWeight.w600),
                        ),
                        subtitle: Text(
                          'Dummy frames sent: ${coverStatus?.dummyFramesSent ?? 0} (${coverStatus?.maxBytesPerMinute ?? 4096} B/min cap). Experimental dummy padding on direct approved links only; does not hide online presence or guarantee anonymity.',
                          style: const TextStyle(fontSize: 10, color: Colors.grey),
                        ),
                        value: coverStatus?.enabled ?? _bridge.coverTrafficEnabled,
                        onChanged: (enabled) {
                          _bridge.setCoverTraffic(enabled);
                          setModalState(() {});
                          setState(() {});
                        },
                      ),
                      SwitchListTile(
                        dense: true,
                        contentPadding: EdgeInsets.zero,
                        title: const Text(
                          'Experimental Normal-Mode Sync (Default OFF)',
                          style: TextStyle(fontSize: 12, fontWeight: FontWeight.w600),
                        ),
                        subtitle: Text(
                          'Cap: ${syncStatus?.maxRecordsPerConversation ?? 1024} records / ${(syncStatus?.maxSessionBytes ?? 262144) ~/ 1024} KiB. Normal-mode retained records only; Privacy/volatile excluded. Pending human crypto review.',
                          style: const TextStyle(fontSize: 10, color: Colors.grey),
                        ),
                        value: syncStatus?.enabled ?? _bridge.syncEnabled,
                        onChanged: (enabled) {
                          _bridge.setSyncEnabled(enabled);
                          setModalState(() {});
                          setState(() {});
                        },
                      ),
                      if ((syncStatus?.enabled ?? _bridge.syncEnabled) && _selectedRecipient != null) ...[
                        const SizedBox(height: 4),
                        Wrap(
                          spacing: 8,
                          children: [
                            OutlinedButton(
                              onPressed: () {
                                try {
                                  final fullPid = _bridge.getPeerFullId(_selectedRecipient!) ?? _selectedRecipient!;
                                  _bridge.initiateSync(fullPid, useMerkle: false);
                                  ScaffoldMessenger.of(context).showSnackBar(
                                    const SnackBar(content: Text('Sync initiated (ID Inventory baseline).')),
                                  );
                                } catch (e) {
                                  ScaffoldMessenger.of(context).showSnackBar(
                                    SnackBar(
                                      content: Text('Sync unavailable: ${e.toString().replaceAll("Exception: ", "")}'),
                                      backgroundColor: Colors.red.shade900,
                                    ),
                                  );
                                }
                                setModalState(() {});
                              },
                              child: const Text('Sync Peer (ID-Inventory)', style: TextStyle(fontSize: 11)),
                            ),
                            OutlinedButton(
                              onPressed: () {
                                try {
                                  final fullPid = _bridge.getPeerFullId(_selectedRecipient!) ?? _selectedRecipient!;
                                  _bridge.initiateSync(fullPid, useMerkle: true);
                                  ScaffoldMessenger.of(context).showSnackBar(
                                    const SnackBar(content: Text('Sync initiated (Experimental Merkle).')),
                                  );
                                } catch (e) {
                                  ScaffoldMessenger.of(context).showSnackBar(
                                    SnackBar(
                                      content: Text('Sync unavailable: ${e.toString().replaceAll("Exception: ", "")}'),
                                      backgroundColor: Colors.red.shade900,
                                    ),
                                  );
                                }
                                setModalState(() {});
                              },
                              child: const Text('Sync Peer (Experimental Merkle)', style: TextStyle(fontSize: 11)),
                            ),
                          ],
                        ),
                      ],
                    ],
                  ],
                ),
              ),
            );
          },
        );
      },
    );
  }

  Widget _statItem(String label, int val) {
    return Expanded(
      child: Column(
        children: [
          Text(val.toString(), style: const TextStyle(fontSize: 18, fontWeight: FontWeight.bold, color: Color(0xFF4CAF50))),
          Text(label, style: const TextStyle(fontSize: 11, color: Colors.grey), textAlign: TextAlign.center, maxLines: 1, overflow: TextOverflow.ellipsis),
        ],
      ),
    );
  }

  void _showGatewayDialog() {
    _gatewayDialogTimer?.cancel();
    Timer? dialogTimer;
    String? localContactUri;

    showModalBottomSheet(
      context: context,
      isScrollControlled: true,
      backgroundColor: const Color(0xFF1E1E1E),
      builder: (ctx) {
        return StatefulBuilder(
          builder: (context, setModalState) {
            dialogTimer ??= Timer.periodic(const Duration(milliseconds: 600), (_) {
              if (context.mounted) {
                setModalState(() {});
              } else {
                dialogTimer?.cancel();
              }
            });
            _gatewayDialogTimer = dialogTimer;
            final stats = _bridge.getGatewayStats();
            final isRunning = stats?.isRunning ?? false;
            if (isRunning && localContactUri == null) {
              localContactUri = _bridge.exportGatewayContact();
            } else if (!isRunning) {
              localContactUri = null;
            }

            return Padding(
              padding: EdgeInsets.only(
                bottom: MediaQuery.of(context).viewInsets.bottom + 16,
                top: 16,
                left: 16,
                right: 16,
              ),
              child: SingleChildScrollView(
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Row(
                      mainAxisAlignment: MainAxisAlignment.spaceBetween,
                      children: [
                        const Text(
                          'Iroh Internet Gateway',
                          style: TextStyle(fontSize: 18, fontWeight: FontWeight.bold),
                        ),
                        Container(
                          padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
                          decoration: BoxDecoration(
                            color: isRunning ? Colors.green.shade800 : Colors.grey.shade800,
                            borderRadius: BorderRadius.circular(4),
                          ),
                          child: Text(
                            isRunning ? 'ONLINE' : 'STOPPED',
                            style: const TextStyle(fontSize: 11, fontWeight: FontWeight.bold),
                          ),
                        ),
                      ],
                    ),
                    const SizedBox(height: 12),

                    // Remote Gateway QR Scanner (ALWAYS available whether online or stopped)
                    SizedBox(
                      width: double.infinity,
                      child: ElevatedButton.icon(
                        icon: const Icon(Icons.qr_code_scanner, size: 18),
                        label: const Text('Scan Remote Gateway QR', style: TextStyle(fontSize: 13, fontWeight: FontWeight.bold)),
                        style: ElevatedButton.styleFrom(
                          backgroundColor: const Color(0xFF00695C),
                          padding: const EdgeInsets.symmetric(vertical: 10),
                        ),
                        onPressed: () {
                          Navigator.pop(ctx);
                          _openQrScanner();
                        },
                      ),
                    ),
                    const SizedBox(height: 8),

                    if (isRunning) ...[
                      if (stats?.endpointId != null) ...[
                        const Text('Endpoint ID:', style: TextStyle(fontSize: 11, color: Colors.grey)),
                        Row(
                          children: [
                            Expanded(
                              child: Text(
                                stats!.endpointId!,
                                style: const TextStyle(fontSize: 11, fontFamily: 'monospace', color: Color(0xFF81C784)),
                                overflow: TextOverflow.ellipsis,
                              ),
                            ),
                            IconButton(
                              icon: const Icon(Icons.copy, size: 16),
                              onPressed: () {
                                Clipboard.setData(ClipboardData(text: stats.endpointId!));
                                ScaffoldMessenger.of(context).showSnackBar(
                                  const SnackBar(content: Text('Endpoint ID copied')),
                                );
                              },
                            ),
                          ],
                        ),
                        const SizedBox(height: 6),
                      ],
                      Row(
                        children: [
                          Expanded(
                            child: OutlinedButton.icon(
                              icon: const Icon(Icons.qr_code, size: 16),
                              label: const Text('Show My Gateway QR', style: TextStyle(fontSize: 12)),
                              onPressed: () {
                                final uri = localContactUri ?? _bridge.exportGatewayContact();
                                if (uri != null && uri.isNotEmpty) {
                                  _showQrDialog(
                                    uri,
                                    title: 'Gateway Pairing QR',
                                    subtitle: 'Scan with another phone to connect via Iroh QUIC tunnel (includes single-use 5-min pairing token).',
                                  );
                                } else {
                                  ScaffoldMessenger.of(context).showSnackBar(
                                    const SnackBar(content: Text('Starting gateway... please try again in a moment')),
                                  );
                                }
                              },
                            ),
                          ),
                          const SizedBox(width: 8),
                          Expanded(
                            child: OutlinedButton.icon(
                              icon: const Icon(Icons.share, size: 16),
                              label: const Text('Share Link', style: TextStyle(fontSize: 12)),
                              onPressed: () {
                                final uri = localContactUri ?? _bridge.exportGatewayContact();
                                if (uri != null && uri.isNotEmpty) {
                                  _bridge.shareText(uri, title: 'Share Gateway Contact');
                                }
                              },
                            ),
                          ),
                        ],
                      ),
                      const SizedBox(height: 12),
                      Row(
                        mainAxisAlignment: MainAxisAlignment.spaceAround,
                        children: [
                          _statItem('QUIC Conns', stats?.activeConnections ?? 0),
                          _statItem('Imported', stats?.forwardsImported ?? 0),
                          _statItem('Exported', stats?.forwardsExported ?? 0),
                          _statItem('Loops Prevented', stats?.loopsPrevented ?? 0),
                        ],
                      ),
                      Builder(
                        builder: (ctx) {
                          final pending = _bridge.getPendingGatewayPeers();
                          if (pending.isEmpty) return const SizedBox.shrink();
                          return Column(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            children: [
                              const SizedBox(height: 12),
                              Text(
                                'Pending Gateway Consent (${pending.length}):',
                                style: const TextStyle(fontSize: 11, color: Colors.amberAccent, fontWeight: FontWeight.bold),
                              ),
                              const SizedBox(height: 4),
                              const Text(
                                'Uninvited peers verified identity-key ownership but cannot use your gateway until approved.',
                                style: TextStyle(fontSize: 10, color: Colors.grey),
                              ),
                              const SizedBox(height: 6),
                              ...pending.map((p) {
                                return Card(
                                  color: const Color(0xFF282828),
                                  margin: const EdgeInsets.only(bottom: 6),
                                  child: Padding(
                                    padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 6),
                                    child: Row(
                                      children: [
                                        Expanded(
                                          child: Column(
                                            crossAxisAlignment: CrossAxisAlignment.start,
                                            children: [
                                              Text(
                                                'ShortId: ${p.shortId}',
                                                style: const TextStyle(fontSize: 11, fontFamily: 'monospace', fontWeight: FontWeight.bold),
                                              ),
                                              Text(
                                                'PeerId: ${p.peerId.length > 16 ? "${p.peerId.substring(0, 16)}..." : p.peerId}',
                                                style: const TextStyle(fontSize: 9, fontFamily: 'monospace', color: Colors.grey),
                                              ),
                                            ],
                                          ),
                                        ),
                                        TextButton(
                                          onPressed: () {
                                            final ok = _bridge.approveGatewayPeer(p.endpointId);
                                            setModalState(() {});
                                            setState(() {});
                                            ScaffoldMessenger.of(context).showSnackBar(
                                              SnackBar(content: Text(ok ? 'Approved gateway peer ${p.shortId}' : 'Approval failed')),
                                            );
                                          },
                                          child: const Text('Approve', style: TextStyle(fontSize: 11, color: Colors.greenAccent)),
                                        ),
                                        TextButton(
                                          onPressed: () {
                                            final ok = _bridge.revokeOrBlockGatewayPeer(p.endpointId, block: true);
                                            setModalState(() {});
                                            setState(() {});
                                            ScaffoldMessenger.of(context).showSnackBar(
                                              SnackBar(content: Text(ok ? 'Blocked gateway peer ${p.shortId}' : 'Block failed')),
                                            );
                                          },
                                          child: const Text('Block', style: TextStyle(fontSize: 11, color: Colors.redAccent)),
                                        ),
                                      ],
                                    ),
                                  ),
                                );
                              }),
                            ],
                          );
                        },
                      ),
                      Builder(
                        builder: (ctx) {
                          final gwRoutes = _bridge.getGatewayRoutes();
                          if (gwRoutes.isEmpty) return const SizedBox.shrink();
                          return Column(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            children: [
                              const SizedBox(height: 12),
                              const Text(
                                'Reachable Peers via Gateway (Auto-synced):',
                                style: TextStyle(fontSize: 11, color: Colors.grey, fontWeight: FontWeight.bold),
                              ),
                              const SizedBox(height: 6),
                              Wrap(
                                spacing: 6,
                                runSpacing: 4,
                                children: gwRoutes.map((r) {
                                  final isSel = _selectedRecipient == r;
                                  return ActionChip(
                                    avatar: const Icon(Icons.public, size: 14, color: Colors.tealAccent),
                                    label: Text(
                                      r.length > 8 ? r.substring(0, 8) : r,
                                      style: const TextStyle(fontSize: 11, fontFamily: 'monospace'),
                                    ),
                                    backgroundColor: isSel ? Colors.teal.shade900 : null,
                                    onPressed: () {
                                      setState(() {
                                        _selectedRecipient = r;
                                      });
                                      Navigator.pop(ctx);
                                    },
                                  );
                                }).toList(),
                              ),
                            ],
                          );
                        },
                      ),
                      const SizedBox(height: 12),
                      Row(
                        children: [
                          Expanded(
                            child: ElevatedButton.icon(
                              icon: const Icon(Icons.connect_without_contact, size: 16),
                              label: const Text('Connect to Contact', style: TextStyle(fontSize: 12)),
                              style: ElevatedButton.styleFrom(backgroundColor: const Color(0xFF2C2C2C)),
                              onPressed: () => _promptConnectContact(setModalState),
                            ),
                          ),
                          const SizedBox(width: 8),
                          Expanded(
                            child: ElevatedButton.icon(
                              icon: const Icon(Icons.alt_route, size: 16),
                              label: const Text('Manual Route', style: TextStyle(fontSize: 12)),
                              style: ElevatedButton.styleFrom(backgroundColor: const Color(0xFF2C2C2C)),
                              onPressed: () => _promptSetRoute(setModalState),
                            ),
                          ),
                        ],
                      ),
                    ] else ...[
                      Container(
                        width: double.infinity,
                        margin: const EdgeInsets.symmetric(vertical: 8),
                        padding: const EdgeInsets.all(10),
                        decoration: BoxDecoration(
                          color: const Color(0xFF242424),
                          borderRadius: BorderRadius.circular(8),
                          border: Border.all(color: Colors.white12),
                        ),
                        child: const Text(
                          'Gateway Bridge is offline on this phone. Start the bridge to act as an Internet gateway for mesh nodes, or tap "Scan Remote Gateway QR" above to connect to another node\'s gateway as a client.',
                          style: TextStyle(fontSize: 11, color: Colors.white70),
                        ),
                      ),
                      SizedBox(
                        width: double.infinity,
                        child: OutlinedButton.icon(
                          icon: const Icon(Icons.connect_without_contact, size: 16),
                          label: const Text('Enter Gateway Contact URI Manually', style: TextStyle(fontSize: 12)),
                          onPressed: () => _promptConnectContact(setModalState),
                        ),
                      ),
                    ],
                    const SizedBox(height: 12),
                    SizedBox(
                      width: double.infinity,
                      child: ElevatedButton(
                        style: ElevatedButton.styleFrom(
                          backgroundColor: isRunning ? Colors.red.shade900 : Colors.green.shade800,
                          padding: const EdgeInsets.symmetric(vertical: 12),
                        ),
                        onPressed: () async {
                          if (isRunning) {
                            await _bridge.stopGateway();
                            localContactUri = null;
                          } else {
                            await _bridge.startGateway();
                            localContactUri = _bridge.exportGatewayContact();
                          }
                          setModalState(() {});
                          setState(() {});
                        },
                        child: Text(
                          isRunning ? 'Stop Gateway Bridge' : 'Start Gateway Bridge',
                          style: const TextStyle(fontWeight: FontWeight.bold),
                        ),
                      ),
                    ),
                  ],
                ),
              ),
            );
          },
        );
      },
    ).whenComplete(() => dialogTimer?.cancel());
  }

  void _promptConnectContact(void Function(void Function()) setModalState) {
    final controller = TextEditingController();
    showDialog(
      context: context,
      builder: (ctx) => AlertDialog(
        backgroundColor: const Color(0xFF1E1E1E),
        title: const Text('Connect Gateway Contact', style: TextStyle(fontSize: 16)),
        content: TextField(
          controller: controller,
          decoration: const InputDecoration(
            hintText: 'bitchat-gw://...',
            hintStyle: TextStyle(fontSize: 12),
          ),
          style: const TextStyle(fontSize: 12, fontFamily: 'monospace'),
          maxLines: 3,
        ),
        actions: [
          TextButton(onPressed: () => Navigator.pop(ctx), child: const Text('Cancel')),
          ElevatedButton(
            onPressed: () {
              final uri = controller.text.trim();
              if (uri.isNotEmpty) {
                final approved = _bridge.importAndApproveGatewayContact(uri);
                if (approved != null) {
                  final ok = _bridge.connectToGatewayContact(uri);
                  Navigator.pop(ctx);
                  setModalState(() {});
                  ScaffoldMessenger.of(context).showSnackBar(
                    SnackBar(content: Text(ok ? 'Connected to gateway' : 'Dial failed')),
                  );
                } else {
                  ScaffoldMessenger.of(context).showSnackBar(
                    const SnackBar(content: Text('Invalid or untrusted contact URI')),
                  );
                }
              }
            },
            child: const Text('Connect'),
          ),
        ],
      ),
    );
  }

  void _promptSetRoute(void Function(void Function()) setModalState) {
    final recipCtrl = TextEditingController();
    final targetCtrl = TextEditingController();
    showDialog(
      context: context,
      builder: (ctx) => AlertDialog(
        backgroundColor: const Color(0xFF1E1E1E),
        title: const Text('Add Recipient Route', style: TextStyle(fontSize: 16)),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            TextField(
              controller: recipCtrl,
              decoration: const InputDecoration(
                labelText: 'Recipient ShortId (16 hex)',
                labelStyle: TextStyle(fontSize: 12),
              ),
              style: const TextStyle(fontSize: 12, fontFamily: 'monospace'),
            ),
            const SizedBox(height: 8),
            TextField(
              controller: targetCtrl,
              decoration: const InputDecoration(
                labelText: 'Target Gateway EndpointId (hex)',
                labelStyle: TextStyle(fontSize: 12),
              ),
              style: const TextStyle(fontSize: 12, fontFamily: 'monospace'),
            ),
          ],
        ),
        actions: [
          TextButton(onPressed: () => Navigator.pop(ctx), child: const Text('Cancel')),
          ElevatedButton(
            onPressed: () {
              final recip = recipCtrl.text.trim();
              final target = targetCtrl.text.trim();
              if (recip.isNotEmpty && target.isNotEmpty) {
                final ok = _bridge.setGatewayRoute(recip, target);
                Navigator.pop(ctx);
                setModalState(() {});
                ScaffoldMessenger.of(context).showSnackBar(
                  SnackBar(content: Text(ok ? 'Route configured' : 'Failed to set route')),
                );
              }
            },
            child: const Text('Save Route'),
          ),
        ],
      ),
    );
  }

  void _showQrDialog(String data, {String title = 'QR Code', String? subtitle}) {
    showDialog(
      context: context,
      builder: (ctx) => AlertDialog(
        backgroundColor: const Color(0xFF1E1E1E),
        title: Text(title, style: const TextStyle(fontSize: 16)),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            SizedBox(
              width: 240,
              height: 240,
              child: Center(
                child: Container(
                  padding: const EdgeInsets.all(8),
                  color: Colors.white,
                  child: QrImageView(
                    data: data,
                    version: QrVersions.auto,
                    size: 220.0,
                  ),
                ),
              ),
            ),
            if (subtitle != null) ...[
              const SizedBox(height: 10),
              Text(
                subtitle,
                style: const TextStyle(fontSize: 11, color: Colors.grey),
                textAlign: TextAlign.center,
              ),
            ],
          ],
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx),
            child: const Text('Close'),
          ),
        ],
      ),
    );
  }

  void _showMyIdQrDialog() {
    final id = _bridge.myShortId;
    if (id.isEmpty) return;
    showDialog(
      context: context,
      builder: (ctx) => AlertDialog(
        backgroundColor: const Color(0xFF1E1E1E),
        title: const Text('My Node ID QR', style: TextStyle(fontSize: 16)),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Container(
              padding: const EdgeInsets.all(12),
              color: Colors.white,
              child: QrImageView(
                data: id,
                version: QrVersions.auto,
                size: 200.0,
              ),
            ),
            const SizedBox(height: 12),
            Text(
              id,
              style: const TextStyle(
                fontFamily: 'monospace',
                fontSize: 14,
                fontWeight: FontWeight.bold,
                color: Color(0xFF81C784),
              ),
            ),
            const SizedBox(height: 4),
            const Text(
              'Other phones can scan this QR to message you directly.',
              style: TextStyle(fontSize: 11, color: Colors.grey),
              textAlign: TextAlign.center,
            ),
          ],
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx),
            child: const Text('Close'),
          ),
        ],
      ),
    );
  }

  void _showTrustAndVouchersDialog() {
    final peerTrustCtrl = TextEditingController();
    final subjectCtrl = TextEditingController();
    final audienceCtrl = TextEditingController(text: _bridge.myPeerId);
    final importVoucherCtrl = TextEditingController();
    String selectedCap = 'direct_message';
    String? issuedVoucherUri;
    String? statusMessage;
    bool statusSuccess = true;

    showModalBottomSheet(
      context: context,
      isScrollControlled: true,
      backgroundColor: const Color(0xFF1E1E1E),
      builder: (ctx) {
        return StatefulBuilder(
          builder: (context, setModalState) {
            return Padding(
              padding: EdgeInsets.only(
                bottom: MediaQuery.of(context).viewInsets.bottom + 16,
                top: 16,
                left: 16,
                right: 16,
              ),
              child: SingleChildScrollView(
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    const Text(
                      'Scoped Peer Trust & 1-Hop Vouchers',
                      style: TextStyle(fontSize: 16, fontWeight: FontWeight.bold),
                    ),
                    const SizedBox(height: 6),
                    const Text(
                      'Scope: 1-hop introduction only (subject -> audience, bounded TTL). Never transitive, never auto-approves relay gateways, and does NOT prove unique human identity.',
                      style: TextStyle(fontSize: 11, color: Colors.amberAccent),
                    ),
                    Builder(
                      builder: (ctx) {
                        final pendingReqs = _bridge.getPendingMessageRequests();
                        final authEvents = _bridge.pollAuthEvents();
                        return Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            if (pendingReqs.isNotEmpty) ...[
                              const Divider(height: 16),
                              Text(
                                'Pending Message Requests (${pendingReqs.length}):',
                                style: const TextStyle(
                                  fontSize: 13,
                                  fontWeight: FontWeight.bold,
                                  color: Colors.amberAccent,
                                ),
                              ),
                              const SizedBox(height: 6),
                              ...pendingReqs.map((req) {
                                final label = req.senderPeerId.length >= 16
                                    ? '${req.senderShortId} (${req.senderPeerId.substring(0, 12)}...)'
                                    : req.senderShortId;
                                return Container(
                                  margin: const EdgeInsets.only(bottom: 6),
                                  padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 6),
                                  decoration: BoxDecoration(
                                    color: const Color(0xFF262626),
                                    borderRadius: BorderRadius.circular(6),
                                    border: Border.all(color: Colors.amber.shade700, width: 0.7),
                                  ),
                                  child: Row(
                                    children: [
                                      Expanded(
                                        child: Column(
                                          crossAxisAlignment: CrossAxisAlignment.start,
                                          children: [
                                            Text(
                                              label,
                                              style: const TextStyle(fontSize: 11, fontFamily: 'monospace'),
                                              overflow: TextOverflow.ellipsis,
                                            ),
                                            Text(
                                              'Reason: ${req.reason}${req.isVolatile ? " | Volatile" : ""}',
                                              style: const TextStyle(fontSize: 10, color: Colors.grey),
                                            ),
                                          ],
                                        ),
                                      ),
                                      TextButton(
                                        onPressed: () {
                                          final ok = _bridge.setPeerTrustState(req.senderPeerId, 'approve');
                                          setModalState(() {
                                            statusSuccess = ok;
                                            statusMessage = ok
                                                ? 'Approved request from ${req.senderShortId}.'
                                                : 'Failed to approve ${req.senderShortId}.';
                                          });
                                        },
                                        child: const Text('Approve', style: TextStyle(fontSize: 11, color: Colors.greenAccent)),
                                      ),
                                      TextButton(
                                        onPressed: () {
                                          final ok = _bridge.setPeerTrustState(req.senderPeerId, 'block');
                                          setModalState(() {
                                            statusSuccess = ok;
                                            statusMessage = ok
                                                ? 'Blocked sender ${req.senderShortId}.'
                                                : 'Failed to block ${req.senderShortId}.';
                                          });
                                        },
                                        child: const Text('Block', style: TextStyle(fontSize: 11, color: Colors.redAccent)),
                                      ),
                                    ],
                                  ),
                                );
                              }),
                            ],
                            if (authEvents.isNotEmpty) ...[
                              const SizedBox(height: 6),
                              Text(
                                'Recent Auth Events (${authEvents.length}): ${authEvents.first.shortId} -> ${authEvents.first.outcome} (${authEvents.first.reason})',
                                style: const TextStyle(fontSize: 10, color: Colors.white60, fontFamily: 'monospace'),
                                maxLines: 2,
                                overflow: TextOverflow.ellipsis,
                              ),
                            ],
                          ],
                        );
                      },
                    ),
                    if (statusMessage != null) ...[
                      const SizedBox(height: 10),
                      Container(
                        width: double.infinity,
                        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
                        decoration: BoxDecoration(
                          color: statusSuccess ? const Color(0xFF1B5E20) : const Color(0xFFB71C1C),
                          borderRadius: BorderRadius.circular(6),
                          border: Border.all(
                            color: statusSuccess ? Colors.greenAccent : Colors.redAccent,
                            width: 1.2,
                          ),
                        ),
                        child: Row(
                          children: [
                            Icon(
                              statusSuccess ? Icons.check_circle : Icons.error_outline,
                              color: statusSuccess ? Colors.greenAccent : Colors.redAccent,
                              size: 18,
                            ),
                            const SizedBox(width: 8),
                            Expanded(
                              child: Text(
                                statusMessage!,
                                style: TextStyle(
                                  color: statusSuccess ? Colors.greenAccent : Colors.redAccent,
                                  fontSize: 11,
                                  fontWeight: FontWeight.bold,
                                ),
                              ),
                            ),
                          ],
                        ),
                      ),
                    ],
                    const SizedBox(height: 10),
                    Row(
                      children: [
                        Expanded(
                          child: Text(
                            'My PeerId: ${_bridge.myPeerId}',
                            style: const TextStyle(fontSize: 10, fontFamily: 'monospace', color: Colors.grey),
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                          ),
                        ),
                        IconButton(
                          icon: const Icon(Icons.copy, size: 16),
                          tooltip: 'Copy full PeerId',
                          onPressed: () {
                            Clipboard.setData(ClipboardData(text: _bridge.myPeerId));
                            setModalState(() {
                              statusSuccess = true;
                              statusMessage = 'Full 64-hex PeerId copied to clipboard.';
                            });
                          },
                        ),
                        ElevatedButton.icon(
                          icon: const Icon(Icons.qr_code, size: 16),
                          label: const Text('Show My QR', style: TextStyle(fontSize: 11)),
                          style: ElevatedButton.styleFrom(
                            backgroundColor: const Color(0xFF2E7D32),
                            padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 4),
                          ),
                          onPressed: () => _showQrDialog(
                            'bitchat-id://${_bridge.myPeerId}',
                            title: 'My 64-Hex PeerId QR',
                            subtitle: 'Other phones can scan this directly to approve you or issue a voucher to you.',
                          ),
                        ),
                      ],
                    ),
                    const Divider(height: 16),
                    const Text(
                      '1. Manage Peer Authorization State',
                      style: TextStyle(fontSize: 13, fontWeight: FontWeight.bold, color: Colors.tealAccent),
                    ),
                    const SizedBox(height: 6),
                    TextField(
                      controller: peerTrustCtrl,
                      decoration: InputDecoration(
                        labelText: 'Target PeerId (64 hex chars)',
                        labelStyle: const TextStyle(fontSize: 11),
                        isDense: true,
                        suffixIcon: IconButton(
                          icon: const Icon(Icons.qr_code_scanner, color: Colors.tealAccent, size: 20),
                          tooltip: 'Scan PeerId QR from other phone',
                          onPressed: () async {
                            final scanned = await Navigator.push<String>(
                              context,
                              MaterialPageRoute(builder: (ctx) => const QrScanScreen()),
                            );
                            if (scanned != null && scanned.isNotEmpty) {
                              final hexOnly = scanned.replaceAll('bitchat-id://', '').replaceAll(RegExp(r'[^0-9a-fA-F]'), '');
                              if (hexOnly.length >= 64) {
                                peerTrustCtrl.text = hexOnly.substring(0, 64);
                              } else if (hexOnly.isNotEmpty) {
                                peerTrustCtrl.text = hexOnly;
                              }
                              setModalState(() {});
                            }
                          },
                        ),
                      ),
                      style: const TextStyle(fontSize: 11, fontFamily: 'monospace'),
                    ),
                    if (_peers.isNotEmpty) ...[
                      const SizedBox(height: 4),
                      Wrap(
                        spacing: 6,
                        runSpacing: 4,
                        crossAxisAlignment: WrapCrossAlignment.center,
                        children: [
                          const Text('Connected peers:', style: TextStyle(fontSize: 10, color: Colors.grey)),
                          for (final p in _peers)
                            ActionChip(
                              avatar: const Icon(Icons.bluetooth, size: 12, color: Colors.tealAccent),
                              label: Text(p, style: const TextStyle(fontSize: 10, fontFamily: 'monospace')),
                              padding: EdgeInsets.zero,
                              materialTapTargetSize: MaterialTapTargetSize.shrinkWrap,
                              onPressed: () {
                                final full = _bridge.getPeerFullId(p);
                                if (full != null && full.length >= 64) {
                                  peerTrustCtrl.text = full;
                                  setModalState(() {
                                    statusSuccess = true;
                                    statusMessage = 'Target set to neighbor $p (64-hex).';
                                  });
                                } else {
                                  setModalState(() {
                                    statusSuccess = false;
                                    statusMessage = 'Neighbor $p key not synced yet. Scan "Show My QR" from the other phone.';
                                  });
                                }
                              },
                            ),
                        ],
                      ),
                    ],
                    const SizedBox(height: 8),
                    Wrap(
                      spacing: 6,
                      runSpacing: 6,
                      children: [
                        ElevatedButton(
                          style: ElevatedButton.styleFrom(backgroundColor: Colors.green.shade800),
                          onPressed: () {
                            final pid = peerTrustCtrl.text.trim();
                            final ok = _bridge.setPeerTrustState(pid, 'approve');
                            setModalState(() {
                              statusSuccess = ok;
                              statusMessage = ok ? 'Peer approved.' : 'Invalid 64-hex PeerId.';
                            });
                          },
                          child: const Text('Approve Peer', style: TextStyle(fontSize: 11)),
                        ),
                        ElevatedButton(
                          style: ElevatedButton.styleFrom(backgroundColor: Colors.blue.shade800),
                          onPressed: () {
                            final pid = peerTrustCtrl.text.trim();
                            final ok = _bridge.setPeerTrustState(pid, 'introducer');
                            setModalState(() {
                              statusSuccess = ok;
                              statusMessage = ok ? 'Peer approved as 1-Hop Introducer!' : 'Invalid 64-hex PeerId.';
                            });
                          },
                          child: const Text('Approve + Introducer', style: TextStyle(fontSize: 11)),
                        ),
                        ElevatedButton(
                          style: ElevatedButton.styleFrom(backgroundColor: Colors.red.shade900),
                          onPressed: () {
                            final pid = peerTrustCtrl.text.trim();
                            final ok = _bridge.setPeerTrustState(pid, 'block');
                            setModalState(() {
                              statusSuccess = ok;
                              statusMessage = ok ? 'Peer blocked & vouchers revoked.' : 'Invalid 64-hex PeerId.';
                            });
                          },
                          child: const Text('Block Peer', style: TextStyle(fontSize: 11)),
                        ),
                      ],
                    ),
                    const Divider(height: 20),
                    const Text(
                      '2. Issue 1-Hop Capability Voucher (Ed25519 Signed)',
                      style: TextStyle(fontSize: 13, fontWeight: FontWeight.bold, color: Colors.greenAccent),
                    ),
                    const SizedBox(height: 6),
                    TextField(
                      controller: subjectCtrl,
                      decoration: InputDecoration(
                        labelText: 'Subject PeerId (64 hex)',
                        labelStyle: const TextStyle(fontSize: 11),
                        isDense: true,
                        suffixIcon: IconButton(
                          icon: const Icon(Icons.qr_code_scanner, color: Colors.greenAccent, size: 20),
                          tooltip: 'Scan Subject PeerId QR',
                          onPressed: () async {
                            final scanned = await Navigator.push<String>(
                              context,
                              MaterialPageRoute(builder: (ctx) => const QrScanScreen()),
                            );
                            if (scanned != null && scanned.isNotEmpty) {
                              final hexOnly = scanned.replaceAll('bitchat-id://', '').replaceAll(RegExp(r'[^0-9a-fA-F]'), '');
                              if (hexOnly.length >= 64) {
                                subjectCtrl.text = hexOnly.substring(0, 64);
                              } else if (hexOnly.isNotEmpty) {
                                subjectCtrl.text = hexOnly;
                              }
                              setModalState(() {});
                            }
                          },
                        ),
                      ),
                      style: const TextStyle(fontSize: 11, fontFamily: 'monospace'),
                    ),
                    Row(
                      children: [
                        TextButton.icon(
                          icon: const Icon(Icons.shuffle, size: 14),
                          label: const Text('Generate Random Subject ID', style: TextStyle(fontSize: 10)),
                          style: TextButton.styleFrom(padding: const EdgeInsets.symmetric(horizontal: 4)),
                          onPressed: () {
                            final rnd = Random();
                            final randHex = List.generate(64, (_) => rnd.nextInt(16).toRadixString(16)).join();
                            subjectCtrl.text = randHex;
                            setModalState(() {});
                          },
                        ),
                      ],
                    ),
                    const SizedBox(height: 4),
                    TextField(
                      controller: audienceCtrl,
                      decoration: InputDecoration(
                        labelText: 'Audience PeerId (64 hex of verifier)',
                        labelStyle: const TextStyle(fontSize: 11),
                        isDense: true,
                        suffixIcon: Row(
                          mainAxisSize: MainAxisSize.min,
                          children: [
                            IconButton(
                              icon: const Icon(Icons.qr_code_scanner, color: Colors.greenAccent, size: 20),
                              tooltip: 'Scan Audience PeerId QR',
                              onPressed: () async {
                                final scanned = await Navigator.push<String>(
                                  context,
                                  MaterialPageRoute(builder: (ctx) => const QrScanScreen()),
                                );
                                if (scanned != null && scanned.isNotEmpty) {
                                  final hexOnly = scanned.replaceAll('bitchat-id://', '').replaceAll(RegExp(r'[^0-9a-fA-F]'), '');
                                  if (hexOnly.length >= 64) {
                                    audienceCtrl.text = hexOnly.substring(0, 64);
                                  } else if (hexOnly.isNotEmpty) {
                                    audienceCtrl.text = hexOnly;
                                  }
                                  setModalState(() {});
                                }
                              },
                            ),
                            IconButton(
                              icon: const Icon(Icons.person, color: Colors.grey, size: 18),
                              tooltip: 'Paste My PeerId',
                              onPressed: () {
                                audienceCtrl.text = _bridge.myPeerId;
                                setModalState(() {});
                              },
                            ),
                          ],
                        ),
                      ),
                      style: const TextStyle(fontSize: 11, fontFamily: 'monospace'),
                    ),
                    if (_peers.isNotEmpty) ...[
                      const SizedBox(height: 4),
                      Wrap(
                        spacing: 6,
                        runSpacing: 4,
                        crossAxisAlignment: WrapCrossAlignment.center,
                        children: [
                          const Text('Audience from mesh:', style: TextStyle(fontSize: 10, color: Colors.grey)),
                          for (final p in _peers)
                            ActionChip(
                              avatar: const Icon(Icons.devices, size: 12, color: Colors.greenAccent),
                              label: Text(p, style: const TextStyle(fontSize: 10, fontFamily: 'monospace')),
                              padding: EdgeInsets.zero,
                              materialTapTargetSize: MaterialTapTargetSize.shrinkWrap,
                              onPressed: () {
                                final full = _bridge.getPeerFullId(p);
                                if (full != null && full.length >= 64) {
                                  audienceCtrl.text = full;
                                  setModalState(() {
                                    statusSuccess = true;
                                    statusMessage = 'Audience set to neighbor $p (64-hex).';
                                  });
                                } else {
                                  setModalState(() {
                                    statusSuccess = false;
                                    statusMessage = 'Neighbor $p key not synced yet. Scan "Show My QR" from the other phone.';
                                  });
                                }
                              },
                            ),
                        ],
                      ),
                    ],
                    const SizedBox(height: 8),
                    Row(
                      children: [
                        const Text('Capability: ', style: TextStyle(fontSize: 11, fontWeight: FontWeight.bold)),
                        const SizedBox(width: 8),
                        DropdownButton<String>(
                          value: selectedCap,
                          isDense: true,
                          items: const [
                            DropdownMenuItem(value: 'direct_message', child: Text('DirectMessage (1 hop)', style: TextStyle(fontSize: 12))),
                            DropdownMenuItem(value: 'gateway_forward', child: Text('GatewayForward (1 hop)', style: TextStyle(fontSize: 12))),
                          ],
                          onChanged: (v) {
                            if (v != null) {
                              setModalState(() => selectedCap = v);
                            }
                          },
                        ),
                      ],
                    ),
                    const SizedBox(height: 8),
                    SizedBox(
                      width: double.infinity,
                      child: ElevatedButton.icon(
                        icon: const Icon(Icons.qr_code, size: 16),
                        style: ElevatedButton.styleFrom(backgroundColor: const Color(0xFF2E7D32)),
                        onPressed: () {
                          final uri = _bridge.issueVoucher(
                            subjectPeerIdHex: subjectCtrl.text.trim(),
                            audiencePeerIdHex: audienceCtrl.text.trim(),
                            capability: selectedCap,
                          );
                          setModalState(() {
                            issuedVoucherUri = uri;
                            if (uri != null) {
                              statusSuccess = true;
                              statusMessage = 'Signed 1-hop voucher generated! Scan QR on Audience phone.';
                            } else {
                              statusSuccess = false;
                              statusMessage = 'Failed: check 64-hex Subject & Audience PeerIds.';
                            }
                          });
                          if (uri != null) {
                            _showQrDialog(
                              uri,
                              title: 'Signed 1-Hop Voucher QR',
                              subtitle: 'Have the Audience phone scan this QR to authorize the Subject peer for 1 hop.',
                            );
                          }
                        },
                        label: const Text('Generate & Show Voucher QR', style: TextStyle(fontSize: 12)),
                      ),
                    ),
                    if (issuedVoucherUri != null) ...[
                      const SizedBox(height: 6),
                      Row(
                        children: [
                          Expanded(
                            child: Text(
                              issuedVoucherUri!,
                              style: const TextStyle(fontSize: 9, fontFamily: 'monospace', color: Color(0xFF81C784)),
                              maxLines: 2,
                              overflow: TextOverflow.ellipsis,
                            ),
                          ),
                          IconButton(
                            icon: const Icon(Icons.copy, size: 15),
                            onPressed: () {
                              Clipboard.setData(ClipboardData(text: issuedVoucherUri!));
                              setModalState(() {
                                statusSuccess = true;
                                statusMessage = 'Voucher URI copied.';
                              });
                            },
                          ),
                          IconButton(
                            icon: const Icon(Icons.qr_code, size: 16),
                            onPressed: () => _showQrDialog(
                              issuedVoucherUri!,
                              title: 'Signed 1-Hop Voucher QR',
                              subtitle: 'Have the Audience phone scan this QR to authorize the Subject peer for 1 hop.',
                            ),
                          ),
                        ],
                      ),
                    ],
                    const Divider(height: 20),
                    const Text(
                      '3. Import 1-Hop Voucher (Requires Approved Introducer)',
                      style: TextStyle(fontSize: 13, fontWeight: FontWeight.bold, color: Colors.amberAccent),
                    ),
                    const SizedBox(height: 6),
                    Row(
                      children: [
                        Expanded(
                          child: TextField(
                            controller: importVoucherCtrl,
                            decoration: InputDecoration(
                              hintText: 'bitchat-voucher://...',
                              hintStyle: const TextStyle(fontSize: 11),
                              isDense: true,
                              suffixIcon: IconButton(
                                icon: const Icon(Icons.qr_code_scanner, color: Colors.amberAccent, size: 20),
                                tooltip: 'Scan Voucher QR',
                                onPressed: () async {
                                  final scanned = await Navigator.push<String>(
                                    context,
                                    MaterialPageRoute(builder: (ctx) => const QrScanScreen()),
                                  );
                                  if (scanned != null && scanned.trim().isNotEmpty) {
                                    importVoucherCtrl.text = scanned.trim();
                                    final res = _bridge.importVoucher(scanned.trim());
                                    final subjectShortId = res?['subject_short_id'] as String? ??
                                        (res?['subject'] != null ? (res!['subject'] as String).substring(0, 8) : null);
                                    setModalState(() {
                                      if (subjectShortId != null) {
                                        statusSuccess = true;
                                        statusMessage = 'SUCCESS: Peer $subjectShortId introduced via 1-hop voucher (scoped capability authorized)!';
                                      } else {
                                        statusSuccess = false;
                                        statusMessage = 'REJECTED: Untrusted introducer, wrong audience, or expired.';
                                      }
                                    });
                                    setState(() {});
                                  }
                                },
                              ),
                            ),
                            style: const TextStyle(fontSize: 11, fontFamily: 'monospace'),
                          ),
                        ),
                        const SizedBox(width: 8),
                        ElevatedButton(
                          onPressed: () {
                            final uri = importVoucherCtrl.text.trim();
                            final res = _bridge.importVoucher(uri);
                            final subjectShortId = res?['subject_short_id'] as String? ??
                                (res?['subject'] != null ? (res!['subject'] as String).substring(0, 8) : null);
                            setModalState(() {
                              if (subjectShortId != null) {
                                statusSuccess = true;
                                statusMessage = 'SUCCESS: Peer $subjectShortId introduced via 1-hop voucher!';
                              } else {
                                statusSuccess = false;
                                statusMessage = 'REJECTED: Untrusted introducer, wrong audience, or expired.';
                              }
                            });
                            setState(() {});
                          },
                          child: const Text('Import', style: TextStyle(fontSize: 11)),
                        ),
                      ],
                    ),
                  ],
                ),
              ),
            );
          },
        );
      },
    );
  }

  Future<void> _openQrScanner() async {
    final scanned = await Navigator.push<String>(
      context,
      MaterialPageRoute(builder: (ctx) => const QrScanScreen()),
    );
    if (!mounted || scanned == null || scanned.isEmpty) return;

    final trimmed = scanned.trim();

    // 1. Gateway Contact URI
    if (trimmed.startsWith('bitchat-gw://')) {
      final approved = _bridge.importAndApproveGatewayContact(trimmed);
      if (approved != null) {
        final ok = _bridge.connectToGatewayContact(trimmed);
        setState(() {});
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text(ok ? 'Connected to Gateway via QR' : 'Dial to gateway failed')),
        );
      } else {
        ScaffoldMessenger.of(context).showSnackBar(
          const SnackBar(content: Text('Invalid Gateway Contact QR')),
        );
      }
      return;
    }

    // 2. Signed 1-Hop Capability Voucher URI
    if (trimmed.startsWith('bitchat-voucher://')) {
      final res = _bridge.importVoucher(trimmed);
      final subjectShortId = res?['subject_short_id'] as String?;
      setState(() {});
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(
          content: Text(
            subjectShortId != null
                ? 'Introduced peer $subjectShortId via 1-hop voucher (scoped capability only)'
                : 'Rejected voucher QR (untrusted introducer, wrong audience, or expired)',
          ),
        ),
      );
      return;
    }

    // 3. Peer ShortId / Hex identifier
    String peerId = trimmed;
    if (peerId.startsWith('bitchat-id://')) {
      peerId = peerId.substring('bitchat-id://'.length);
    }
    final hexOnly = peerId.replaceAll(RegExp(r'[^0-9a-fA-F]'), '');
    if (hexOnly.length >= 16) {
      final shortId = hexOnly.substring(0, 16).toUpperCase();
      setState(() {
        _selectedRecipient = shortId;
      });
      _bridge.initiateHandshake(shortId);
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Selected recipient: $shortId')),
      );
    } else {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Unrecognized QR: $trimmed')),
      );
    }
  }
}

class QrScanScreen extends StatefulWidget {
  const QrScanScreen({super.key});

  @override
  State<QrScanScreen> createState() => _QrScanScreenState();
}

class _QrScanScreenState extends State<QrScanScreen> {
  final MobileScannerController _controller = MobileScannerController(
    detectionSpeed: DetectionSpeed.noDuplicates,
  );
  bool _handled = false;

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  void _onDetect(BarcodeCapture capture) {
    if (_handled) return;
    final barcode = capture.barcodes.firstOrNull;
    final raw = barcode?.rawValue?.trim();
    if (raw == null || raw.isEmpty) return;

    _handled = true;
    Navigator.of(context).pop(raw);
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      backgroundColor: Colors.black,
      appBar: AppBar(
        backgroundColor: Colors.black,
        title: const Text('Scan QR Code', style: TextStyle(fontSize: 18)),
        actions: [
          IconButton(
            icon: ValueListenableBuilder<MobileScannerState>(
              valueListenable: _controller,
              builder: (context, state, child) {
                final torch = state.torchState == TorchState.on;
                return Icon(torch ? Icons.flash_on : Icons.flash_off);
              },
            ),
            onPressed: () => _controller.toggleTorch(),
          ),
          IconButton(
            icon: const Icon(Icons.flip_camera_android),
            onPressed: () => _controller.switchCamera(),
          ),
        ],
      ),
      body: Stack(
        children: [
          MobileScanner(
            controller: _controller,
            onDetect: _onDetect,
          ),
          Center(
            child: Container(
              width: 250,
              height: 250,
              decoration: BoxDecoration(
                border: Border.all(color: const Color(0xFF4CAF50), width: 2),
                borderRadius: BorderRadius.circular(16),
              ),
            ),
          ),
          Positioned(
            bottom: 40,
            left: 20,
            right: 20,
            child: Container(
              padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 10),
              decoration: BoxDecoration(
                color: Colors.black.withValues(alpha: 0.7),
                borderRadius: BorderRadius.circular(8),
              ),
              child: const Text(
                'Point camera at another node\'s ID QR or Gateway Contact QR',
                textAlign: TextAlign.center,
                style: TextStyle(color: Colors.white, fontSize: 13),
              ),
            ),
          ),
        ],
      ),
    );
  }
}
