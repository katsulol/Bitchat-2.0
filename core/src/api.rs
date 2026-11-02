use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tokio::sync::RwLock;

use serde::{Deserialize, Serialize};

use crate::envelope::{pad_inner_payload, unpad_bucket, MsgType};
use crate::identity::{FileKeyStore, Identity, KeyStore, PeerId, ShortId};
use crate::mesh::delivery::{
    ApplicationFrame, ApplicationFrameKind, DedupStatus, DeliveryAction, DeliveryCoordinator,
    EnqueueMessage, InboxDeduplicator,
};
use crate::mesh::peer::PeerSessionManager;
use crate::mesh::policy::CoverageAwareFlood;
use crate::mesh::relay::MeshNode;
use crate::storage::{
    InjectedKeyProvider, KeyProvider, MemoryOutboxStore, MessageMode, OutboxLimits, OutboxManager,
    SendResult, SqliteOutboxStore,
};
use crate::transport::bridge::{BridgeHandle, BridgeTransport};

// ponytail: flutter_rust_bridge API surface wrapping core identity, mesh routing, and BLE bridge transport.

/// Status transition event emitted to Flutter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryEventRecord {
    pub logical_id: String,
    pub state: String,
    pub recipient: String,
    pub timestamp: u64,
}

/// Comprehensive diagnostics snapshot for UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticsSnapshot {
    pub my_short_id: String,
    pub my_peer_id: String,
    pub connected_peers_count: usize,
    pub outbox_queued_count: usize,
    pub outbox_inflight_count: usize,
    pub deliveries_count: u64,
    pub gateway_active: bool,
    pub gateway_relay_disabled: bool,
    pub last_route_candidate: Option<String>,
}

/// High-level routing metrics snapshot for mobile UI.
#[derive(Debug, Clone, Default)]
pub struct BridgeStats {
    pub transmissions: u64,
    pub duplicates_dropped: u64,
    pub relays_suppressed: u64,
    pub deliveries: u64,
}

/// Decoded chat message delivered to Flutter layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeMessage {
    pub msg_id: String,
    pub sender: String,
    pub recipient: Option<String>,
    pub text: String,
    pub hop_count: u8,
    pub is_broadcast: bool,
}

/// Outbound BLE radio frame emitted by Rust core to be written to peripheral/central GATT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeOutbound {
    pub to: Option<String>,
    pub bytes: Vec<u8>,
}

/// Core singleton service managing the node runtime for Flutter.
pub struct BitchatCoreService {
    node: MeshNode<BridgeTransport>,
    handle: BridgeHandle,
    peer_mgr: PeerSessionManager,
    outbox_mgr: OutboxManager,
    delivery_coord: DeliveryCoordinator,
    dedup: InboxDeduplicator,
    delivery_events: Vec<DeliveryEventRecord>,
    gateway_bridge: Option<std::sync::Arc<crate::gateway::GatewayBridge>>,
    last_route_candidate: Option<String>,
}

static SERVICE: OnceLock<RwLock<Option<BitchatCoreService>>> = OnceLock::new();

fn get_service() -> &'static RwLock<Option<BitchatCoreService>> {
    SERVICE.get_or_init(|| RwLock::new(None))
}

fn hex_to_short_id(hex_str: &str) -> Result<ShortId, String> {
    let hex_clean = hex_str.trim().trim_start_matches("0x");
    if !hex_clean.is_ascii() || hex_clean.len() != 16 {
        return Err(format!(
            "Invalid ShortId: expected 16 ASCII hexadecimal characters, got len {}",
            hex_clean.len()
        ));
    }
    let mut bytes = [0u8; 8];
    for i in 0..8 {
        let byte_str = &hex_clean[i * 2..i * 2 + 2];
        bytes[i] = u8::from_str_radix(byte_str, 16)
            .map_err(|e| format!("Invalid hex character at byte {i}: {e}"))?;
    }
    Ok(ShortId(bytes))
}

/// Initialize the Bitchat core service, loading or generating long-term identity in `storage_path`.
pub async fn init(storage_path: String) -> Result<String, String> {
    init_with_key(storage_path, None, 1).await
}

/// Initialize Bitchat core service with explicit DEK key from platform keystore (Android Keystore).
pub async fn init_with_key(
    storage_path: String,
    dek: Option<[u8; 32]>,
    key_version: u32,
) -> Result<String, String> {
    let mut lock = get_service().write().await;
    if let Some(svc) = lock.as_ref() {
        return Ok(svc.node.identity().short_id().to_string());
    }

    let path = PathBuf::from(&storage_path).join("identity.key");
    let keystore = FileKeyStore::new(path);
    let identity = if let Some(id) = keystore
        .load()
        .map_err(|e| format!("Failed to load identity: {e}"))?
    {
        id
    } else {
        let new_id = Identity::generate();
        keystore
            .save(&new_id)
            .map_err(|e| format!("Failed to save identity: {e}"))?;
        new_id
    };

    let short_id = identity.short_id();
    let peer_id = identity.peer_id();
    let (transport, handle) = BridgeTransport::new(short_id);
    let node = MeshNode::new(identity, transport, Box::new(CoverageAwareFlood::default()));

    let db_path = PathBuf::from(&storage_path).join("bitchat_outbox.db");
    let key_provider: Arc<dyn KeyProvider> = match dek {
        Some(k) => Arc::new(InjectedKeyProvider::new(k, key_version)),
        None => Arc::new(InjectedKeyProvider::random(key_version)),
    };
    let limits = OutboxLimits::default();
    let sqlite_store = SqliteOutboxStore::open(&db_path, key_provider, limits)
        .map_err(|e| format!("Failed to open SQLite store: {e:?}"))?;
    let memory_store = MemoryOutboxStore::new(limits);
    let outbox_mgr = OutboxManager::new(peer_id, sqlite_store, memory_store, limits);
    let delivery_coord = DeliveryCoordinator::new(peer_id);
    let dedup = InboxDeduplicator::new(10_000, std::time::Duration::from_secs(86400));

    let service = BitchatCoreService {
        node,
        handle,
        peer_mgr: PeerSessionManager::new(),
        outbox_mgr,
        delivery_coord,
        dedup,
        delivery_events: Vec::new(),
        gateway_bridge: None,
        last_route_candidate: None,
    };

    *lock = Some(service);
    Ok(short_id.to_string())
}

/// Query our 32-byte Ed25519 PeerId public key hex string.
pub async fn my_peer_id() -> Result<String, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    Ok(svc.node.identity().peer_id().to_string())
}

/// Query our 8-byte ShortId hex string.
pub async fn my_short_id() -> Result<String, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    Ok(svc.node.identity().short_id().to_string())
}

/// Broadcast a public plaintext chat message to all peers in the mesh.
pub async fn send_broadcast(text: String) -> Result<String, String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;

    let msg_id = svc
        .node
        .send_broadcast(text.into_bytes())
        .await
        .map_err(|e| format!("Send broadcast failed: {e}"))?;

    Ok(hex::encode(msg_id))
}

/// Send an end-to-end encrypted or addressed direct chat message to `recipient_short_id`.
pub async fn send_to(recipient_short_id: String, text: String) -> Result<String, String> {
    let target = hex_to_short_id(&recipient_short_id)?;
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;

    // If an E2E session is established and finished, encrypt payload
    if let Some(session) = svc.peer_mgr.get_session(&target) {
        if session.is_handshake_finished() {
            let msg_id = svc
                .peer_mgr
                .send_encrypted(&mut svc.node, target, text.as_bytes())
                .await
                .map_err(|e| format!("E2E encrypted send failed: {e}"))?;
            return Ok(hex::encode(msg_id));
        } else if session.is_failed() {
            let _ = svc.peer_mgr.initiate_handshake(&mut svc.node, target).await;
        }
    } else {
        // No session exists: automatically initiate Noise XX handshake!
        let _ = svc.peer_mgr.initiate_handshake(&mut svc.node, target).await;
    }

    // Fail closed: Do NOT silently send plaintext
    Err(
        "Securing direct E2E channel with peer... Handshake started. Please tap send again."
            .to_string(),
    )
}

/// Send an end-to-end encrypted direct chat message with persistent outbox and delivery tracking.
///
/// `mode_u8`: 0 = Normal (SQLite encrypted), 1 = Privacy (volatile RAM only).
/// Returns 32-character hexadecimal logical message ID upon successful outbox admission.
pub async fn send_message(
    recipient_short_id: String,
    text: String,
    mode_u8: u8,
) -> Result<String, String> {
    let target = hex_to_short_id(&recipient_short_id)?;
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;

    let mode = match mode_u8 {
        1 => MessageMode::Privacy,
        _ => MessageMode::Normal,
    };

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let recipient_peer_id = svc
        .peer_mgr
        .get_session(&target)
        .and_then(|s| s.peer_id())
        .unwrap_or_else(|| {
            let mut b = [0u8; 32];
            b[..8].copy_from_slice(&target.0);
            PeerId(b)
        });

    let send_res = svc.outbox_mgr.send_to(
        recipient_peer_id,
        text.as_bytes().to_vec(),
        mode,
        None,
        now_secs,
    );

    let (logical_id, initial_state) = match send_res {
        SendResult::Accepted { logical_id, state } => (logical_id, state),
        SendResult::Rejected(e) => return Err(format!("Send rejected: {e:?}")),
    };

    let logical_id_hex = hex::encode(logical_id);

    svc.delivery_events.push(DeliveryEventRecord {
        logical_id: logical_id_hex.clone(),
        state: format!("{initial_state:?}"),
        recipient: recipient_short_id.clone(),
        timestamp: now_secs,
    });

    svc.delivery_coord.enqueue(
        EnqueueMessage {
            logical_id,
            recipient_peer_id,
            recipient_short_id: target,
            body: text.as_bytes().to_vec(),
            ttl: mode.default_ttl(),
            max_attempts: 12,
            is_privacy_mode: mode == MessageMode::Privacy,
        },
        std::time::Instant::now(),
    );

    // If session ready, attempt immediate dispatch
    if svc.peer_mgr.is_session_ready(&target) {
        let app_frame = ApplicationFrame::new_message(
            logical_id,
            svc.node.identity().peer_id(),
            recipient_peer_id,
            text.as_bytes().to_vec(),
            mode.default_ttl(),
            now_secs,
        );
        let frame_bytes = app_frame.encode();
        let padded =
            pad_inner_payload(&frame_bytes, true).map_err(|e| format!("Padding error: {e}"))?;

        let _ = svc
            .peer_mgr
            .send_encrypted(&mut svc.node, target, &padded)
            .await;
        let _ = svc.delivery_coord.mark_in_flight(
            &logical_id,
            std::time::Duration::from_millis(500),
            std::time::Instant::now(),
        );
        let _ = svc.outbox_mgr.mark_in_flight(&logical_id, now_secs);

        svc.delivery_events.push(DeliveryEventRecord {
            logical_id: logical_id_hex.clone(),
            state: "InFlight".to_string(),
            recipient: recipient_short_id.clone(),
            timestamp: now_secs,
        });
        svc.last_route_candidate = Some(format!("DirectBle({target})"));
    } else {
        // Initiate handshake
        let _ = svc.peer_mgr.initiate_handshake(&mut svc.node, target).await;
        svc.last_route_candidate = Some(format!("SessionHandshakeRequired({target})"));
    }

    Ok(logical_id_hex)
}

/// Query current delivery state of a logical message.
pub async fn get_delivery_state(logical_id_hex: String) -> Result<String, String> {
    let clean = logical_id_hex.trim().trim_start_matches("0x");
    if clean.len() != 32 {
        return Err("Invalid logical_id: expected 32 hex chars".to_string());
    }
    let mut logical_id = [0u8; 16];
    for i in 0..16 {
        logical_id[i] = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16)
            .map_err(|e| format!("Invalid hex: {e}"))?;
    }

    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;

    if let Some(st) = svc.delivery_coord.get_state(&logical_id) {
        return Ok(format!("{st:?}"));
    }
    if let Ok(Some(rec)) = svc.outbox_mgr.get(&logical_id) {
        return Ok(format!("{:?}", rec.state));
    }
    Err("Logical message not found".to_string())
}

/// Drain queued delivery transition events emitted by the delivery subsystem.
pub async fn poll_delivery_events() -> Result<String, String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    let events = std::mem::take(&mut svc.delivery_events);
    serde_json::to_string(&events).map_err(|e| format!("Serialization error: {e}"))
}

/// Retrieve comprehensive routing diagnostics and quota snapshot for mobile UI.
pub async fn get_diagnostics() -> Result<String, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;

    let diag = DiagnosticsSnapshot {
        my_short_id: svc.node.identity().short_id().to_string(),
        my_peer_id: svc.node.identity().peer_id().to_string(),
        connected_peers_count: svc.handle.connected_peers().len(),
        outbox_queued_count: svc.delivery_coord.queued_count(),
        outbox_inflight_count: svc.delivery_coord.in_flight_count(),
        deliveries_count: svc.node.stats().deliveries,
        gateway_active: svc.gateway_bridge.is_some(),
        gateway_relay_disabled: true,
        last_route_candidate: svc.last_route_candidate.clone(),
    };

    serde_json::to_string(&diag).map_err(|e| format!("Serialization error: {e}"))
}

/// Step the outbox pump, processing timeouts, retries, and expiration.
pub async fn pump_outbox() -> Result<usize, String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;

    let now_instant = std::time::Instant::now();
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let actions = svc.delivery_coord.check_timeouts(now_instant);
    let mut handled = 0;

    for action in actions {
        match action {
            DeliveryAction::ReadyToRetry(logical_id) => {
                if let Some(record) = svc.delivery_coord.get_record(&logical_id).cloned() {
                    let target = record.recipient_short_id;
                    if svc.peer_mgr.is_session_ready(&target) {
                        let app_frame = ApplicationFrame::new_message(
                            logical_id,
                            svc.node.identity().peer_id(),
                            record.recipient_peer_id,
                            record.body.clone(),
                            std::time::Duration::from_secs(300),
                            now_secs,
                        );
                        let frame_bytes = app_frame.encode();
                        if let Ok(padded) = pad_inner_payload(&frame_bytes, true) {
                            let _ = svc
                                .peer_mgr
                                .send_encrypted(&mut svc.node, target, &padded)
                                .await;
                            let delay_ms = (1000u64 * (1u64 << record.attempts.min(4))).min(15000);
                            let _ = svc.delivery_coord.mark_in_flight(
                                &logical_id,
                                std::time::Duration::from_millis(delay_ms),
                                now_instant,
                            );
                            let _ = svc.outbox_mgr.mark_in_flight(&logical_id, now_secs);
                            handled += 1;
                        }
                    } else {
                        let _ = svc.peer_mgr.initiate_handshake(&mut svc.node, target).await;
                        let delay_ms = (1000u64 * (1u64 << record.attempts.min(4))).min(10000);
                        let _ = svc.delivery_coord.mark_in_flight(
                            &logical_id,
                            std::time::Duration::from_millis(delay_ms),
                            now_instant,
                        );
                    }
                }
            }
            DeliveryAction::Expired(logical_id) => {
                let _ = svc.outbox_mgr.mark_failed(&logical_id);
                svc.delivery_events.push(DeliveryEventRecord {
                    logical_id: hex::encode(logical_id),
                    state: "Expired".to_string(),
                    recipient: "".to_string(),
                    timestamp: now_secs,
                });
                handled += 1;
            }
            DeliveryAction::Failed(logical_id) => {
                let _ = svc.outbox_mgr.mark_failed(&logical_id);
                svc.delivery_events.push(DeliveryEventRecord {
                    logical_id: hex::encode(logical_id),
                    state: "Failed".to_string(),
                    recipient: "".to_string(),
                    timestamp: now_secs,
                });
                handled += 1;
            }
        }
    }

    Ok(handled)
}

/// Explicitly initiate a mutual Noise XX handshake with a peer across mesh or gateway.
pub async fn initiate_handshake(recipient_short_id: String) -> Result<(), String> {
    let target = hex_to_short_id(&recipient_short_id)?;
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;

    if !svc.peer_mgr.is_session_ready(&target) {
        svc.peer_mgr
            .initiate_handshake(&mut svc.node, target)
            .await
            .map_err(|e| format!("Initiate handshake failed: {e}"))?;
    }
    Ok(())
}

/// Check if a completed Noise XX E2E session exists with `recipient_short_id`.
pub async fn is_session_ready(recipient_short_id: String) -> Result<bool, String> {
    let target = hex_to_short_id(&recipient_short_id)?;
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    Ok(svc.peer_mgr.is_session_ready(&target))
}

/// Get current routing metrics snapshot.
pub async fn get_stats() -> Result<BridgeStats, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    let snap = svc.node.stats();
    Ok(BridgeStats {
        transmissions: snap.transmissions,
        duplicates_dropped: snap.duplicates_dropped,
        relays_suppressed: snap.relays_suppressed,
        deliveries: snap.deliveries,
    })
}

/// Notify Rust that a BLE peer connected.
pub async fn on_peer_connected(short_id: String) -> Result<(), String> {
    let peer = hex_to_short_id(&short_id)?;
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.handle.on_peer_connected(peer);

    // Reset backoff for any queued messages waiting for this peer
    svc.delivery_coord
        .reset_backoff_for_peer(&peer, std::time::Instant::now());

    // Auto-initiate handshake with peer if lower ShortId and session not ready
    let my_short = svc.node.identity().short_id();
    if my_short < peer && !svc.peer_mgr.is_session_ready(&peer) {
        let _ = svc.peer_mgr.initiate_handshake(&mut svc.node, peer).await;
    }

    if let Some(ref bridge) = svc.gateway_bridge {
        let peers = svc.handle.connected_peers();
        let bridge = std::sync::Arc::clone(bridge);
        tokio::spawn(async move {
            let _ = bridge.announce_local_peers(&peers).await;
        });
    }
    Ok(())
}

/// Notify Rust that a BLE peer disconnected.
pub async fn on_peer_disconnected(short_id: String) -> Result<(), String> {
    let peer = hex_to_short_id(&short_id)?;
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    svc.handle.on_peer_disconnected(peer);
    if let Some(ref bridge) = svc.gateway_bridge {
        let peers = svc.handle.connected_peers();
        let bridge = std::sync::Arc::clone(bridge);
        tokio::spawn(async move {
            let _ = bridge.announce_local_peers(&peers).await;
        });
    }
    Ok(())
}

/// Deliver an incoming BLE frame payload from a connected peer into Rust.
pub async fn on_frame_received(short_id: String, bytes: Vec<u8>) -> Result<(), String> {
    let peer = hex_to_short_id(&short_id)?;
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    svc.handle
        .on_frame_received(peer, bytes)
        .map_err(|e| format!("Failed to deliver frame: {e}"))
}

/// Poll next outbound frame from Rust to be transmitted across mobile BLE.
pub async fn poll_outbound() -> Result<Option<BridgeOutbound>, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    let frame_opt = svc.node.transport().try_recv_outbound();
    Ok(frame_opt.map(|f| BridgeOutbound {
        to: f.to.map(|s| s.to_string()),
        bytes: f.bytes,
    }))
}

/// Step node and poll next delivered inbound message for UI display.
pub async fn poll_inbound() -> Result<Option<BridgeMessage>, String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;

    // Step discrete event loop with bounded budget (max 10 steps per poll)
    let mut steps = 0;
    while steps < 10 {
        match svc.node.poll_step(std::time::Instant::now()).await {
            Ok(true) => steps += 1,
            Ok(false) => break,
            Err(e) => {
                tracing::warn!("poll_step error: {e}");
                break;
            }
        }
    }

    if let Some(msg) = svc.node.recv_inbound() {
        // If message is a handshake frame, handle it internally
        if msg.msg_type == MsgType::Handshake {
            let _ = svc.peer_mgr.handle_handshake(&mut svc.node, &msg).await;
            return Ok(None);
        }

        // Try decrypting if active session exists
        if let Ok(decrypted) = svc.peer_mgr.decrypt_inbound(&msg) {
            let unpadded =
                unpad_bucket(&decrypted.plaintext).unwrap_or_else(|_| decrypted.plaintext.clone());

            if let Ok(app_frame) = ApplicationFrame::decode(&unpadded) {
                let now_secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

                match app_frame.kind {
                    ApplicationFrameKind::Message => {
                        let dedup_status = svc.dedup.check_or_insert(
                            &app_frame.sender_peer_id,
                            &app_frame.logical_id,
                            std::time::Instant::now(),
                        );

                        // Auto-emit authenticated ACK
                        let ack = ApplicationFrame::new_ack(
                            app_frame.logical_id,
                            svc.node.identity().peer_id(),
                            app_frame.sender_peer_id,
                            now_secs,
                        );
                        let ack_bytes = ack.encode();
                        if let Ok(padded_ack) = pad_inner_payload(&ack_bytes, true) {
                            let _ = svc
                                .peer_mgr
                                .send_encrypted(&mut svc.node, decrypted.origin_sender, &padded_ack)
                                .await;
                        }

                        if dedup_status == DedupStatus::New {
                            let text = String::from_utf8_lossy(&app_frame.body).to_string();
                            return Ok(Some(BridgeMessage {
                                msg_id: hex::encode(app_frame.logical_id),
                                sender: decrypted.origin_sender.to_string(),
                                recipient: Some(svc.node.short_id().to_string()),
                                text,
                                hop_count: decrypted.hop_count,
                                is_broadcast: false,
                            }));
                        } else {
                            // Deduplicated: suppress duplicate display
                            return Ok(None);
                        }
                    }
                    ApplicationFrameKind::Ack => {
                        let _ = svc
                            .delivery_coord
                            .handle_ack(&app_frame, &app_frame.sender_peer_id);
                        let _ = svc.outbox_mgr.mark_delivered(&app_frame.logical_id);
                        svc.delivery_events.push(DeliveryEventRecord {
                            logical_id: hex::encode(app_frame.logical_id),
                            state: "Delivered".to_string(),
                            recipient: app_frame.sender_peer_id.to_string(),
                            timestamp: now_secs,
                        });
                        return Ok(None);
                    }
                }
            } else {
                let text = String::from_utf8_lossy(&decrypted.plaintext).to_string();
                return Ok(Some(BridgeMessage {
                    msg_id: hex::encode(msg.msg_id),
                    sender: decrypted.origin_sender.to_string(),
                    recipient: Some(svc.node.short_id().to_string()),
                    text,
                    hop_count: decrypted.hop_count,
                    is_broadcast: false,
                }));
            }
        }

        // Explicit public chat message validation
        if msg.msg_type == MsgType::Chat && msg.recipient.is_broadcast() {
            let text = String::from_utf8_lossy(&msg.payload).to_string();
            return Ok(Some(BridgeMessage {
                msg_id: hex::encode(msg.msg_id),
                sender: msg.origin_sender.to_string(),
                recipient: None,
                text,
                hop_count: msg.hop_count,
                is_broadcast: true,
            }));
        }

        // Section 1.D: Reject failed decryption or invalid message type! Never display ciphertext as text!
        return Ok(None);
    }

    Ok(None)
}

/// Return list of currently connected BLE peer ShortIds.
pub async fn connected_peers() -> Result<Vec<String>, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    Ok(svc
        .handle
        .connected_peers()
        .into_iter()
        .map(|p| p.to_string())
        .collect())
}

/// Start internet gateway bridge on local node.
pub async fn gateway_start(relay_mode_str: Option<String>) -> Result<String, String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;

    if svc.gateway_bridge.is_some() {
        return Err("Gateway bridge is already active".to_string());
    }

    let relay_mode = match relay_mode_str.as_deref() {
        Some("disabled") => iroh::RelayMode::Disabled,
        _ => iroh::RelayMode::Default,
    };

    let id_bytes = svc.node.identity().to_bytes();
    let id_clone = Identity::from_bytes(&id_bytes);

    let bridge = crate::gateway::GatewayBridge::new(id_clone, None, relay_mode)
        .await
        .map_err(|e| format!("Failed to create GatewayBridge: {e}"))?;

    let bridge = std::sync::Arc::new(bridge);
    svc.node.set_gateway_bridge(std::sync::Arc::clone(&bridge));

    // Spawn accept loop for incoming gateway peer connections
    let ep = bridge.endpoint().clone();
    let bridge_clone = std::sync::Arc::clone(&bridge);
    tokio::spawn(async move {
        while let Ok((_remote_id, conn)) = crate::gateway::iroh_link::accept_gateway_conn(&ep).await
        {
            let bridge_inner = std::sync::Arc::clone(&bridge_clone);
            tokio::spawn(async move {
                let _ = bridge_inner.spawn_session_handler(conn, None).await;
            });
        }
    });

    let endpoint_id_str = bridge.endpoint_id().to_string();
    svc.gateway_bridge = Some(bridge);
    Ok(endpoint_id_str)
}

/// Stop internet gateway bridge on local node.
pub async fn gateway_stop() -> Result<(), String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;

    if let Some(bridge) = svc.gateway_bridge.take() {
        svc.node.clear_gateway_bridge();
        bridge.close().await;
        Ok(())
    } else {
        Err("Gateway bridge is not running".to_string())
    }
}

/// Export this node's gateway pairing contact string (URI) for out-of-band QR sharing.
/// Generates a bounded, single-use pairing invitation capability with 5-minute monotonic expiry.
pub async fn gateway_export_contact() -> Result<String, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;

    let bridge = svc
        .gateway_bridge
        .as_ref()
        .ok_or("Gateway bridge is not running")?;
    let contact = bridge
        .export_pairing_contact(None, true)
        .await
        .map_err(|e| format!("Failed to export contact: {e}"))?;

    contact
        .export_to_string()
        .map_err(|e| format!("Failed to serialize contact: {e}"))
}

/// Import, verify, and approve a remote gateway contact string.
pub async fn gateway_import_and_approve_contact(contact_str: String) -> Result<String, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;

    let bridge = svc
        .gateway_bridge
        .as_ref()
        .ok_or("Gateway bridge is not running")?;
    let contact = bridge
        .import_and_approve_contact(&contact_str)
        .await
        .map_err(|e| format!("Failed to import contact: {e}"))?;

    Ok(contact.iroh_endpoint_id_hex)
}

/// Dial and establish connection to an approved remote gateway contact URI.
pub async fn gateway_connect_to_contact(contact_str: String) -> Result<(), String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;

    let bridge = svc
        .gateway_bridge
        .as_ref()
        .ok_or("Gateway bridge is not running")?;
    let contact = crate::gateway::GatewayContact::import_from_str(&contact_str)
        .map_err(|e| format!("Invalid contact string: {e}"))?;

    bridge
        .connect_to_contact(&contact)
        .await
        .map_err(|e| format!("Failed to connect to gateway: {e}"))
}

/// Set manual route mapping a destination ShortId hex to a target Gateway EndpointId hex.
pub async fn gateway_set_route(
    recipient_short_id: String,
    target_endpoint_id_hex: String,
) -> Result<(), String> {
    let short_id = hex_to_short_id(&recipient_short_id)?;
    let endpoint_id = target_endpoint_id_hex
        .parse::<iroh::EndpointId>()
        .map_err(|e| format!("Invalid endpoint ID hex: {e}"))?;

    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;

    let bridge = svc
        .gateway_bridge
        .as_ref()
        .ok_or("Gateway bridge is not running")?;
    bridge
        .routes()
        .write()
        .await
        .set_route(short_id, endpoint_id);

    Ok(())
}

/// Get current gateway statistics snapshot as JSON string.
pub async fn gateway_get_stats() -> Result<String, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;

    if let Some(bridge) = svc.gateway_bridge.as_ref() {
        let stats = bridge.stats().snapshot();
        let last_err_json = match stats.last_error {
            Some(e) => format!("\"{}\"", e.replace('\\', "\\\\").replace('\"', "\\\"")),
            None => "null".to_string(),
        };
        Ok(format!(
            "{{\"is_running\":true,\"endpoint_id\":\"{}\",\"active_connections\":{},\"messages_imported\":{},\"messages_exported\":{},\"loops_prevented\":{},\"messages_rejected\":{},\"last_error\":{}}}",
            bridge.endpoint_id(),
            stats.active_connections,
            stats.forwards_imported,
            stats.forwards_exported,
            stats.loops_prevented,
            stats.forwards_rejected,
            last_err_json,
        ))
    } else {
        Ok("{\"is_running\":false,\"endpoint_id\":null,\"active_connections\":0,\"messages_imported\":0,\"messages_exported\":0,\"loops_prevented\":0,\"messages_rejected\":0,\"last_error\":null}".to_string())
    }
}

/// Query list of remote recipient ShortIds currently reachable through active gateway bridges.
pub async fn gateway_get_routes() -> Result<Vec<String>, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;

    if let Some(bridge) = svc.gateway_bridge.as_ref() {
        let my_short = svc.node.identity().short_id();
        let routes = bridge.routes().read().await.list_routes();
        Ok(routes
            .into_iter()
            .map(|(sid, _)| sid)
            .filter(|sid| *sid != my_short)
            .map(|sid| sid.to_string())
            .collect())
    } else {
        Ok(Vec::new())
    }
}

mod hex {
    pub fn encode<T: AsRef<[u8]>>(data: T) -> String {
        data.as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_bridge_api_lifecycle() {
        let temp_dir = std::env::temp_dir().join(format!("bitchat_test_{}", rand::random::<u32>()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let short_id = init(temp_dir.to_str().unwrap().to_string()).await.unwrap();
        assert_eq!(short_id.len(), 16);

        let my_id = my_short_id().await.unwrap();
        assert_eq!(short_id, my_id);

        let peer_pk = my_peer_id().await.unwrap();
        assert_eq!(peer_pk.len(), 64);

        // Connect neighbor
        let neighbor_hex = "0102030405060708";
        on_peer_connected(neighbor_hex.to_string()).await.unwrap();
        let peers = connected_peers().await.unwrap();
        assert_eq!(peers, vec![neighbor_hex.to_string()]);

        // Send broadcast
        let msg_id = send_broadcast("Hello Flutter Mesh!".to_string())
            .await
            .unwrap();
        assert_eq!(msg_id.len(), 16);

        // Check outbound frame
        let outbound = poll_outbound().await.unwrap().unwrap();
        assert_eq!(outbound.to, None);
        assert!(!outbound.bytes.is_empty());

        // Check stats
        let stats = get_stats().await.unwrap();
        assert!(stats.transmissions >= 1);

        // Disconnect
        on_peer_disconnected(neighbor_hex.to_string())
            .await
            .unwrap();
        assert!(connected_peers().await.unwrap().is_empty());

        // Test Gateway Bridge Lifecycle
        let ep_id = gateway_start(Some("disabled".to_string()))
            .await
            .expect("Start gateway");
        assert!(!ep_id.is_empty());

        let contact_uri = gateway_export_contact()
            .await
            .expect("Export gateway contact");
        assert!(contact_uri.starts_with("bitchat-gw://"));

        let gw_stats = gateway_get_stats().await.expect("Get gateway stats");
        assert!(gw_stats.contains("\"is_running\":true"));

        gateway_stop().await.expect("Stop gateway");

        let gw_stats_after = gateway_get_stats().await.expect("Get stats after stop");
        assert!(gw_stats_after.contains("\"is_running\":false"));

        // Test Phase 4 Delivery & Diagnostics API
        let logical_id = send_message(
            neighbor_hex.to_string(),
            "Hello delivery outbox!".to_string(),
            0,
        )
        .await
        .expect("Send logical message");
        assert_eq!(logical_id.len(), 32);

        let state = get_delivery_state(logical_id.clone())
            .await
            .expect("Query delivery state");
        assert!(state == "Queued" || state == "InFlight");

        let events_json = poll_delivery_events().await.expect("Poll delivery events");
        assert!(events_json.contains(&logical_id));

        let diag_json = get_diagnostics().await.expect("Get diagnostics");
        assert!(diag_json.contains("\"my_short_id\""));

        let pumped = pump_outbox().await.expect("Pump outbox");
        let _ = pumped;

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
