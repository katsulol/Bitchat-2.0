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
use crate::mesh::proximity::ProximityTracker;
use crate::mesh::relay::MeshNode;
use crate::mesh::resource::{
    BatteryBand, NetworkMetering, RelayResourceProfile, ResourceBudgetManager, ResourceSnapshot,
    ThermalState, TriState,
};
use crate::storage::volatile::{
    VolatileNegotiationFrame, VolatileSessionManager, VolatileSessionState, VOLATILE_MAGIC,
};
use crate::storage::{
    InjectedKeyProvider, KeyProvider, MemoryOutboxStore, MessageMode, OutboxLimits, OutboxManager,
    SendResult, SqliteOutboxStore,
};
use crate::transport::bridge::{BridgeHandle, BridgeTransport};
use crate::trust::{Capability, SignedVoucher, TrustQuotaConfig, TrustStore};

pub const CORE_BUILD_VERSION: &str = "v2.0.5-phase5";

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
    pub build_version: String,
    pub resource_profile: String,
    pub battery_band: String,
    pub charging_state: String,
    pub power_save_mode: String,
    pub pending_gateway_count: usize,
    pub approved_gateway_count: usize,
    pub active_vouchers_count: usize,
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
    trust_store: TrustStore,
    resource_mgr: ResourceBudgetManager,
    resource_snapshot: ResourceSnapshot,
    proximity_tracker: ProximityTracker,
    volatile_mgr: VolatileSessionManager,
    next_voucher_seq: u32,
}

static SERVICE: OnceLock<RwLock<Option<BitchatCoreService>>> = OnceLock::new();

fn get_service() -> &'static RwLock<Option<BitchatCoreService>> {
    SERVICE.get_or_init(|| RwLock::new(None))
}

fn hex_to_short_id(hex_str: &str) -> Result<ShortId, String> {
    ShortId::from_user_input(hex_str).ok_or_else(|| {
        format!(
            "Invalid ShortId '{}': expected 16 strict ASCII hex characters (non-zero)",
            hex_str
        )
    })
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
    let now_instant = std::time::Instant::now();

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
        trust_store: TrustStore::new(peer_id, TrustQuotaConfig::default()),
        resource_mgr: ResourceBudgetManager::new(
            RelayResourceProfile::Balanced,
            std::time::Duration::from_secs(10),
            now_instant,
        ),
        resource_snapshot: ResourceSnapshot::default_at(now_instant),
        proximity_tracker: ProximityTracker::default(),
        volatile_mgr: VolatileSessionManager::new(true),
        next_voucher_seq: 1,
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

    let (pending_gw, approved_gw) = if let Some(ref bridge) = svc.gateway_bridge {
        let cfg_arc = bridge.config();
        let cfg = cfg_arc.read().await;
        (cfg.list_pending().len(), cfg.approved_count())
    } else {
        (0, 0)
    };

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
        build_version: CORE_BUILD_VERSION.to_string(),
        resource_profile: format!("{:?}", svc.resource_mgr.active_profile()),
        battery_band: format!("{:?}", svc.resource_snapshot.battery_band),
        charging_state: format!("{:?}", svc.resource_snapshot.charging),
        power_save_mode: format!("{:?}", svc.resource_snapshot.power_save_mode),
        pending_gateway_count: pending_gw,
        approved_gateway_count: approved_gw,
        active_vouchers_count: svc.trust_store.active_voucher_count(),
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

    // Prune and zeroize expired volatile conversation messages
    let _ = svc.volatile_mgr.prune_expired(now_instant);
    svc.trust_store
        .prune_expired_vouchers(now_instant, Some(now_secs));

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
                        let has_ble = !svc.handle.connected_peers().is_empty();
                        let has_gateway = svc.gateway_bridge.is_some();
                        let has_route = has_ble || has_gateway;
                        if has_route {
                            let _ = svc.peer_mgr.initiate_handshake(&mut svc.node, target).await;
                        }
                        let delay_ms =
                            (1000u64 * (1u64 << record.handshake_attempts.min(4))).min(10000);
                        let _ = svc.delivery_coord.defer_until_session_or_route(
                            &logical_id,
                            has_route,
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

    // Reset backoff for any queued messages waiting for this peer only if identity is verified
    let verified_pid = svc.peer_mgr.get_session(&peer).and_then(|s| s.peer_id());
    if let Some(pid) = verified_pid {
        svc.proximity_tracker
            .bind_verified_peer(&peer.to_string(), pid, std::time::Instant::now());
    }
    svc.delivery_coord.reset_backoff_for_verified_peer(
        &peer,
        verified_pid.is_some(),
        std::time::Instant::now(),
    );

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
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.handle.on_peer_disconnected(peer);
    svc.proximity_tracker
        .on_link_disconnected(&peer.to_string());
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
            if svc.peer_mgr.is_session_ready(&msg.origin_sender) {
                let now_inst = std::time::Instant::now();
                svc.delivery_coord.reset_backoff_for_verified_peer(
                    &msg.origin_sender,
                    true,
                    now_inst,
                );
                if let Some(pid) = svc
                    .peer_mgr
                    .get_session(&msg.origin_sender)
                    .and_then(|s| s.peer_id())
                {
                    svc.proximity_tracker.bind_verified_peer(
                        &msg.origin_sender.to_string(),
                        pid,
                        now_inst,
                    );
                }
            }
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
                        // Check if this is an authenticated VolatileNegotiationFrame (BC2V)
                        if app_frame.body.starts_with(VOLATILE_MAGIC) {
                            if let Ok(vframe) = VolatileNegotiationFrame::decode(&app_frame.body) {
                                let sender_pid = app_frame.sender_peer_id;
                                if let Ok(Some(resp_frame)) =
                                    svc.volatile_mgr.handle_negotiation_frame(
                                        sender_pid,
                                        &vframe,
                                        &mut svc.outbox_mgr,
                                        &mut svc.delivery_coord,
                                    )
                                {
                                    let mut resp_logical_id = [0u8; 16];
                                    rand::RngCore::fill_bytes(
                                        &mut rand::rngs::OsRng,
                                        &mut resp_logical_id,
                                    );
                                    let resp_app = ApplicationFrame::new_message(
                                        resp_logical_id,
                                        svc.node.identity().peer_id(),
                                        sender_pid,
                                        resp_frame.encode().to_vec(),
                                        std::time::Duration::from_secs(60),
                                        now_secs,
                                    );
                                    if let Ok(padded_resp) =
                                        pad_inner_payload(&resp_app.encode(), true)
                                    {
                                        let _ = svc
                                            .peer_mgr
                                            .send_encrypted(
                                                &mut svc.node,
                                                decrypted.origin_sender,
                                                &padded_resp,
                                            )
                                            .await;
                                    }
                                }
                            }
                            return Ok(None);
                        }

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
                            // If conversation is in Active volatile mode, record in zeroizing memory store
                            let _ = svc.volatile_mgr.record_volatile_message(
                                app_frame.sender_peer_id,
                                app_frame.logical_id,
                                app_frame.sender_peer_id,
                                app_frame.body.clone(),
                                std::time::Instant::now(),
                            );
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

/// List incoming gateway peers currently in `IdentityVerifiedPendingConsent` as a JSON array.
pub async fn gateway_list_pending() -> Result<String, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;

    if let Some(bridge) = svc.gateway_bridge.as_ref() {
        let pending = bridge.config().read().await.list_pending();
        let items: Vec<serde_json::Value> = pending
            .into_iter()
            .map(|p| {
                serde_json::json!({
                    "endpoint_id": p.endpoint_id.to_string(),
                    "peer_id": PeerId(p.bitchat_pubkey).to_string(),
                })
            })
            .collect();
        serde_json::to_string(&items).map_err(|e| format!("Serialization error: {e}"))
    } else {
        Ok("[]".to_string())
    }
}

/// Explicitly approve a pending or recognized gateway endpoint ID (`IdentityVerifiedPendingConsent -> Approved`).
pub async fn gateway_approve_peer(endpoint_id_hex: String) -> Result<(), String> {
    let endpoint_id = endpoint_id_hex
        .trim()
        .parse::<iroh::EndpointId>()
        .map_err(|e| format!("Invalid endpoint ID: {e}"))?;

    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    let bridge = svc
        .gateway_bridge
        .as_ref()
        .ok_or("Gateway bridge is not running")?;

    let pending_pubkey = bridge
        .config()
        .read()
        .await
        .list_pending()
        .into_iter()
        .find(|p| p.endpoint_id == endpoint_id)
        .map(|p| PeerId(p.bitchat_pubkey));

    let approved = bridge.config().write().await.approve_gateway(endpoint_id);
    if !approved {
        return Err("Gateway endpoint is blocked locally".to_string());
    }

    if let Some(pid) = pending_pubkey {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut caps = std::collections::HashSet::new();
        caps.insert(Capability::UseGatewayForForwarding);
        caps.insert(Capability::PermitOutboundGateway);
        caps.insert(Capability::ReceiveDirectMessage);
        let _ = svc.trust_store.approve_peer(pid, caps.clone(), now_secs);
        let _ = bridge
            .trust_store()
            .write()
            .await
            .approve_peer(pid, caps, now_secs);
    }
    Ok(())
}

/// Revoke or permanently block a gateway endpoint ID, atomically withdrawing routes and closing sessions.
pub async fn gateway_revoke_or_block_peer(
    endpoint_id_hex: String,
    block: bool,
) -> Result<(), String> {
    let endpoint_id = endpoint_id_hex
        .trim()
        .parse::<iroh::EndpointId>()
        .map_err(|e| format!("Invalid endpoint ID: {e}"))?;

    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    let bridge = svc
        .gateway_bridge
        .as_ref()
        .ok_or("Gateway bridge is not running")?;
    bridge.revoke_or_block_peer(endpoint_id, block).await;
    Ok(())
}

/// Set local trust state for a 64-hex `PeerId`: `"approve"`, `"introducer"`, `"revoke"`, or `"block"`.
pub async fn trust_set_peer_state(peer_id_hex: String, action: String) -> Result<(), String> {
    let pid = PeerId::from_hex(&peer_id_hex)
        .ok_or_else(|| "Invalid PeerId: expected 64 strict hex characters".to_string())?;
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;

    match action.as_str() {
        "approve" => {
            let mut caps = std::collections::HashSet::new();
            caps.insert(Capability::ReceiveDirectMessage);
            caps.insert(Capability::UseGatewayForForwarding);
            caps.insert(Capability::PermitOutboundGateway);
            svc.trust_store
                .approve_peer(pid, caps.clone(), now_secs)
                .map_err(|e| format!("Approve failed: {e}"))?;
            if let Some(ref bridge) = svc.gateway_bridge {
                let _ = bridge
                    .trust_store()
                    .write()
                    .await
                    .approve_peer(pid, caps, now_secs);
            }
        }
        "introducer" => {
            let mut caps = std::collections::HashSet::new();
            caps.insert(Capability::ReceiveDirectMessage);
            caps.insert(Capability::UseGatewayForForwarding);
            caps.insert(Capability::PermitOutboundGateway);
            caps.insert(Capability::ActAsIntroducer);
            svc.trust_store
                .approve_peer(pid, caps.clone(), now_secs)
                .map_err(|e| format!("Approve introducer failed: {e}"))?;
            if let Some(ref bridge) = svc.gateway_bridge {
                let _ = bridge
                    .trust_store()
                    .write()
                    .await
                    .approve_peer(pid, caps, now_secs);
            }
        }
        "revoke" => {
            svc.trust_store.revoke_peer(pid, now_secs);
            if let Some(ref bridge) = svc.gateway_bridge {
                bridge
                    .trust_store()
                    .write()
                    .await
                    .revoke_peer(pid, now_secs);
            }
        }
        "block" => {
            svc.trust_store.block_peer(pid, now_secs);
            if let Some(ref bridge) = svc.gateway_bridge {
                bridge.trust_store().write().await.block_peer(pid, now_secs);
            }
        }
        other => return Err(format!("Unsupported trust action: {other}")),
    }

    Ok(())
}

/// Issue a signed one-hop Ed25519 capability voucher (`bitchat-voucher://<hex>`).
///
/// `capability_str`: `"direct_message"` or `"gateway_forward"`.
pub async fn trust_issue_voucher(
    subject_peer_id_hex: String,
    audience_peer_id_hex: String,
    capability_str: String,
    lifetime_secs: u64,
) -> Result<String, String> {
    let subject = PeerId::from_hex(&subject_peer_id_hex)
        .ok_or_else(|| "Invalid subject PeerId (expected 64 hex chars)".to_string())?;
    let audience = PeerId::from_hex(&audience_peer_id_hex)
        .ok_or_else(|| "Invalid audience PeerId (expected 64 hex chars)".to_string())?;

    let cap = match capability_str.as_str() {
        "gateway_forward" => Capability::UseGatewayForForwarding,
        _ => Capability::ReceiveDirectMessage,
    };

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let bounded_lifetime = lifetime_secs.clamp(60, crate::trust::MAX_VOUCHER_LIFETIME_SECS);

    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    let seq = svc.next_voucher_seq;
    svc.next_voucher_seq = svc.next_voucher_seq.saturating_add(1);

    let voucher = SignedVoucher::issue(
        svc.node.identity(),
        subject,
        audience,
        cap,
        now_secs,
        now_secs.saturating_add(bounded_lifetime),
        seq,
    )
    .map_err(|e| format!("Failed to sign voucher: {e}"))?;

    let wire = voucher
        .to_wire_bytes()
        .map_err(|e| format!("Failed to encode voucher: {e}"))?;
    Ok(format!("bitchat-voucher://{}", hex::encode(wire)))
}

/// Import and verify a signed one-hop capability voucher (`bitchat-voucher://<hex>`).
pub async fn trust_import_voucher(voucher_uri: String) -> Result<String, String> {
    let trimmed = voucher_uri.trim();
    let hex_part = trimmed
        .strip_prefix("bitchat-voucher://")
        .unwrap_or(trimmed);
    if hex_part.len() != crate::trust::SIGNED_VOUCHER_WIRE_LEN * 2 {
        return Err(format!(
            "Invalid voucher hex length: expected {}, got {}",
            crate::trust::SIGNED_VOUCHER_WIRE_LEN * 2,
            hex_part.len()
        ));
    }

    let mut raw = [0u8; crate::trust::SIGNED_VOUCHER_WIRE_LEN];
    for i in 0..crate::trust::SIGNED_VOUCHER_WIRE_LEN {
        raw[i] = u8::from_str_radix(&hex_part[i * 2..i * 2 + 2], 16)
            .map_err(|e| format!("Invalid voucher hex byte: {e}"))?;
    }

    let voucher =
        SignedVoucher::from_wire_bytes(&raw).map_err(|e| format!("Malformed voucher: {e}"))?;
    let now_instant = std::time::Instant::now();
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;

    svc.trust_store
        .import_voucher(&voucher, now_instant, Some(now_secs))
        .map_err(|e| format!("Voucher rejected: {e}"))?;

    if let Some(ref bridge) = svc.gateway_bridge {
        let _ = bridge.trust_store().write().await.import_voucher(
            &voucher,
            now_instant,
            Some(now_secs),
        );
    }

    let summary = serde_json::json!({
        "issuer": voucher.issuer.to_string(),
        "subject": voucher.subject.to_string(),
        "capability": format!("{:?}", voucher.capability),
        "expires_at_unix": voucher.expires_at_unix,
    });
    serde_json::to_string(&summary).map_err(|e| format!("Serialization error: {e}"))
}

/// Update local device power/resource state and recompute `RelayResourceProfile` with hysteresis.
///
/// - `battery_pct`: `0..=100`, or `< 0` if unknown
/// - `charging`: `1` = true, `0` = false, `< 0` = unknown
/// - `power_save`: `1` = true, `0` = false, `< 0` = unknown
/// - `thermal`: `0` = Nominal, `1` = Fair, `2` = Serious, `3` = Critical, `< 0` = Unknown
/// - `user_paused`: `true` if user explicitly paused third-party mesh relaying
pub async fn update_resource_state(
    battery_pct: i32,
    charging: i32,
    power_save: i32,
    thermal: i32,
    user_paused: bool,
) -> Result<String, String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;

    let now_inst = std::time::Instant::now();
    let battery_band = match battery_pct {
        0..=9 => BatteryBand::Critical,
        10..=24 => BatteryBand::Low,
        25..=70 => BatteryBand::Medium,
        71..=100 => BatteryBand::High,
        _ => BatteryBand::Unknown,
    };
    let charging_state = match charging {
        1 => TriState::True,
        0 => TriState::False,
        _ => TriState::Unknown,
    };
    let power_save_state = match power_save {
        1 => TriState::True,
        0 => TriState::False,
        _ => TriState::Unknown,
    };
    let thermal_state = match thermal {
        0 => ThermalState::Nominal,
        1 => ThermalState::Fair,
        2 => ThermalState::Serious,
        3 => ThermalState::Critical,
        _ => ThermalState::Unknown,
    };

    let snap = ResourceSnapshot {
        charging: charging_state,
        battery_band,
        power_save_mode: power_save_state,
        thermal_state,
        network_metering: NetworkMetering::Unknown,
        ble_ready: true,
        is_foreground: true,
        user_paused_relaying: user_paused,
        user_profile_override: None,
        timestamp: now_inst,
    };

    let profile = svc.resource_mgr.update_snapshot(&snap);
    svc.resource_snapshot = snap;
    Ok(format!("{profile:?}"))
}

/// Record a direct-link BLE RSSI sample (`rssi_dbm`) for `short_id_hex` and return its coarse proximity category.
pub async fn record_ble_rssi(short_id_hex: String, rssi_dbm: i8) -> Result<String, String> {
    let peer_short = hex_to_short_id(&short_id_hex)?;
    let link_key = peer_short.to_string();
    let now_inst = std::time::Instant::now();

    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;

    svc.proximity_tracker
        .record_direct_link_rssi(&link_key, rssi_dbm, 1, now_inst);

    let verified_pid = svc
        .peer_mgr
        .get_session(&peer_short)
        .and_then(|s| s.peer_id())
        .unwrap_or_else(|| {
            // Bind to link-derived synthetic PeerId once link identity is established on direct BLE
            let mut b = [0u8; 32];
            b[..8].copy_from_slice(&peer_short.0);
            PeerId(b)
        });
    svc.proximity_tracker
        .bind_verified_peer(&link_key, verified_pid, now_inst);

    let cat = svc
        .proximity_tracker
        .peer_proximity(&verified_pid, now_inst);
    Ok(format!("{cat:?}"))
}

/// Query coarse proximity category (`"Nearby"`, `"Further"`, `"Unknown"`) for a peer `ShortId`.
pub async fn get_peer_proximity(short_id_hex: String) -> Result<String, String> {
    let peer_short = hex_to_short_id(&short_id_hex)?;
    let now_inst = std::time::Instant::now();

    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;

    let verified_pid = svc
        .peer_mgr
        .get_session(&peer_short)
        .and_then(|s| s.peer_id())
        .unwrap_or_else(|| {
            let mut b = [0u8; 32];
            b[..8].copy_from_slice(&peer_short.0);
            PeerId(b)
        });

    let cat = svc
        .proximity_tracker
        .peer_proximity(&verified_pid, now_inst);
    Ok(format!("{cat:?}"))
}

/// Initiate or update an authenticated volatile conversation with `recipient_short_id`.
///
/// If `retention_secs == 0`, closes the volatile session and cancels pending volatile sends.
pub async fn set_volatile_session(
    recipient_short_id: String,
    retention_secs: u32,
) -> Result<String, String> {
    let target = hex_to_short_id(&recipient_short_id)?;
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;

    let recipient_peer_id = svc
        .peer_mgr
        .get_session(&target)
        .and_then(|s| s.peer_id())
        .unwrap_or_else(|| {
            let mut b = [0u8; 32];
            b[..8].copy_from_slice(&target.0);
            PeerId(b)
        });

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if retention_secs == 0 {
        let close_frame = match svc.volatile_mgr.session_state(&recipient_peer_id) {
            VolatileSessionState::PendingNegotiation { session_id, .. }
            | VolatileSessionState::Active { session_id, .. } => {
                Some(VolatileNegotiationFrame::new_close(session_id))
            }
            _ => None,
        };
        svc.volatile_mgr.close_conversation(
            &recipient_peer_id,
            &mut svc.outbox_mgr,
            &mut svc.delivery_coord,
        );
        if let Some(cf) = close_frame {
            if svc.peer_mgr.is_session_ready(&target) {
                let mut lid = [0u8; 16];
                rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut lid);
                let app_frame = ApplicationFrame::new_message(
                    lid,
                    svc.node.identity().peer_id(),
                    recipient_peer_id,
                    cf.encode().to_vec(),
                    std::time::Duration::from_secs(60),
                    now_secs,
                );
                if let Ok(padded) = pad_inner_payload(&app_frame.encode(), true) {
                    let _ = svc
                        .peer_mgr
                        .send_encrypted(&mut svc.node, target, &padded)
                        .await;
                }
            }
        }
        return Ok("NotRequested".to_string());
    }

    let offer = svc
        .volatile_mgr
        .initiate_volatile_session(recipient_peer_id, retention_secs);

    if svc.peer_mgr.is_session_ready(&target) {
        let mut lid = [0u8; 16];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut lid);
        let app_frame = ApplicationFrame::new_message(
            lid,
            svc.node.identity().peer_id(),
            recipient_peer_id,
            offer.encode().to_vec(),
            std::time::Duration::from_secs(60),
            now_secs,
        );
        if let Ok(padded) = pad_inner_payload(&app_frame.encode(), true) {
            let _ = svc
                .peer_mgr
                .send_encrypted(&mut svc.node, target, &padded)
                .await;
        }
    } else {
        let _ = svc.peer_mgr.initiate_handshake(&mut svc.node, target).await;
    }

    Ok("PendingNegotiation".to_string())
}

/// Query volatile conversation state for `recipient_short_id` as JSON (`{state, retention_secs}`).
pub async fn get_volatile_state(recipient_short_id: String) -> Result<String, String> {
    let target = hex_to_short_id(&recipient_short_id)?;
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;

    let recipient_peer_id = svc
        .peer_mgr
        .get_session(&target)
        .and_then(|s| s.peer_id())
        .unwrap_or_else(|| {
            let mut b = [0u8; 32];
            b[..8].copy_from_slice(&target.0);
            PeerId(b)
        });

    let (state_str, ret_secs) = match svc.volatile_mgr.session_state(&recipient_peer_id) {
        VolatileSessionState::NotRequested => ("NotRequested", 0u64),
        VolatileSessionState::PendingNegotiation {
            requested_retention,
            ..
        } => ("PendingNegotiation", requested_retention.as_secs()),
        VolatileSessionState::Active {
            effective_retention,
            ..
        } => ("Active", effective_retention.as_secs()),
        VolatileSessionState::UnsupportedByPeer => ("UnsupportedByPeer", 0u64),
    };

    let json = serde_json::json!({
        "state": state_str,
        "retention_secs": ret_secs,
    });
    serde_json::to_string(&json).map_err(|e| format!("Serialization error: {e}"))
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
        assert!(diag_json.contains("\"build_version\":\"v2.0.5-phase5\""));

        // Test Phase 5 Resource, Proximity, Voucher & Volatile APIs
        let prof = update_resource_state(15, 0, 1, 0, false)
            .await
            .expect("Update resource state");
        assert_eq!(prof, "Minimal");

        let _ = record_ble_rssi(neighbor_hex.to_string(), -58).await;
        let _ = record_ble_rssi(neighbor_hex.to_string(), -57).await;
        let prox = record_ble_rssi(neighbor_hex.to_string(), -56)
            .await
            .expect("Record RSSI");
        assert_eq!(prox, "Nearby");
        assert_eq!(
            get_peer_proximity(neighbor_hex.to_string()).await.unwrap(),
            "Nearby"
        );

        let vol_st = set_volatile_session(neighbor_hex.to_string(), 300)
            .await
            .expect("Set volatile session");
        assert_eq!(vol_st, "PendingNegotiation");
        let vol_json = get_volatile_state(neighbor_hex.to_string())
            .await
            .expect("Get volatile state");
        assert!(vol_json.contains("\"PendingNegotiation\""));

        let subject_pk = crate::identity::Identity::generate().peer_id().to_string();
        let audience_pk = crate::identity::Identity::generate().peer_id().to_string();
        let voucher_uri =
            trust_issue_voucher(subject_pk, audience_pk, "direct_message".to_string(), 600)
                .await
                .expect("Issue voucher");
        assert!(voucher_uri.starts_with("bitchat-voucher://"));

        let pumped = pump_outbox().await.expect("Pump outbox");
        let _ = pumped;

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
