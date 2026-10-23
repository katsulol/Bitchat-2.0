import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'mesh_bridge.dart';

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  runApp(const BitchatApp());
}

class BitchatApp extends StatelessWidget {
  const BitchatApp({super.key});

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
      home: const MeshChatScreen(),
    );
  }
}

class MeshChatScreen extends StatefulWidget {
  const MeshChatScreen({super.key});

  @override
  State<MeshChatScreen> createState() => _MeshChatScreenState();
}

class _MeshChatScreenState extends State<MeshChatScreen> {
  final MeshBridge _bridge = MeshBridge();
  final TextEditingController _inputController = TextEditingController();
  final List<ChatMessage> _messages = [];
  List<String> _peers = [];
  String? _selectedRecipient; // null = Broadcast

  @override
  void initState() {
    super.initState();
    _bridge.startMesh();

    _bridge.messages.listen((msg) {
      setState(() {
        _messages.add(msg);
        if (_messages.length > 500) {
          _messages.removeRange(0, _messages.length - 500);
        }
      });
    });

    _bridge.peers.listen((peers) {
      setState(() {
        _peers = peers;
        // corrections.md: Never auto-reset selected recipient to broadcast on disconnect!
      });
    });
  }

  Future<void> _sendMessage() async {
    final text = _inputController.text.trim();
    if (text.isEmpty) return;

    try {
      if (_selectedRecipient == null) {
        await _bridge.sendBroadcast(text);
      } else {
        await _bridge.sendDirect(_selectedRecipient!, text);
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
    _bridge.stopMesh();
    _bridge.dispose();
    _inputController.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        backgroundColor: const Color(0xFF1E1E1E),
        title: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                const Text(
                  'Bitchat 2.0 Mesh',
                  style: TextStyle(fontSize: 18, fontWeight: FontWeight.bold),
                ),
                const SizedBox(width: 8),
                Container(
                  padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
                  decoration: BoxDecoration(
                    color: const Color(0xFF2E7D32),
                    borderRadius: BorderRadius.circular(4),
                  ),
                  child: const Text(
                    'v2.0.4',
                    style: TextStyle(
                      fontSize: 10,
                      fontWeight: FontWeight.bold,
                      color: Colors.white,
                      fontFamily: 'monospace',
                    ),
                  ),
                ),
              ],
            ),
            Row(
              children: [
                Text(
                  'ID: ${_bridge.myShortId}',
                  style: const TextStyle(fontSize: 12, color: Color(0xFF81C784), fontFamily: 'monospace'),
                ),
                const SizedBox(width: 6),
                InkWell(
                  onTap: () {
                    Clipboard.setData(ClipboardData(text: _bridge.myShortId));
                    ScaffoldMessenger.of(context).showSnackBar(
                      const SnackBar(content: Text('ShortId copied to clipboard')),
                    );
                  },
                  child: const Icon(Icons.copy, size: 14, color: Colors.grey),
                ),
              ],
            ),
          ],
        ),
        actions: [
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
                Text(
                  '${_peers.length} Neighbors:',
                  style: const TextStyle(fontSize: 13, fontWeight: FontWeight.bold),
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: _peers.isEmpty
                      ? const Text(
                          'Scanning for BLE mesh peers...',
                          style: TextStyle(fontSize: 12, color: Colors.grey, fontStyle: FontStyle.italic),
                        )
                      : SingleChildScrollView(
                          scrollDirection: Axis.horizontal,
                          child: Row(
                            children: _peers.map((peer) {
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
                                  },
                                ),
                              );
                            }).toList(),
                          ),
                        ),
                ),
              ],
            ),
          ),

          // Message recipient indicator banner (corrections.md: shows direct peer / offline status)
          if (_selectedRecipient != null)
            Container(
              padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 6),
              color: _peers.contains(_selectedRecipient)
                  ? Colors.blue.shade900.withAlpha(80)
                  : Colors.orange.shade900.withAlpha(80),
              child: Row(
                children: [
                  Icon(
                    _peers.contains(_selectedRecipient) ? Icons.send : Icons.cloud_off,
                    size: 14,
                    color: _peers.contains(_selectedRecipient) ? Colors.blueAccent : Colors.orangeAccent,
                  ),
                  const SizedBox(width: 6),
                  Text(
                    _peers.contains(_selectedRecipient)
                        ? 'Direct to: ${_selectedRecipient!.length > 8 ? _selectedRecipient!.substring(0, 8) : _selectedRecipient}'
                        : 'Peer ${_selectedRecipient!.length > 8 ? _selectedRecipient!.substring(0, 8) : _selectedRecipient} (Offline)',
                    style: TextStyle(
                      fontSize: 12,
                      color: _peers.contains(_selectedRecipient) ? Colors.blueAccent : Colors.orangeAccent,
                    ),
                  ),
                  const Spacer(),
                  GestureDetector(
                    onTap: () => setState(() => _selectedRecipient = null),
                    child: const Text('Cancel', style: TextStyle(fontSize: 12, color: Colors.white70)),
                  ),
                ],
              ),
            ),

          // Message feed
          Expanded(
            child: _messages.isEmpty
                ? Center(
                    child: Column(
                      mainAxisAlignment: MainAxisAlignment.center,
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
                                      color: msg.isBroadcast ? Colors.amber.shade900 : Colors.purple.shade900,
                                      borderRadius: BorderRadius.circular(4),
                                    ),
                                    child: Text(
                                      msg.isBroadcast ? 'PUB' : 'DIRECT',
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
                            ],
                          ),
                        ),
                      );
                    },
                  ),
          ),

          // Message input bar
          Container(
            padding: const EdgeInsets.all(8),
            color: const Color(0xFF1E1E1E),
            child: SafeArea(
              child: Row(
                children: [
                  ActionChip(
                    avatar: Icon(
                      _selectedRecipient == null ? Icons.campaign : Icons.lock,
                      size: 16,
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
                  const SizedBox(width: 8),
                  Expanded(
                    child: TextField(
                      controller: _inputController,
                      decoration: InputDecoration(
                        hintText: _selectedRecipient == null ? 'Broadcast message to mesh...' : 'Private message...',
                        border: OutlineInputBorder(
                          borderRadius: BorderRadius.circular(24),
                          borderSide: BorderSide.none,
                        ),
                        filled: true,
                        fillColor: const Color(0xFF2C2C2C),
                        contentPadding: const EdgeInsets.symmetric(horizontal: 16, vertical: 10),
                      ),
                      onSubmitted: (_) => _sendMessage(),
                    ),
                  ),
                  const SizedBox(width: 6),
                  IconButton(
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

  void _showNodeDetails() {
    showModalBottomSheet(
      context: context,
      backgroundColor: const Color(0xFF1E1E1E),
      builder: (context) {
        return Padding(
          padding: const EdgeInsets.all(20),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              const Text(
                'Bitchat 2.0 Node Details',
                style: TextStyle(fontSize: 18, fontWeight: FontWeight.bold),
              ),
              const SizedBox(height: 12),
              Text('ShortId: ${_bridge.myShortId}', style: const TextStyle(fontFamily: 'monospace')),
              const SizedBox(height: 6),
              const Text('Full Ed25519 PeerId:', style: TextStyle(fontSize: 12, color: Colors.grey)),
              Text(_bridge.myPeerId, style: const TextStyle(fontSize: 10, fontFamily: 'monospace')),
              const Divider(height: 24),
              Row(
                mainAxisAlignment: MainAxisAlignment.spaceAround,
                children: [
                  _statItem('Tx', _bridge.transmissions),
                  _statItem('Delivered', _bridge.deliveries),
                  _statItem('Suppressed', _bridge.relaysSuppressed),
                  _statItem('Duplicates', _bridge.duplicatesDropped),
                ],
              ),
            ],
          ),
        );
      },
    );
  }

  Widget _statItem(String label, int val) {
    return Column(
      children: [
        Text(val.toString(), style: const TextStyle(fontSize: 18, fontWeight: FontWeight.bold, color: Color(0xFF4CAF50))),
        Text(label, style: const TextStyle(fontSize: 11, color: Colors.grey)),
      ],
    );
  }

  void _showGatewayDialog() {
    showModalBottomSheet(
      context: context,
      isScrollControlled: true,
      backgroundColor: const Color(0xFF1E1E1E),
      builder: (ctx) {
        return StatefulBuilder(
          builder: (context, setModalState) {
            final stats = _bridge.getGatewayStats();
            final isRunning = stats?.isRunning ?? false;
            final contactUri = isRunning ? _bridge.exportGatewayContact() : null;

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
                    if (isRunning && stats?.endpointId != null) ...[
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
                    ],
                    if (isRunning && contactUri != null) ...[
                      const SizedBox(height: 8),
                      const Text('Gateway Contact URI:', style: TextStyle(fontSize: 11, color: Colors.grey)),
                      Row(
                        children: [
                          Expanded(
                            child: Text(
                              contactUri,
                              style: const TextStyle(fontSize: 10, fontFamily: 'monospace'),
                              maxLines: 2,
                              overflow: TextOverflow.ellipsis,
                            ),
                          ),
                          IconButton(
                            icon: const Icon(Icons.copy, size: 16),
                            onPressed: () {
                              Clipboard.setData(ClipboardData(text: contactUri));
                              ScaffoldMessenger.of(context).showSnackBar(
                                const SnackBar(content: Text('Contact URI copied')),
                              );
                            },
                          ),
                        ],
                      ),
                    ],
                    const Divider(height: 24),
                    if (isRunning) ...[
                      Row(
                        mainAxisAlignment: MainAxisAlignment.spaceAround,
                        children: [
                          _statItem('QUIC Conns', stats?.activeConnections ?? 0),
                          _statItem('Imported', stats?.forwardsImported ?? 0),
                          _statItem('Exported', stats?.forwardsExported ?? 0),
                          _statItem('Loops Prevented', stats?.loopsPrevented ?? 0),
                        ],
                      ),
                      const SizedBox(height: 16),
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
                              label: const Text('Set Route', style: TextStyle(fontSize: 12)),
                              style: ElevatedButton.styleFrom(backgroundColor: const Color(0xFF2C2C2C)),
                              onPressed: () => _promptSetRoute(setModalState),
                            ),
                          ),
                        ],
                      ),
                      const SizedBox(height: 12),
                    ],
                    SizedBox(
                      width: double.infinity,
                      child: ElevatedButton(
                        style: ElevatedButton.styleFrom(
                          backgroundColor: isRunning ? Colors.red.shade900 : Colors.green.shade800,
                        ),
                        onPressed: () async {
                          if (isRunning) {
                            await _bridge.stopGateway();
                          } else {
                            await _bridge.startGateway();
                          }
                          setModalState(() {});
                          setState(() {});
                        },
                        child: Text(isRunning ? 'Stop Gateway Bridge' : 'Start Gateway Bridge'),
                      ),
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
}
