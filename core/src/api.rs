use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

use serde::{Deserialize, Serialize};

use crate::envelope::{pad_inner_payload, unpad_bucket, MsgType};
use crate::gateway::GatewayBridge;
use crate::identity::{FileKeyStore, Identity, KeyStore, PeerId, ShortId};
use crate::mesh::delivery::{
    ApplicationFrame, ApplicationFrameKind, DedupStatus, DeliveryAction, DeliveryCoordinator,
    DeliveryState, EnqueueMessage, InboxDeduplicator, DEFAULT_ACK_TIMEOUT,
};
use crate::mesh::peer::PeerSessionManager;
use crate::mesh::policy::CoverageAwareFlood;
use crate::mesh::proximity::{ProximityCategory, ProximityTracker};
use crate::mesh::relay::{MeshNode, SendTarget};
use crate::mesh::resource::{
    BatteryBand, NetworkMetering, RelayResourceProfile, ResourceBudgetManager, ResourceSnapshot,
    ThermalState, TriState,
};
use crate::mesh::router::{
    GatewayCandidate, ObservedTransport, RouteCandidateMulti, RouteContextMulti, RouteReason,
    Router,
};
use crate::storage::volatile::{
    VolatileNegotiationFrame, VolatileSessionManager, VolatileSessionState, VOLATILE_MAGIC,
};
use crate::storage::{
    InjectedKeyProvider, KeyProvider, MemoryOutboxStore, MessageMode, OutboxLimits, OutboxManager,
    SendResult, SqliteOutboxStore,
};
use crate::transport::bridge::{BridgeHandle, BridgeTransport};
use crate::transport::Transport;
use crate::trust::{
    AuthOutcome, AuthReason, AuthorizationContext, Capability, GatewayApprovalStatus,
    InboundAuthEvent, PeerLinkIdentityState, PeerTrustState, PendingMessageRequest,
    PersistedGatewayApproval, SignedVoucher, SignedVoucherRevocation, SqliteTrustStore,
    TrustPolicy, TrustProvenance, TrustQuotaConfig, TrustStore, DEFAULT_MAX_PENDING_REQUESTS,
};

pub const CORE_BUILD_VERSION: &str = "v2.0.5-phase5";

/// Status transition event emitted to Flutter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryEventRecord {
    pub logical_id: String,
    pub state: String,
    pub recipient: String,
    pub timestamp: u64,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub transport: String,
    #[serde(default)]
    pub attempt_count: u32,
    #[serde(default)]
    pub expires_at: u64,
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
    #[serde(default)]
    pub transit_packets_forwarded: u64,
    #[serde(default)]
    pub transit_bytes_forwarded: u64,
    #[serde(default)]
    pub transit_packets_dropped_policy: u64,
    #[serde(default)]
    pub relay_contribution_mode: String,
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
    pub auth_status: String,
}

/// Outbound BLE radio frame emitted by Rust core to be written to peripheral/central GATT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeOutbound {
    pub to: Option<String>,
    pub bytes: Vec<u8>,
}

const MAX_AUTH_EVENT_QUEUE: usize = 128;

/// Core service managing the node runtime, outbox, trust, and gateway bridge.
pub struct BitchatCoreService {
    node: MeshNode<BridgeTransport>,
    handle: BridgeHandle,
    peer_mgr: PeerSessionManager,
    outbox_mgr: OutboxManager,
    delivery_coord: DeliveryCoordinator,
    dedup: InboxDeduplicator,
    delivery_events: Vec<DeliveryEventRecord>,
    gateway_bridge: Option<Arc<GatewayBridge>>,
    last_route_candidate: Option<String>,
    trust_store: Arc<RwLock<TrustStore>>,
    trust_db: Arc<SqliteTrustStore>,
    persisted_gateway_approvals: std::sync::RwLock<HashMap<String, PersistedGatewayApproval>>,
    link_identities: HashMap<String, PeerLinkIdentityState>,
    auth_events: VecDeque<InboundAuthEvent>,
    pending_message_requests: VecDeque<PendingMessageRequest>,
    resource_mgr: Arc<std::sync::Mutex<ResourceBudgetManager>>,
    resource_snapshot: ResourceSnapshot,
    relay_contribution_mode: String,
    proximity_tracker: ProximityTracker,
    volatile_mgr: VolatileSessionManager,
    known_peer_ids: HashMap<ShortId, PeerId>,
    ack_timeout: Duration,
    gateway_opted_in: bool,
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

fn parse_logical_id_hex(logical_id_hex: &str) -> Result<[u8; 16], String> {
    let clean = logical_id_hex.trim().trim_start_matches("0x");
    if clean.len() != 32 {
        return Err("Invalid logical_id: expected 32 hex chars".to_string());
    }
    let mut logical_id = [0u8; 16];
    for i in 0..16 {
        logical_id[i] = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16)
            .map_err(|e| format!("Invalid hex: {e}"))?;
    }
    Ok(logical_id)
}

impl BitchatCoreService {
    /// Construct a standalone `BitchatCoreService` instance rooted in `storage_path`,
    /// rehydrating any pending Normal-mode outbox records and verified trust state from encrypted SQLite.
    pub async fn new_with_key(
        storage_path: String,
        dek: Option<[u8; 32]>,
        key_version: u32,
    ) -> Result<Self, String> {
        let base_dir = PathBuf::from(&storage_path);
        let key_provider: Arc<dyn KeyProvider> = match dek {
            Some(k) => Arc::new(InjectedKeyProvider::new(k, key_version)),
            None => {
                let protected_exists = ["bitchat_outbox.db", "bitchat_trust.db", "identity.enc"]
                    .iter()
                    .any(|f| {
                        base_dir
                            .join(f)
                            .metadata()
                            .map(|m| m.len() > 0)
                            .unwrap_or(false)
                    });
                if protected_exists {
                    return Err(
                        "KEY_UNAVAILABLE: Protected storage exists on disk without platform DEK; failing closed"
                            .to_string(),
                    );
                }
                Arc::new(InjectedKeyProvider::random(key_version))
            }
        };
        Self::new_with_key_provider(storage_path, key_provider).await
    }

    /// Construct a standalone `BitchatCoreService` backed by an explicit `KeyProvider`.
    pub async fn new_with_key_provider(
        storage_path: String,
        key_provider: Arc<dyn KeyProvider>,
    ) -> Result<Self, String> {
        // Validate key availability before opening/creating encrypted databases.
        let _ = key_provider
            .get_or_create_dek()
            .map_err(|e| format!("KEY_UNAVAILABLE: Failed to unlock KeyProvider: {e}"))?;

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
        let mut node = MeshNode::new(identity, transport, Box::new(CoverageAwareFlood::default()));
        let now_instant = Instant::now();
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let resource_mgr = Arc::new(std::sync::Mutex::new(ResourceBudgetManager::new(
            RelayResourceProfile::Balanced,
            Duration::from_secs(10),
            now_instant,
        )));
        node.set_resource_budget(Arc::clone(&resource_mgr));

        // Open and hydrate encrypted TrustStore BEFORE creating gateway/outbox admission state (fail-closed)
        let trust_db_path = PathBuf::from(&storage_path).join("bitchat_trust.db");
        let trust_db = Arc::new(
            SqliteTrustStore::open(&trust_db_path, peer_id, Arc::clone(&key_provider))
                .map_err(|e| format!("TRUST_STORAGE_ERROR: {e}"))?,
        );
        let mut trust_store = TrustStore::new(peer_id, TrustQuotaConfig::default());
        let loaded_gw_approvals = trust_db
            .load_into(&mut trust_store, now_instant, Some(now_secs))
            .map_err(|e| format!("TRUST_STORAGE_ERROR: Failed to hydrate persisted trust: {e}"))?;
        let mut gw_map = HashMap::new();
        for rec in loaded_gw_approvals {
            gw_map.insert(rec.endpoint_id.clone(), rec);
        }

        let db_path = PathBuf::from(&storage_path).join("bitchat_outbox.db");
        let limits = OutboxLimits::default();
        let sqlite_store = SqliteOutboxStore::open(&db_path, key_provider, limits)
            .map_err(|e| format!("Failed to open SQLite store: {e:?}"))?;
        let memory_store = MemoryOutboxStore::new(limits);
        let mut outbox_mgr = OutboxManager::new(peer_id, sqlite_store, memory_store, limits);
        let mut delivery_coord = DeliveryCoordinator::new(peer_id);
        let dedup = InboxDeduplicator::new(10_000, Duration::from_secs(86400));
        let ack_timeout = DEFAULT_ACK_TIMEOUT;
        let mut delivery_events = Vec::new();

        if let Ok(pending) = outbox_mgr.get_pending(None) {
            for rec in pending {
                let recip_str = rec.recipient.to_string();
                let attempts = rec.attempt_count;
                let exp_unix = rec.expires_at;
                if let Some(DeliveryAction::Expired(lid)) =
                    delivery_coord.rehydrate_record(rec, now_instant, now_secs, ack_timeout)
                {
                    let _ = outbox_mgr.mark_expired(&lid);
                    delivery_events.push(DeliveryEventRecord {
                        logical_id: hex::encode(lid),
                        state: "Expired".to_string(),
                        recipient: recip_str,
                        timestamp: now_secs,
                        reason: "Expired".to_string(),
                        transport: "Unknown".to_string(),
                        attempt_count: attempts,
                        expires_at: exp_unix,
                    });
                }
            }
        }

        Ok(Self {
            node,
            handle,
            peer_mgr: PeerSessionManager::new(),
            outbox_mgr,
            delivery_coord,
            dedup,
            delivery_events,
            gateway_bridge: None,
            last_route_candidate: None,
            trust_store: Arc::new(RwLock::new(trust_store)),
            trust_db,
            persisted_gateway_approvals: std::sync::RwLock::new(gw_map),
            link_identities: HashMap::new(),
            auth_events: VecDeque::with_capacity(MAX_AUTH_EVENT_QUEUE),
            pending_message_requests: VecDeque::with_capacity(DEFAULT_MAX_PENDING_REQUESTS),
            resource_mgr,
            resource_snapshot: ResourceSnapshot::default_at(now_instant),
            relay_contribution_mode: "balanced".to_string(),
            proximity_tracker: ProximityTracker::default(),
            volatile_mgr: VolatileSessionManager::new(true),
            known_peer_ids: HashMap::new(),
            ack_timeout,
            gateway_opted_in: false,
            next_voucher_seq: 1,
        })
    }

    pub fn my_short_id(&self) -> ShortId {
        self.node.identity().short_id()
    }

    pub fn my_peer_id(&self) -> PeerId {
        self.node.identity().peer_id()
    }

    pub fn transport_handle(&self) -> BridgeHandle {
        self.handle.clone()
    }

    pub fn gateway_bridge_arc(&self) -> Option<Arc<GatewayBridge>> {
        self.gateway_bridge.as_ref().map(Arc::clone)
    }

    pub fn set_ack_timeout(&mut self, timeout: Duration) {
        self.ack_timeout = timeout.max(Duration::from_millis(20));
    }

    pub fn register_verified_peer_id(&mut self, peer_id: PeerId) {
        self.known_peer_ids.insert(peer_id.short_id(), peer_id);
    }

    pub fn trust_store_arc(&self) -> Arc<RwLock<TrustStore>> {
        Arc::clone(&self.trust_store)
    }

    pub fn on_unbound_link_connected(&mut self, link_id: String) {
        self.link_identities
            .entry(link_id.clone())
            .or_insert(PeerLinkIdentityState::UnboundLink { link_id });
    }

    pub fn record_link_rssi(&mut self, link_id: &str, rssi_dbm: i8) -> ProximityCategory {
        let now_inst = Instant::now();
        self.link_identities
            .entry(link_id.to_string())
            .or_insert_with(|| PeerLinkIdentityState::UnboundLink {
                link_id: link_id.to_string(),
            });
        self.proximity_tracker
            .record_direct_link_rssi(link_id, rssi_dbm, 1, now_inst);

        match self.link_identities.get(link_id) {
            Some(PeerLinkIdentityState::VerifiedPeer { peer_id, .. }) => {
                self.proximity_tracker.peer_proximity(peer_id, now_inst)
            }
            _ => ProximityCategory::Unknown,
        }
    }

    pub fn link_identity_state(&self, link_id: &str) -> Option<&PeerLinkIdentityState> {
        self.link_identities.get(link_id)
    }

    pub fn disallow_immediate_relay(&mut self, peer: ShortId) {
        self.node.disallow_immediate_relay(peer);
    }

    pub fn allow_immediate_relay(&mut self, peer: ShortId) {
        self.node.allow_immediate_relay(peer);
    }

    pub fn poll_auth_events(&mut self) -> Vec<InboundAuthEvent> {
        self.auth_events.drain(..).collect()
    }

    pub fn pending_message_requests(&self) -> Vec<PendingMessageRequest> {
        self.pending_message_requests.iter().cloned().collect()
    }

    fn push_auth_event(&mut self, ev: InboundAuthEvent) {
        if self.auth_events.len() >= MAX_AUTH_EVENT_QUEUE {
            self.auth_events.pop_front();
        }
        self.auth_events.push_back(ev);
    }

    fn push_pending_request(&mut self, req: PendingMessageRequest) {
        if self
            .pending_message_requests
            .iter()
            .any(|existing| existing.logical_id == req.logical_id)
        {
            return;
        }
        if self.pending_message_requests.len() >= DEFAULT_MAX_PENDING_REQUESTS {
            self.pending_message_requests.pop_front();
        }
        self.pending_message_requests.push_back(req);
    }

    pub fn clear_pending_requests_for(&mut self, peer: &PeerId) {
        let peer_hex = peer.to_string();
        self.pending_message_requests
            .retain(|req| req.sender_peer_id != peer_hex);
    }

    pub async fn set_gateway_opted_in(&mut self, opted_in: bool) {
        self.gateway_opted_in = opted_in;
        if let Some(ref bridge) = self.gateway_bridge {
            bridge.set_enabled(opted_in).await;
        }
    }

    /// Resolve a recipient string (strict 64-hex `PeerId` or verified 16-hex `ShortId`).
    /// Never synthesizes a fake zero-padded `PeerId` from an unverified `ShortId`.
    async fn resolve_verified_peer_id(
        &self,
        recipient_hex: &str,
    ) -> Result<Option<(PeerId, ShortId)>, String> {
        if let Some(pid) = PeerId::from_hex(recipient_hex) {
            return Ok(Some((pid, pid.short_id())));
        }
        let sid = hex_to_short_id(recipient_hex)?;
        if let Some(pid) = self
            .peer_mgr
            .verified_peer_id(&sid)
            .or_else(|| self.known_peer_ids.get(&sid).copied())
        {
            return Ok(Some((pid, sid)));
        }
        if let Some(ref bridge) = self.gateway_bridge {
            if let Some(pid) = bridge.verified_peer_id_for_short(&sid).await {
                return Ok(Some((pid, sid)));
            }
        }
        Ok(None)
    }

    pub async fn send_broadcast(&mut self, text: String) -> Result<String, String> {
        let msg_id = self
            .node
            .send_broadcast(text.into_bytes())
            .await
            .map_err(|e| format!("Send broadcast failed: {e}"))?;
        Ok(hex::encode(msg_id))
    }

    pub async fn send_to(
        &mut self,
        recipient_short_id: String,
        text: String,
    ) -> Result<String, String> {
        let target = hex_to_short_id(&recipient_short_id)?;
        if let Some(session) = self.peer_mgr.get_session(&target) {
            if session.is_handshake_finished() {
                if let Some(pid) = self.peer_mgr.verified_peer_id(&target) {
                    let now_secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let mut logical_id = [0u8; 16];
                    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut logical_id);
                    let frame = ApplicationFrame::new_message(
                        logical_id,
                        self.node.identity().peer_id(),
                        pid,
                        text.into_bytes(),
                        Duration::from_secs(900),
                        now_secs,
                    );
                    let padded = pad_inner_payload(&frame.encode(), true)
                        .map_err(|e| format!("Padding failed: {e}"))?;
                    let msg_id = self
                        .peer_mgr
                        .send_encrypted(&mut self.node, target, &padded)
                        .await
                        .map_err(|e| format!("E2E encrypted send failed: {e}"))?;
                    return Ok(hex::encode(msg_id));
                }
            } else if session.is_failed() {
                let _ = self
                    .peer_mgr
                    .initiate_handshake(&mut self.node, target)
                    .await;
            }
        } else {
            let _ = self
                .peer_mgr
                .initiate_handshake(&mut self.node, target)
                .await;
        }
        Err(
            "Securing direct E2E channel with peer... Handshake started. Please tap send again."
                .to_string(),
        )
    }

    pub async fn send_message(
        &mut self,
        recipient_hex: String,
        text: String,
        mode_u8: u8,
    ) -> Result<String, String> {
        self.send_message_with_deadline(recipient_hex, text, mode_u8, None)
            .await
    }

    pub async fn send_message_with_deadline(
        &mut self,
        recipient_hex: String,
        text: String,
        mode_u8: u8,
        deadline_secs: Option<u64>,
    ) -> Result<String, String> {
        let mode = match mode_u8 {
            1 => MessageMode::Privacy,
            _ => MessageMode::Normal,
        };

        let resolved = self.resolve_verified_peer_id(&recipient_hex).await?;
        let (recipient_peer_id, target) = match resolved {
            Some(pair) => pair,
            None => {
                let sid = hex_to_short_id(&recipient_hex)?;
                if self.node.transport().neighbors().contains(&sid)
                    && !self.peer_mgr.is_session_ready(&sid)
                {
                    let _ = self
                        .peer_mgr
                        .initiate_handshake_via(&mut self.node, sid, SendTarget::DirectBle)
                        .await;
                }
                return Err(
                    "Recipient full identity not yet verified; establishing secure session"
                        .to_string(),
                );
            }
        };

        self.known_peer_ids.insert(target, recipient_peer_id);

        if matches!(
            self.trust_store.read().await.peer_state(&recipient_peer_id),
            PeerTrustState::Blocked { .. }
        ) {
            return Err("Recipient is locally blocked".to_string());
        }

        let now_instant = Instant::now();
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let ttl_override = deadline_secs.map(Duration::from_secs);
        let send_res = self.outbox_mgr.send_to(
            recipient_peer_id,
            text.as_bytes().to_vec(),
            mode,
            ttl_override,
            now_secs,
        );

        let (logical_id, initial_state) = match send_res {
            SendResult::Accepted { logical_id, state } => (logical_id, state),
            SendResult::Rejected(e) => return Err(format!("Send rejected: {e:?}")),
        };

        let actual_ttl = ttl_override.unwrap_or_else(|| mode.default_ttl());
        let expires_at_unix = self
            .outbox_mgr
            .get(&logical_id)
            .ok()
            .flatten()
            .map(|r| r.expires_at)
            .unwrap_or_else(|| now_secs.saturating_add(actual_ttl.as_secs()));

        let logical_id_hex = hex::encode(logical_id);
        let recipient_label = recipient_peer_id.to_string();

        self.delivery_coord.enqueue_with_meta(
            EnqueueMessage {
                logical_id,
                recipient_peer_id,
                recipient_short_id: target,
                body: text.as_bytes().to_vec(),
                ttl: actual_ttl,
                max_attempts: mode.max_attempts(),
                is_privacy_mode: mode == MessageMode::Privacy,
            },
            mode,
            expires_at_unix,
            self.ack_timeout,
            now_instant,
        );

        self.delivery_events.push(DeliveryEventRecord {
            logical_id: logical_id_hex.clone(),
            state: format!("{initial_state:?}"),
            recipient: recipient_label,
            timestamp: now_secs,
            reason: "Queued".to_string(),
            transport: "Unknown".to_string(),
            attempt_count: 0,
            expires_at: expires_at_unix,
        });

        // Respect single active attempt per recipient: only dispatch immediately if no active InFlight
        if !self
            .delivery_coord
            .has_active_in_flight_for(&recipient_peer_id, now_instant)
        {
            self.dispatch_record(logical_id, now_instant, now_secs)
                .await;
        }

        Ok(logical_id_hex)
    }

    async fn dispatch_record(
        &mut self,
        logical_id: [u8; 16],
        now_instant: Instant,
        now_secs: u64,
    ) -> bool {
        let initial_rec = match self.delivery_coord.get_record(&logical_id).cloned() {
            Some(r) if !r.state.is_terminal() => r,
            _ => return false,
        };

        if now_instant >= initial_rec.expires_at {
            let _ = self.delivery_coord.check_timeouts(now_instant);
            return false;
        }

        // Re-check recipient trust state before admitting/resuming outbound work
        let recip_trust = self
            .trust_store
            .read()
            .await
            .peer_state(&initial_rec.recipient_peer_id);
        if matches!(
            recip_trust,
            PeerTrustState::Blocked { .. } | PeerTrustState::Revoked { .. }
        ) {
            let reason_label = if matches!(recip_trust, PeerTrustState::Blocked { .. }) {
                "RecipientBlocked"
            } else {
                "RecipientRevoked"
            };
            self.delivery_coord
                .fail(&logical_id, RouteReason::ExhaustedAttemptsQueue);
            let _ = self.outbox_mgr.mark_failed(&logical_id);
            self.delivery_events.push(DeliveryEventRecord {
                logical_id: hex::encode(logical_id),
                state: "Failed".to_string(),
                recipient: initial_rec.recipient_peer_id.to_string(),
                timestamp: now_secs,
                reason: reason_label.to_string(),
                transport: "Unknown".to_string(),
                attempt_count: initial_rec.attempts,
                expires_at: initial_rec.expires_at_unix,
            });
            return false;
        }

        let gw_candidates = if let Some(ref bridge) = self.gateway_bridge {
            bridge
                .authorized_candidates_for_peer(&initial_rec.recipient_peer_id)
                .await
                .into_iter()
                .map(|(endpoint_id, peer_id, path)| GatewayCandidate {
                    endpoint_id,
                    peer_id,
                    path,
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        let max_iters = gw_candidates.len().max(1) + 2;
        for _ in 0..max_iters {
            let rec = match self.delivery_coord.get_record(&logical_id).cloned() {
                Some(r) if !r.state.is_terminal() => r,
                _ => return false,
            };

            let ble_neighbors = self.node.transport().neighbors();
            let is_direct_ble = ble_neighbors.contains(&rec.recipient_short_id);
            let has_ble_neighbors = !ble_neighbors.is_empty();
            let has_session = self.peer_mgr.is_session_ready(&rec.recipient_short_id);
            let relay_disabled = self
                .gateway_bridge
                .as_ref()
                .map(|b| b.relay_disabled())
                .unwrap_or(true);

            let ctx = RouteContextMulti {
                target_peer_id: rec.recipient_peer_id,
                target_short_id: rec.recipient_short_id,
                mode: rec.mode,
                is_direct_ble_neighbor: is_direct_ble,
                has_ble_neighbors,
                has_authenticated_session: has_session,
                gateway_candidates: gw_candidates.clone(),
                attempted_gateways: rec.attempted_gateways.clone(),
                gateway_opted_in: self.gateway_opted_in && self.gateway_bridge.is_some(),
                privacy_block_relay: true,
                iroh_endpoint_relay_disabled: relay_disabled,
                attempt_count: rec.attempts,
                max_attempts: rec.max_attempts,
                ble_radio_ready: self.resource_snapshot.ble_ready,
            };

            let decision = Router::decide_multi(&ctx);
            self.last_route_candidate = Some(format!("{:?}", decision.candidate));

            let (send_target, gw_ep_opt, observed_transport) = match decision.candidate {
                RouteCandidateMulti::DirectBle(_) => {
                    (SendTarget::DirectBle, None, ObservedTransport::DirectBle)
                }
                RouteCandidateMulti::IrohGateway {
                    endpoint_id, path, ..
                } => {
                    let obs = match path {
                        crate::mesh::router::IrohPathObservation::Direct => {
                            ObservedTransport::IrohDirect
                        }
                        crate::mesh::router::IrohPathObservation::Relay
                        | crate::mesh::router::IrohPathObservation::Mixed => {
                            ObservedTransport::IrohRelay
                        }
                        crate::mesh::router::IrohPathObservation::Unknown => {
                            ObservedTransport::IrohRelay
                        }
                    };
                    (SendTarget::Gateway(endpoint_id), Some(endpoint_id), obs)
                }
                RouteCandidateMulti::MeshFlood { .. } => (
                    SendTarget::BleMeshFlood,
                    None,
                    ObservedTransport::BleMeshFlood,
                ),
                RouteCandidateMulti::QueueBackoff {
                    next_retry_after_secs,
                    round_exhausted,
                } => {
                    if round_exhausted {
                        self.delivery_coord.clear_attempted_gateways(&logical_id);
                    }
                    let backoff = if round_exhausted && self.ack_timeout < Duration::from_secs(1) {
                        self.ack_timeout
                    } else {
                        let jitter_ms = (rand::random::<u64>() % 350).min(349);
                        Duration::from_millis(
                            next_retry_after_secs
                                .saturating_mul(1000)
                                .saturating_add(jitter_ms),
                        )
                    };
                    self.delivery_coord.set_route_meta(
                        &logical_id,
                        decision.reason,
                        ObservedTransport::Unknown,
                    );
                    let _ = self.delivery_coord.defer_until_session_or_route(
                        &logical_id,
                        false,
                        backoff,
                        now_instant,
                    );
                    if self.delivery_coord.record_emitted_event(
                        &logical_id,
                        DeliveryState::Queued,
                        decision.reason,
                    ) {
                        self.delivery_events.push(DeliveryEventRecord {
                            logical_id: hex::encode(logical_id),
                            state: "Queued".to_string(),
                            recipient: rec.recipient_peer_id.to_string(),
                            timestamp: now_secs,
                            reason: format!("{:?}", decision.reason),
                            transport: "Unknown".to_string(),
                            attempt_count: rec.attempts,
                            expires_at: rec.expires_at_unix,
                        });
                    }
                    return false;
                }
            };

            if decision.needs_handshake {
                let hs_res = self
                    .peer_mgr
                    .initiate_handshake_via(&mut self.node, rec.recipient_short_id, send_target)
                    .await;
                if hs_res.is_err() {
                    if let Some(ep) = gw_ep_opt {
                        self.delivery_coord.record_gateway_attempt(&logical_id, ep);
                        continue;
                    }
                }
                if hs_res.is_ok() {
                    // Rotate alternate gateway on repeated handshake timeouts
                    if rec.handshake_attempts > 0 {
                        if let Some(ep) = gw_ep_opt {
                            self.delivery_coord.record_gateway_attempt(&logical_id, ep);
                        }
                    }
                }
                let shift = rec.handshake_attempts.min(4);
                let base_ms = (1000u64 << shift).min(10_000);
                let delay = if self.ack_timeout < Duration::from_secs(1) {
                    Duration::from_millis(base_ms.min(self.ack_timeout.as_millis().max(50) as u64))
                } else {
                    Duration::from_millis(base_ms)
                };
                self.delivery_coord.set_route_meta(
                    &logical_id,
                    decision.reason,
                    observed_transport,
                );
                let _ = self.delivery_coord.defer_until_session_or_route(
                    &logical_id,
                    hs_res.is_ok(),
                    delay,
                    now_instant,
                );
                if self.delivery_coord.record_emitted_event(
                    &logical_id,
                    DeliveryState::Queued,
                    decision.reason,
                ) {
                    self.delivery_events.push(DeliveryEventRecord {
                        logical_id: hex::encode(logical_id),
                        state: "Queued".to_string(),
                        recipient: rec.recipient_peer_id.to_string(),
                        timestamp: now_secs,
                        reason: format!("{:?}", decision.reason),
                        transport: format!("{observed_transport:?}"),
                        attempt_count: rec.attempts,
                        expires_at: rec.expires_at_unix,
                    });
                }
                return hs_res.is_ok();
            }

            if let Some(ep) = gw_ep_opt {
                self.delivery_coord.record_gateway_attempt(&logical_id, ep);
            }

            let remaining_ttl = rec.expires_at.saturating_duration_since(now_instant);
            let app_frame = ApplicationFrame::new_message(
                logical_id,
                self.node.identity().peer_id(),
                rec.recipient_peer_id,
                rec.body.clone(),
                remaining_ttl,
                now_secs,
            );
            let frame_bytes = app_frame.encode();
            let Ok(padded) = pad_inner_payload(&frame_bytes, true) else {
                return false;
            };

            match self
                .peer_mgr
                .send_encrypted_via(&mut self.node, rec.recipient_short_id, &padded, send_target)
                .await
            {
                Ok(_) => {
                    let wait = if self.ack_timeout >= Duration::from_secs(1) {
                        let exp_wait_ms = (4000u64 << rec.attempts.min(3)).min(15_000);
                        self.ack_timeout.max(Duration::from_millis(exp_wait_ms))
                    } else {
                        self.ack_timeout
                    };
                    self.delivery_coord.set_route_meta(
                        &logical_id,
                        decision.reason,
                        observed_transport,
                    );
                    let _ = self
                        .delivery_coord
                        .mark_in_flight(&logical_id, wait, now_instant);
                    let _ = self.outbox_mgr.mark_in_flight(&logical_id, now_secs);
                    let new_attempts = self
                        .delivery_coord
                        .get_record(&logical_id)
                        .map(|r| r.attempts)
                        .unwrap_or(rec.attempts.saturating_add(1));
                    let _ = self.delivery_coord.record_emitted_event(
                        &logical_id,
                        DeliveryState::InFlight,
                        decision.reason,
                    );
                    self.delivery_events.push(DeliveryEventRecord {
                        logical_id: hex::encode(logical_id),
                        state: "InFlight".to_string(),
                        recipient: rec.recipient_peer_id.to_string(),
                        timestamp: now_secs,
                        reason: format!("{:?}", decision.reason),
                        transport: format!("{observed_transport:?}"),
                        attempt_count: new_attempts,
                        expires_at: rec.expires_at_unix,
                    });
                    return true;
                }
                Err(_) => {
                    if gw_ep_opt.is_some() {
                        // Immediate fallback to alternate gateway within same dispatch pass
                        continue;
                    }
                    let _ = self.delivery_coord.defer_until_session_or_route(
                        &logical_id,
                        false,
                        Duration::from_secs(2),
                        now_instant,
                    );
                    return false;
                }
            }
        }

        false
    }

    pub async fn get_delivery_state(&self, logical_id_hex: String) -> Result<String, String> {
        let logical_id = parse_logical_id_hex(&logical_id_hex)?;

        if let Some(st) = self.delivery_coord.get_state(&logical_id) {
            return Ok(format!("{st:?}"));
        }
        if let Ok(Some(rec)) = self.outbox_mgr.get(&logical_id) {
            return Ok(format!("{:?}", rec.state));
        }
        Err("Logical message not found".to_string())
    }

    pub async fn cancel_message(&mut self, logical_id_hex: String) -> Result<bool, String> {
        let logical_id = parse_logical_id_hex(&logical_id_hex)?;
        let transitioned = self.delivery_coord.cancel(&logical_id);
        let _ = self.outbox_mgr.mark_cancelled(&logical_id);
        if transitioned {
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let meta = self.delivery_coord.get_record(&logical_id);
            self.delivery_events.push(DeliveryEventRecord {
                logical_id: hex::encode(logical_id),
                state: "Cancelled".to_string(),
                recipient: meta
                    .map(|r| r.recipient_peer_id.to_string())
                    .unwrap_or_default(),
                timestamp: now_secs,
                reason: "Cancelled".to_string(),
                transport: meta
                    .map(|r| format!("{:?}", r.last_transport))
                    .unwrap_or_else(|| "Unknown".to_string()),
                attempt_count: meta.map(|r| r.attempts).unwrap_or(0),
                expires_at: meta.map(|r| r.expires_at_unix).unwrap_or(0),
            });
        }
        Ok(transitioned)
    }

    pub fn poll_delivery_events_vec(&mut self) -> Vec<DeliveryEventRecord> {
        std::mem::take(&mut self.delivery_events)
    }

    pub fn poll_delivery_events_json(&mut self) -> Result<String, String> {
        let events = std::mem::take(&mut self.delivery_events);
        serde_json::to_string(&events).map_err(|e| format!("Serialization error: {e}"))
    }

    pub async fn get_diagnostics_snapshot(&self) -> DiagnosticsSnapshot {
        let (pending_gw, approved_gw) = if let Some(ref bridge) = self.gateway_bridge {
            let cfg_arc = bridge.config();
            let cfg = cfg_arc.read().await;
            (cfg.list_pending().len(), cfg.approved_count())
        } else {
            (0, 0)
        };
        let active_vouchers = self.trust_store.read().await.active_voucher_count();
        let relay_disabled = self
            .gateway_bridge
            .as_ref()
            .map(|b| b.relay_disabled())
            .unwrap_or(true);
        let node_stats = self.node.stats();
        let active_profile = if let Ok(mgr) = self.resource_mgr.lock() {
            mgr.active_profile()
        } else {
            RelayResourceProfile::Balanced
        };

        DiagnosticsSnapshot {
            my_short_id: self.node.identity().short_id().to_string(),
            my_peer_id: self.node.identity().peer_id().to_string(),
            connected_peers_count: self.handle.connected_peers().len(),
            outbox_queued_count: self.delivery_coord.queued_count(),
            outbox_inflight_count: self.delivery_coord.in_flight_count(),
            deliveries_count: node_stats.deliveries,
            gateway_active: self.gateway_bridge.is_some(),
            gateway_relay_disabled: relay_disabled,
            last_route_candidate: self.last_route_candidate.clone(),
            build_version: CORE_BUILD_VERSION.to_string(),
            resource_profile: format!("{active_profile:?}"),
            battery_band: format!("{:?}", self.resource_snapshot.battery_band),
            charging_state: format!("{:?}", self.resource_snapshot.charging),
            power_save_mode: format!("{:?}", self.resource_snapshot.power_save_mode),
            pending_gateway_count: pending_gw,
            approved_gateway_count: approved_gw,
            active_vouchers_count: active_vouchers,
            transit_packets_forwarded: node_stats.transit_packets_forwarded,
            transit_bytes_forwarded: node_stats.transit_bytes_forwarded,
            transit_packets_dropped_policy: node_stats.transit_packets_dropped_policy,
            relay_contribution_mode: self.relay_contribution_mode.clone(),
        }
    }

    pub async fn pump_outbox(&mut self) -> Result<usize, String> {
        let now_instant = Instant::now();
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let _ = self.volatile_mgr.prune_expired(now_instant);
        self.trust_store
            .write()
            .await
            .prune_expired_vouchers(now_instant, Some(now_secs));

        let actions = self.delivery_coord.check_timeouts(now_instant);
        let mut handled = 0;

        for action in actions {
            match action {
                DeliveryAction::ReadyToRetry(logical_id) => {
                    if self
                        .dispatch_record(logical_id, now_instant, now_secs)
                        .await
                    {
                        handled += 1;
                    }
                }
                DeliveryAction::Expired(logical_id) => {
                    let _ = self.outbox_mgr.mark_expired(&logical_id);
                    let meta = self.delivery_coord.get_record(&logical_id).cloned();
                    self.delivery_events.push(DeliveryEventRecord {
                        logical_id: hex::encode(logical_id),
                        state: "Expired".to_string(),
                        recipient: meta
                            .as_ref()
                            .map(|r| r.recipient_peer_id.to_string())
                            .unwrap_or_default(),
                        timestamp: now_secs,
                        reason: "Expired".to_string(),
                        transport: meta
                            .as_ref()
                            .map(|r| format!("{:?}", r.last_transport))
                            .unwrap_or_else(|| "Unknown".to_string()),
                        attempt_count: meta.as_ref().map(|r| r.attempts).unwrap_or(0),
                        expires_at: meta.as_ref().map(|r| r.expires_at_unix).unwrap_or(0),
                    });
                    handled += 1;
                }
                DeliveryAction::Failed(logical_id) => {
                    let _ = self.outbox_mgr.mark_failed(&logical_id);
                    let meta = self.delivery_coord.get_record(&logical_id).cloned();
                    self.delivery_events.push(DeliveryEventRecord {
                        logical_id: hex::encode(logical_id),
                        state: "Failed".to_string(),
                        recipient: meta
                            .as_ref()
                            .map(|r| r.recipient_peer_id.to_string())
                            .unwrap_or_default(),
                        timestamp: now_secs,
                        reason: meta
                            .as_ref()
                            .map(|r| format!("{:?}", r.last_reason))
                            .unwrap_or_else(|| "ExhaustedAttemptsQueue".to_string()),
                        transport: meta
                            .as_ref()
                            .map(|r| format!("{:?}", r.last_transport))
                            .unwrap_or_else(|| "Unknown".to_string()),
                        attempt_count: meta.as_ref().map(|r| r.attempts).unwrap_or(0),
                        expires_at: meta.as_ref().map(|r| r.expires_at_unix).unwrap_or(0),
                    });
                    handled += 1;
                }
            }
        }

        Ok(handled)
    }

    pub async fn initiate_handshake(&mut self, recipient_short_id: String) -> Result<(), String> {
        let target = hex_to_short_id(&recipient_short_id)?;
        if !self.peer_mgr.is_session_ready(&target) {
            self.peer_mgr
                .initiate_handshake(&mut self.node, target)
                .await
                .map_err(|e| format!("Initiate handshake failed: {e}"))?;
        }
        Ok(())
    }

    pub fn is_session_ready(&self, recipient_short_id: &str) -> Result<bool, String> {
        let target = hex_to_short_id(recipient_short_id)?;
        Ok(self.peer_mgr.is_session_ready(&target))
    }

    pub fn get_stats(&self) -> BridgeStats {
        let snap = self.node.stats();
        BridgeStats {
            transmissions: snap.transmissions,
            duplicates_dropped: snap.duplicates_dropped,
            relays_suppressed: snap.relays_suppressed,
            deliveries: snap.deliveries,
        }
    }
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
        return Ok(svc.my_short_id().to_string());
    }
    let service = BitchatCoreService::new_with_key(storage_path, dek, key_version).await?;
    let short_hex = service.my_short_id().to_string();
    *lock = Some(service);
    Ok(short_hex)
}

/// Query our 32-byte Ed25519 PeerId public key hex string.
pub async fn my_peer_id() -> Result<String, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    Ok(svc.my_peer_id().to_string())
}

/// Query our 8-byte ShortId hex string.
pub async fn my_short_id() -> Result<String, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    Ok(svc.my_short_id().to_string())
}

/// Broadcast a public plaintext chat message to all peers in the mesh.
pub async fn send_broadcast(text: String) -> Result<String, String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.send_broadcast(text).await
}

/// Send an end-to-end encrypted or addressed direct chat message to `recipient_short_id`.
pub async fn send_to(recipient_short_id: String, text: String) -> Result<String, String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.send_to(recipient_short_id, text).await
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
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.send_message(recipient_short_id, text, mode_u8).await
}

/// Send an end-to-end encrypted direct chat message with an explicit optional deadline preset (seconds).
pub async fn send_message_with_deadline(
    recipient_short_id: String,
    text: String,
    mode_u8: u8,
    deadline_secs: Option<u64>,
) -> Result<String, String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.send_message_with_deadline(recipient_short_id, text, mode_u8, deadline_secs)
        .await
}

/// Cancel a non-terminal outbound logical message by its 32-hex ID.
pub async fn cancel_message(logical_id_hex: String) -> Result<bool, String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.cancel_message(logical_id_hex).await
}

/// Query current delivery state of a logical message.
pub async fn get_delivery_state(logical_id_hex: String) -> Result<String, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    svc.get_delivery_state(logical_id_hex).await
}

/// Drain queued delivery transition events emitted by the delivery subsystem.
pub async fn poll_delivery_events() -> Result<String, String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.poll_delivery_events_json()
}

/// Retrieve comprehensive routing diagnostics and quota snapshot for mobile UI.
pub async fn get_diagnostics() -> Result<String, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    let diag = svc.get_diagnostics_snapshot().await;
    serde_json::to_string(&diag).map_err(|e| format!("Serialization error: {e}"))
}

/// Step the outbox pump, processing timeouts, retries, and expiration.
pub async fn pump_outbox() -> Result<usize, String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.pump_outbox().await
}

/// Explicitly initiate a mutual Noise XX handshake with a peer across mesh or gateway.
pub async fn initiate_handshake(recipient_short_id: String) -> Result<(), String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.initiate_handshake(recipient_short_id).await
}

/// Check if a completed Noise XX E2E session exists with `recipient_short_id`.
pub async fn is_session_ready(recipient_short_id: String) -> Result<bool, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    svc.is_session_ready(&recipient_short_id)
}

/// Get current routing metrics snapshot.
pub async fn get_stats() -> Result<BridgeStats, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    Ok(svc.get_stats())
}

impl BitchatCoreService {
    fn local_verified_ble_peers(&self) -> Vec<(ShortId, Option<PeerId>)> {
        self.handle
            .connected_peers()
            .into_iter()
            .map(|sid| {
                let pid = self
                    .peer_mgr
                    .verified_peer_id(&sid)
                    .or_else(|| self.known_peer_ids.get(&sid).copied());
                (sid, pid)
            })
            .collect()
    }

    fn spawn_announce_local_peers(&self) {
        if let Some(ref bridge) = self.gateway_bridge {
            let peers = self.local_verified_ble_peers();
            let bridge = Arc::clone(bridge);
            tokio::spawn(async move {
                let _ = bridge.announce_local_verified_peers(&peers).await;
            });
        }
    }

    pub async fn on_peer_connected(&mut self, short_id: String) -> Result<(), String> {
        let peer = hex_to_short_id(&short_id)?;
        self.handle.on_peer_connected(peer);

        let verified_pid = self
            .peer_mgr
            .verified_peer_id(&peer)
            .or_else(|| self.known_peer_ids.get(&peer).copied());
        let link_key = peer.to_string();
        if let Some(pid) = verified_pid {
            self.link_identities.insert(
                link_key.clone(),
                PeerLinkIdentityState::VerifiedPeer {
                    link_id: link_key.clone(),
                    short_id: peer,
                    peer_id: pid,
                },
            );
            self.proximity_tracker
                .bind_verified_peer(&link_key, pid, Instant::now());
        } else {
            self.link_identities.insert(
                link_key,
                PeerLinkIdentityState::ClaimedShortId {
                    link_id: peer.to_string(),
                    short_id: peer,
                },
            );
        }
        self.delivery_coord.reset_backoff_for_verified_peer(
            &peer,
            verified_pid.is_some(),
            Instant::now(),
        );

        let my_short = self.node.identity().short_id();
        if my_short < peer && !self.peer_mgr.is_session_ready(&peer) {
            let _ = self
                .peer_mgr
                .initiate_handshake_via(&mut self.node, peer, SendTarget::DirectBle)
                .await;
        }

        self.spawn_announce_local_peers();
        Ok(())
    }

    pub async fn on_peer_disconnected(&mut self, short_id: String) -> Result<(), String> {
        let peer = hex_to_short_id(&short_id)?;
        let link_key = peer.to_string();
        self.handle.on_peer_disconnected(peer);
        self.link_identities.remove(&link_key);
        self.proximity_tracker.on_link_disconnected(&link_key);
        self.spawn_announce_local_peers();
        Ok(())
    }

    pub fn on_frame_received(&self, short_id: String, bytes: Vec<u8>) -> Result<(), String> {
        let peer = hex_to_short_id(&short_id)?;
        self.handle
            .on_frame_received(peer, bytes)
            .map_err(|e| format!("Failed to deliver frame: {e}"))
    }

    pub fn poll_outbound(&self) -> Option<BridgeOutbound> {
        self.node
            .transport()
            .try_recv_outbound()
            .map(|f| BridgeOutbound {
                to: f.to.map(|s| s.to_string()),
                bytes: f.bytes,
            })
    }

    pub async fn poll_inbound(&mut self) -> Result<Option<BridgeMessage>, String> {
        let mut steps = 0;
        while steps < 10 {
            match self.node.poll_step(Instant::now()).await {
                Ok(true) => steps += 1,
                Ok(false) => break,
                Err(e) => {
                    tracing::warn!("poll_step error: {e}");
                    break;
                }
            }
        }

        if let Some(msg) = self.node.recv_inbound() {
            let now_inst = Instant::now();
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            if msg.msg_type == MsgType::Handshake {
                let _ = self.peer_mgr.handle_handshake(&mut self.node, &msg).await;
                if self.peer_mgr.is_session_ready(&msg.origin_sender) {
                    self.delivery_coord.reset_backoff_for_verified_peer(
                        &msg.origin_sender,
                        true,
                        now_inst,
                    );
                    self.delivery_coord
                        .clear_attempted_gateways_for_peer(&msg.origin_sender);
                    if let Some(pid) = self.peer_mgr.verified_peer_id(&msg.origin_sender) {
                        self.known_peer_ids.insert(msg.origin_sender, pid);
                        let _ = self.trust_store.write().await.record_identity_verified(pid);

                        // Bind direct-link RSSI and link identity ONLY on single-hop direct link
                        let is_direct_single_hop = msg.hop_count == 0
                            && msg.immediate_relay == Some(msg.origin_sender)
                            && self.handle.connected_peers().contains(&msg.origin_sender);
                        if is_direct_single_hop {
                            let link_key = msg.origin_sender.to_string();
                            self.link_identities.insert(
                                link_key.clone(),
                                PeerLinkIdentityState::VerifiedPeer {
                                    link_id: link_key.clone(),
                                    short_id: msg.origin_sender,
                                    peer_id: pid,
                                },
                            );
                            self.proximity_tracker
                                .bind_verified_peer(&link_key, pid, now_inst);
                        }
                        self.spawn_announce_local_peers();
                    }
                }
                return Ok(None);
            }

            if !msg.recipient.is_broadcast() {
                let decrypted = match self.peer_mgr.decrypt_inbound(&msg) {
                    Ok(d) => d,
                    Err(_) => {
                        self.push_auth_event(InboundAuthEvent {
                            peer_id: String::new(),
                            short_id: msg.origin_sender.to_string(),
                            logical_id: None,
                            outcome: AuthOutcome::Deny,
                            reason: AuthReason::CapabilityNotGranted,
                            capability: "ReceiveDirectMessage".to_string(),
                            timestamp: now_secs,
                        });
                        return Ok(None);
                    }
                };

                let auth_sender = decrypted.remote_peer_id;
                self.known_peer_ids
                    .insert(decrypted.origin_sender, auth_sender);

                let Ok(unpadded) = unpad_bucket(&decrypted.plaintext) else {
                    self.push_auth_event(InboundAuthEvent {
                        peer_id: auth_sender.to_string(),
                        short_id: decrypted.origin_sender.to_string(),
                        logical_id: None,
                        outcome: AuthOutcome::Deny,
                        reason: AuthReason::CapabilityNotGranted,
                        capability: "ReceiveDirectMessage".to_string(),
                        timestamp: now_secs,
                    });
                    return Ok(None);
                };

                let Ok(app_frame) = ApplicationFrame::decode(&unpadded) else {
                    self.push_auth_event(InboundAuthEvent {
                        peer_id: auth_sender.to_string(),
                        short_id: decrypted.origin_sender.to_string(),
                        logical_id: None,
                        outcome: AuthOutcome::Deny,
                        reason: AuthReason::CapabilityNotGranted,
                        capability: "ReceiveDirectMessage".to_string(),
                        timestamp: now_secs,
                    });
                    return Ok(None);
                };

                match app_frame.kind {
                    ApplicationFrameKind::Message => {
                        if app_frame.sender_peer_id != auth_sender
                            || app_frame.recipient_peer_id != self.node.identity().peer_id()
                        {
                            self.push_auth_event(InboundAuthEvent {
                                peer_id: auth_sender.to_string(),
                                short_id: decrypted.origin_sender.to_string(),
                                logical_id: Some(hex::encode(app_frame.logical_id)),
                                outcome: AuthOutcome::Deny,
                                reason: AuthReason::CapabilityNotGranted,
                                capability: "ReceiveDirectMessage".to_string(),
                                timestamp: now_secs,
                            });
                            return Ok(None);
                        }

                        let auth_ctx = AuthorizationContext {
                            now_monotonic: now_inst,
                            now_unix: Some(now_secs),
                            gateway_opted_in: self.gateway_opted_in,
                            allow_vouched_gateway_forwarding: false,
                        };
                        let trust_dec = {
                            let mut ts = self.trust_store.write().await;
                            TrustPolicy::authorize(
                                &mut ts,
                                &auth_sender,
                                Capability::ReceiveDirectMessage,
                                &auth_ctx,
                            )
                        };
                        if trust_dec.outcome == AuthOutcome::Deny {
                            self.push_auth_event(InboundAuthEvent {
                                peer_id: auth_sender.to_string(),
                                short_id: decrypted.origin_sender.to_string(),
                                logical_id: Some(hex::encode(app_frame.logical_id)),
                                outcome: AuthOutcome::Deny,
                                reason: trust_dec.reason,
                                capability: "ReceiveDirectMessage".to_string(),
                                timestamp: now_secs,
                            });
                            return Ok(None);
                        }

                        if app_frame.body.starts_with(VOLATILE_MAGIC) {
                            if let Ok(vframe) = VolatileNegotiationFrame::decode(&app_frame.body) {
                                if let Ok(Some(resp_frame)) =
                                    self.volatile_mgr.handle_negotiation_frame(
                                        auth_sender,
                                        &vframe,
                                        &mut self.outbox_mgr,
                                        &mut self.delivery_coord,
                                    )
                                {
                                    let mut resp_logical_id = [0u8; 16];
                                    rand::RngCore::fill_bytes(
                                        &mut rand::rngs::OsRng,
                                        &mut resp_logical_id,
                                    );
                                    let resp_app = ApplicationFrame::new_message(
                                        resp_logical_id,
                                        self.node.identity().peer_id(),
                                        auth_sender,
                                        resp_frame.encode().to_vec(),
                                        Duration::from_secs(60),
                                        now_secs,
                                    );
                                    if let Ok(padded_resp) =
                                        pad_inner_payload(&resp_app.encode(), true)
                                    {
                                        let _ = self
                                            .peer_mgr
                                            .send_encrypted(
                                                &mut self.node,
                                                decrypted.origin_sender,
                                                &padded_resp,
                                            )
                                            .await;
                                    }
                                }
                            }
                            return Ok(None);
                        }

                        let auth_status = if trust_dec.outcome == AuthOutcome::Pending {
                            let reg_ok = {
                                let mut ts = self.trust_store.write().await;
                                ts.record_identity_verified(auth_sender).is_ok()
                            };
                            if !reg_ok {
                                self.push_auth_event(InboundAuthEvent {
                                    peer_id: auth_sender.to_string(),
                                    short_id: decrypted.origin_sender.to_string(),
                                    logical_id: Some(hex::encode(app_frame.logical_id)),
                                    outcome: AuthOutcome::Deny,
                                    reason: AuthReason::MessageRequestQuotaExceeded,
                                    capability: "ReceiveDirectMessage".to_string(),
                                    timestamp: now_secs,
                                });
                                return Ok(None);
                            }
                            let is_vol = !matches!(
                                self.volatile_mgr.session_state(&auth_sender),
                                VolatileSessionState::NotRequested
                            );
                            self.push_pending_request(PendingMessageRequest {
                                sender_peer_id: auth_sender.to_string(),
                                sender_short_id: decrypted.origin_sender.to_string(),
                                logical_id: hex::encode(app_frame.logical_id),
                                reason: trust_dec.reason,
                                received_at_unix: now_secs,
                                is_volatile: is_vol,
                            });
                            self.push_auth_event(InboundAuthEvent {
                                peer_id: auth_sender.to_string(),
                                short_id: decrypted.origin_sender.to_string(),
                                logical_id: Some(hex::encode(app_frame.logical_id)),
                                outcome: AuthOutcome::Pending,
                                reason: trust_dec.reason,
                                capability: "ReceiveDirectMessage".to_string(),
                                timestamp: now_secs,
                            });
                            "PendingRequest".to_string()
                        } else {
                            self.push_auth_event(InboundAuthEvent {
                                peer_id: auth_sender.to_string(),
                                short_id: decrypted.origin_sender.to_string(),
                                logical_id: Some(hex::encode(app_frame.logical_id)),
                                outcome: AuthOutcome::Allow,
                                reason: trust_dec.reason,
                                capability: "ReceiveDirectMessage".to_string(),
                                timestamp: now_secs,
                            });
                            "Allowed".to_string()
                        };

                        let dedup_status = self.dedup.check_or_insert(
                            &auth_sender,
                            &app_frame.logical_id,
                            now_inst,
                        );

                        let ack = ApplicationFrame::new_ack(
                            app_frame.logical_id,
                            self.node.identity().peer_id(),
                            auth_sender,
                            now_secs,
                        );
                        if let Ok(padded_ack) = pad_inner_payload(&ack.encode(), true) {
                            let _ = self
                                .peer_mgr
                                .send_encrypted(
                                    &mut self.node,
                                    decrypted.origin_sender,
                                    &padded_ack,
                                )
                                .await;
                        }

                        if dedup_status == DedupStatus::New {
                            let _ = self.volatile_mgr.record_volatile_message(
                                auth_sender,
                                app_frame.logical_id,
                                auth_sender,
                                app_frame.body.clone(),
                                now_inst,
                            );
                            let text = String::from_utf8_lossy(&app_frame.body).to_string();
                            return Ok(Some(BridgeMessage {
                                msg_id: hex::encode(app_frame.logical_id),
                                sender: decrypted.origin_sender.to_string(),
                                recipient: Some(self.node.short_id().to_string()),
                                text,
                                hop_count: decrypted.hop_count,
                                is_broadcast: false,
                                auth_status,
                            }));
                        }
                        return Ok(None);
                    }
                    ApplicationFrameKind::Ack => {
                        if let Ok(true) = self.delivery_coord.handle_ack(&app_frame, &auth_sender) {
                            let _ = self.outbox_mgr.mark_delivered(&app_frame.logical_id);
                            let meta = self.delivery_coord.get_record(&app_frame.logical_id);
                            let transport = meta
                                .map(|r| format!("{:?}", r.last_transport))
                                .unwrap_or_else(|| "Unknown".to_string());
                            let attempt_count = meta.map(|r| r.attempts).unwrap_or(0);
                            let expires_at = meta.map(|r| r.expires_at_unix).unwrap_or(0);
                            self.delivery_events.push(DeliveryEventRecord {
                                logical_id: hex::encode(app_frame.logical_id),
                                state: "Delivered".to_string(),
                                recipient: auth_sender.to_string(),
                                timestamp: now_secs,
                                reason: "Delivered".to_string(),
                                transport,
                                attempt_count,
                                expires_at,
                            });
                        }
                        return Ok(None);
                    }
                }
            }

            if msg.msg_type == MsgType::Chat && msg.recipient.is_broadcast() {
                let text = String::from_utf8_lossy(&msg.payload).to_string();
                return Ok(Some(BridgeMessage {
                    msg_id: hex::encode(msg.msg_id),
                    sender: msg.origin_sender.to_string(),
                    recipient: None,
                    text,
                    hop_count: msg.hop_count,
                    is_broadcast: true,
                    auth_status: "Broadcast".to_string(),
                }));
            }

            return Ok(None);
        }

        Ok(None)
    }

    pub fn connected_peers(&self) -> Vec<String> {
        self.handle
            .connected_peers()
            .into_iter()
            .map(|p| p.to_string())
            .collect()
    }

    pub async fn gateway_start(
        &mut self,
        relay_mode_str: Option<String>,
    ) -> Result<String, String> {
        if self.gateway_bridge.is_some() {
            return Err("Gateway bridge is already active".to_string());
        }

        let relay_mode = match relay_mode_str.as_deref() {
            Some("disabled") => iroh::RelayMode::Disabled,
            _ => iroh::RelayMode::Default,
        };

        let id_bytes = self.node.identity().to_bytes();
        let id_clone = Identity::from_bytes(&id_bytes);

        let bridge = GatewayBridge::new_with_trust_store_and_db(
            id_clone,
            None,
            relay_mode,
            Arc::clone(&self.trust_store),
            Some(Arc::clone(&self.trust_db)),
        )
        .await
        .map_err(|e| format!("Failed to create GatewayBridge: {e}"))?;

        // Hydrate persisted gateway approvals/revocations/blocks BEFORE enabling ingress traffic
        {
            let snapshot: Vec<PersistedGatewayApproval> = self
                .persisted_gateway_approvals
                .read()
                .map(|m| m.values().cloned().collect())
                .unwrap_or_default();
            let cfg_arc = bridge.config();
            let mut cfg = cfg_arc.write().await;
            for rec in snapshot {
                if let Ok(ep) = rec.endpoint_id.parse::<iroh::EndpointId>() {
                    cfg.seed_consent_state(ep, rec.status.to_consent_state());
                }
            }
        }

        bridge.set_enabled(true).await;
        self.gateway_opted_in = true;

        let bridge = Arc::new(bridge);
        self.node.set_gateway_bridge(Arc::clone(&bridge));

        let ep = bridge.endpoint().clone();
        let bridge_clone = Arc::clone(&bridge);
        tokio::spawn(async move {
            while let Ok((_remote_id, conn)) =
                crate::gateway::iroh_link::accept_gateway_conn(&ep).await
            {
                let bridge_inner = Arc::clone(&bridge_clone);
                tokio::spawn(async move {
                    let _ = bridge_inner.spawn_session_handler(conn, None).await;
                });
            }
        });

        let endpoint_id_str = bridge.endpoint_id().to_string();
        self.gateway_bridge = Some(bridge);
        Ok(endpoint_id_str)
    }

    pub async fn gateway_stop(&mut self) -> Result<(), String> {
        self.gateway_opted_in = false;
        if let Some(bridge) = self.gateway_bridge.take() {
            self.node.clear_gateway_bridge();
            bridge.close().await;
            Ok(())
        } else {
            Err("Gateway bridge is not running".to_string())
        }
    }

    pub async fn gateway_export_contact(&self) -> Result<String, String> {
        let bridge = self
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

    pub async fn gateway_import_and_approve_contact(
        &mut self,
        contact_str: String,
    ) -> Result<String, String> {
        let bridge = self
            .gateway_bridge
            .as_ref()
            .ok_or("Gateway bridge is not running")?;
        let contact = bridge
            .import_and_approve_contact(&contact_str)
            .await
            .map_err(|e| format!("Failed to import contact: {e}"))?;
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if let Ok(pk) = contact.binding.bitchat_pubkey() {
            if let Ok(mut map) = self.persisted_gateway_approvals.write() {
                map.insert(
                    contact.iroh_endpoint_id_hex.clone(),
                    PersistedGatewayApproval {
                        endpoint_id: contact.iroh_endpoint_id_hex.clone(),
                        bitchat_pubkey: pk,
                        status: GatewayApprovalStatus::Approved,
                        updated_at_unix: now_secs,
                    },
                );
            }
        }
        self.gateway_opted_in = true;
        Ok(contact.iroh_endpoint_id_hex)
    }

    pub async fn gateway_connect_to_contact(&self, contact_str: String) -> Result<(), String> {
        let bridge = self
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

    pub async fn gateway_set_route(
        &self,
        recipient_short_id: String,
        target_endpoint_id_hex: String,
    ) -> Result<(), String> {
        let short_id = hex_to_short_id(&recipient_short_id)?;
        let endpoint_id = target_endpoint_id_hex
            .parse::<iroh::EndpointId>()
            .map_err(|e| format!("Invalid endpoint ID hex: {e}"))?;
        let bridge = self
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

    pub async fn gateway_set_route_for_peer(
        &self,
        recipient: PeerId,
        target_ep: iroh::EndpointId,
    ) -> Result<(), String> {
        let bridge = self
            .gateway_bridge
            .as_ref()
            .ok_or("Gateway bridge is not running")?;
        bridge
            .routes()
            .write()
            .await
            .set_route_for_peer(recipient, target_ep);
        Ok(())
    }

    pub async fn gateway_get_stats(&self) -> Result<String, String> {
        if let Some(bridge) = self.gateway_bridge.as_ref() {
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

    pub async fn gateway_get_routes(&self) -> Vec<String> {
        if let Some(bridge) = self.gateway_bridge.as_ref() {
            let my_short = self.node.identity().short_id();
            let routes = bridge.routes().read().await.list_routes();
            routes
                .into_iter()
                .map(|(sid, _)| sid)
                .filter(|sid| *sid != my_short)
                .map(|sid| sid.to_string())
                .collect()
        } else {
            Vec::new()
        }
    }

    pub async fn gateway_list_pending(&self) -> Result<String, String> {
        if let Some(bridge) = self.gateway_bridge.as_ref() {
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

    pub async fn gateway_approve_peer(&mut self, endpoint_id_hex: String) -> Result<(), String> {
        let endpoint_id = endpoint_id_hex
            .trim()
            .parse::<iroh::EndpointId>()
            .map_err(|e| format!("Invalid endpoint ID: {e}"))?;
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let bridge = self
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
        let known_pubkey = pending_pubkey.or_else(|| {
            self.persisted_gateway_approvals
                .read()
                .ok()
                .and_then(|m| {
                    m.get(&endpoint_id.to_string())
                        .map(|r| PeerId(r.bitchat_pubkey))
                })
                .filter(|p| p.0 != [0u8; 32])
        });

        // Verify local TrustStore does not block this peer before persisting approval
        if let Some(pid) = known_pubkey {
            if matches!(
                self.trust_store.read().await.peer_state(&pid),
                PeerTrustState::Blocked { .. }
            ) {
                return Err("Peer identity is blocked locally".to_string());
            }
        }

        let approved = bridge.config().write().await.approve_gateway(endpoint_id);
        if !approved {
            return Err("Gateway endpoint is blocked locally".to_string());
        }

        let mut caps = std::collections::HashSet::new();
        caps.insert(Capability::UseGatewayForForwarding);
        caps.insert(Capability::PermitOutboundGateway);
        caps.insert(Capability::ReceiveDirectMessage);

        let ep_str = endpoint_id.to_string();
        let gw_rec = PersistedGatewayApproval {
            endpoint_id: ep_str.clone(),
            bitchat_pubkey: known_pubkey.map(|p| p.0).unwrap_or([0u8; 32]),
            status: GatewayApprovalStatus::Approved,
            updated_at_unix: now_secs,
        };
        if let Some(pid) = known_pubkey {
            self.trust_db
                .upsert_peer_state(
                    &pid,
                    &PeerTrustState::Approved {
                        capabilities: caps.clone(),
                        granted_at_unix: now_secs,
                    },
                    TrustProvenance::ExplicitLocal,
                )
                .map_err(|e| format!("Failed to persist peer approval: {e}"))?;
        }
        self.trust_db
            .upsert_gateway_approval(&gw_rec)
            .map_err(|e| format!("Failed to persist gateway approval: {e}"))?;
        if let Ok(mut map) = self.persisted_gateway_approvals.write() {
            map.insert(ep_str, gw_rec);
        }

        if let Some(pid) = known_pubkey {
            let _ = self
                .trust_store
                .write()
                .await
                .approve_peer(pid, caps, now_secs);
            self.clear_pending_requests_for(&pid);
        }
        Ok(())
    }

    pub async fn gateway_revoke_or_block_peer(
        &self,
        endpoint_id_hex: String,
        block: bool,
    ) -> Result<(), String> {
        let endpoint_id = endpoint_id_hex
            .trim()
            .parse::<iroh::EndpointId>()
            .map_err(|e| format!("Invalid endpoint ID: {e}"))?;
        let ep_str = endpoint_id.to_string();
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let known_pubkey = if let Some(ref bridge) = self.gateway_bridge {
            bridge.session_pubkey(&endpoint_id).await
        } else {
            None
        }
        .or_else(|| {
            self.persisted_gateway_approvals
                .read()
                .ok()
                .and_then(|m| m.get(&ep_str).map(|r| r.bitchat_pubkey))
                .filter(|pk| *pk != [0u8; 32])
        });

        let gw_status = if block {
            GatewayApprovalStatus::Blocked
        } else {
            GatewayApprovalStatus::Revoked
        };
        let gw_rec = PersistedGatewayApproval {
            endpoint_id: ep_str.clone(),
            bitchat_pubkey: known_pubkey.unwrap_or([0u8; 32]),
            status: gw_status,
            updated_at_unix: now_secs,
        };
        self.trust_db
            .upsert_gateway_approval(&gw_rec)
            .map_err(|e| format!("Failed to persist gateway state: {e}"))?;
        if let Ok(mut map) = self.persisted_gateway_approvals.write() {
            map.insert(ep_str, gw_rec);
        }

        if let Some(ref bridge) = self.gateway_bridge {
            bridge.revoke_or_block_peer(endpoint_id, block).await;
        }
        if let Some(pk) = known_pubkey {
            let pid = PeerId(pk);
            let st = if block {
                PeerTrustState::Blocked {
                    blocked_at_unix: now_secs,
                }
            } else {
                let ts = self.trust_store.read().await;
                match ts.peer_state(&pid) {
                    PeerTrustState::Approved {
                        mut capabilities,
                        granted_at_unix,
                    } => {
                        capabilities.remove(&Capability::UseGatewayForForwarding);
                        capabilities.remove(&Capability::PermitOutboundGateway);
                        if capabilities.is_empty() {
                            PeerTrustState::Revoked {
                                revoked_at_unix: now_secs,
                            }
                        } else {
                            PeerTrustState::Approved {
                                capabilities,
                                granted_at_unix,
                            }
                        }
                    }
                    _ => PeerTrustState::Revoked {
                        revoked_at_unix: now_secs,
                    },
                }
            };
            self.trust_db
                .upsert_peer_state(&pid, &st, TrustProvenance::ExplicitLocal)
                .map_err(|e| format!("Failed to persist peer state: {e}"))?;
            self.trust_store.write().await.restore_peer_state(pid, st);
        }
        Ok(())
    }

    pub async fn trust_set_peer_state(
        &mut self,
        peer_id_hex: String,
        action: String,
    ) -> Result<(), String> {
        let pid = PeerId::from_hex(&peer_id_hex)
            .ok_or_else(|| "Invalid PeerId: expected 64 strict hex characters".to_string())?;
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let (new_state, bridge_to_revoke) = {
            let mut ts = self.trust_store.write().await;
            let (st, bridge_arg) = match action.as_str() {
                "approve" => {
                    let mut caps = std::collections::HashSet::new();
                    caps.insert(Capability::ReceiveDirectMessage);
                    caps.insert(Capability::UseGatewayForForwarding);
                    caps.insert(Capability::PermitOutboundGateway);
                    let st = PeerTrustState::Approved {
                        capabilities: caps.clone(),
                        granted_at_unix: now_secs,
                    };
                    // Validate quota before persisting
                    let prev_state = ts.peer_state(&pid);
                    ts.approve_peer(pid, caps, now_secs)
                        .map_err(|e| format!("Approve failed: {e}"))?;
                    if let Err(e) =
                        self.trust_db
                            .upsert_peer_state(&pid, &st, TrustProvenance::ExplicitLocal)
                    {
                        ts.restore_peer_state(pid, prev_state);
                        return Err(format!("Failed to persist peer approval: {e}"));
                    }
                    (st, None)
                }
                "introducer" => {
                    let mut caps = std::collections::HashSet::new();
                    caps.insert(Capability::ReceiveDirectMessage);
                    caps.insert(Capability::UseGatewayForForwarding);
                    caps.insert(Capability::PermitOutboundGateway);
                    caps.insert(Capability::ActAsIntroducer);
                    let st = PeerTrustState::Approved {
                        capabilities: caps.clone(),
                        granted_at_unix: now_secs,
                    };
                    let prev_state = ts.peer_state(&pid);
                    ts.approve_peer(pid, caps, now_secs)
                        .map_err(|e| format!("Approve introducer failed: {e}"))?;
                    if let Err(e) =
                        self.trust_db
                            .upsert_peer_state(&pid, &st, TrustProvenance::ExplicitLocal)
                    {
                        ts.restore_peer_state(pid, prev_state);
                        return Err(format!("Failed to persist introducer approval: {e}"));
                    }
                    (st, None)
                }
                "revoke" => {
                    let st = PeerTrustState::Revoked {
                        revoked_at_unix: now_secs,
                    };
                    self.trust_db
                        .upsert_peer_state(&pid, &st, TrustProvenance::ExplicitLocal)
                        .map_err(|e| format!("Failed to persist revocation: {e}"))?;
                    ts.revoke_peer(pid, now_secs);
                    (
                        st,
                        self.gateway_bridge.as_ref().map(|b| (Arc::clone(b), false)),
                    )
                }
                "block" => {
                    let st = PeerTrustState::Blocked {
                        blocked_at_unix: now_secs,
                    };
                    self.trust_db
                        .upsert_peer_state(&pid, &st, TrustProvenance::ExplicitLocal)
                        .map_err(|e| format!("Failed to persist block: {e}"))?;
                    ts.block_peer(pid, now_secs);
                    (
                        st,
                        self.gateway_bridge.as_ref().map(|b| (Arc::clone(b), true)),
                    )
                }
                other => return Err(format!("Unsupported trust action: {other}")),
            };
            (st, bridge_arg)
        };
        let _ = new_state;

        if action == "revoke" || action == "block" {
            let gw_status = if action == "block" {
                GatewayApprovalStatus::Blocked
            } else {
                GatewayApprovalStatus::Revoked
            };
            let matched_eps: Vec<PersistedGatewayApproval> = self
                .persisted_gateway_approvals
                .read()
                .map(|m| {
                    m.values()
                        .filter(|r| r.bitchat_pubkey == pid.0)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            for mut rec in matched_eps {
                rec.status = gw_status;
                rec.updated_at_unix = now_secs;
                self.trust_db
                    .upsert_gateway_approval(&rec)
                    .map_err(|e| format!("Failed to persist gateway revocation: {e}"))?;
                if let Ok(mut map) = self.persisted_gateway_approvals.write() {
                    map.insert(rec.endpoint_id.clone(), rec);
                }
            }
        }

        if let Some((bridge, block)) = bridge_to_revoke {
            bridge.revoke_or_block_by_peer_id(&pid, block).await;
        }
        if action == "revoke" || action == "block" {
            let reason_label = if action == "block" {
                "RecipientBlocked"
            } else {
                "RecipientRevoked"
            };
            let failed_lids = self
                .delivery_coord
                .fail_for_recipient(&pid, RouteReason::ExhaustedAttemptsQueue);
            for (lid, attempts, expires_at) in failed_lids {
                let _ = self.outbox_mgr.mark_failed(&lid);
                self.delivery_events.push(DeliveryEventRecord {
                    logical_id: hex::encode(lid),
                    state: "Failed".to_string(),
                    recipient: pid.to_string(),
                    timestamp: now_secs,
                    reason: reason_label.to_string(),
                    transport: "Unknown".to_string(),
                    attempt_count: attempts,
                    expires_at,
                });
            }
        }
        self.clear_pending_requests_for(&pid);

        Ok(())
    }

    pub async fn trust_issue_voucher(
        &mut self,
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

        let seq = self.next_voucher_seq;
        self.next_voucher_seq = self.next_voucher_seq.saturating_add(1);

        let voucher = SignedVoucher::issue(
            self.node.identity(),
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

    pub async fn trust_import_voucher(&mut self, voucher_uri: String) -> Result<String, String> {
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
        let now_instant = Instant::now();
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        {
            let mut ts = self.trust_store.write().await;
            ts.import_voucher(&voucher, now_instant, Some(now_secs))
                .map_err(|e| format!("Voucher rejected: {e}"))?;
            if let Some(grant) = ts.get_voucher(&voucher.credential_id).cloned() {
                if let Err(e) = self.trust_db.upsert_voucher(&grant) {
                    return Err(format!("Failed to persist voucher: {e}"));
                }
            }
        }

        let summary = serde_json::json!({
            "issuer": voucher.issuer.to_string(),
            "subject": voucher.subject.to_string(),
            "subject_short_id": voucher.subject.short_id().to_string(),
            "capability": format!("{:?}", voucher.capability),
            "expires_at_unix": voucher.expires_at_unix,
        });
        serde_json::to_string(&summary).map_err(|e| format!("Serialization error: {e}"))
    }

    pub async fn trust_import_revocation(
        &mut self,
        rev: SignedVoucherRevocation,
    ) -> Result<(), String> {
        let now_instant = Instant::now();
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut ts = self.trust_store.write().await;
        ts.import_revocation(&rev, now_instant)
            .map_err(|e| format!("Revocation rejected: {e}"))?;
        self.trust_db
            .upsert_revocation(
                &rev.issuer,
                &rev.credential_id,
                rev.revocation_seq,
                now_secs,
            )
            .map_err(|e| format!("Failed to persist voucher revocation: {e}"))?;
        Ok(())
    }

    pub fn get_peer_id(&self, short_id: String) -> Result<String, String> {
        let peer_short =
            ShortId::from_hex(&short_id).ok_or_else(|| "Invalid short ID".to_string())?;
        self.peer_mgr
            .verified_peer_id(&peer_short)
            .or_else(|| self.known_peer_ids.get(&peer_short).copied())
            .map(|p| p.to_string())
            .ok_or_else(|| "Peer session not established or peer ID unknown".to_string())
    }

    pub fn update_resource_state(
        &mut self,
        battery_pct: i32,
        charging: i32,
        power_save: i32,
        thermal: i32,
        user_paused: bool,
    ) -> String {
        let now_inst = Instant::now();
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
            user_profile_override: self.resource_snapshot.user_profile_override,
            timestamp: now_inst,
        };

        if user_paused {
            self.relay_contribution_mode = "receive_only".to_string();
        } else if self.relay_contribution_mode == "receive_only" {
            self.relay_contribution_mode = "balanced".to_string();
        }

        let profile = if let Ok(mut mgr) = self.resource_mgr.lock() {
            mgr.update_snapshot_with_control(&snap, false)
        } else {
            RelayResourceProfile::Balanced
        };
        self.resource_snapshot = snap;
        format!("{profile:?}")
    }

    pub fn set_relay_contribution_profile(&mut self, mode: &str) -> Result<String, String> {
        let now_inst = Instant::now();
        let (paused, override_prof, normalized_mode) = match mode.trim() {
            "receive_only" | "paused" => (true, None, "receive_only"),
            "balanced" => (false, Some(RelayResourceProfile::Balanced), "balanced"),
            "help_network" | "assisted" => {
                (false, Some(RelayResourceProfile::Assisted), "help_network")
            }
            "auto" => (false, None, "auto"),
            other => return Err(format!("Unsupported relay contribution mode: {other}")),
        };

        self.resource_snapshot.user_paused_relaying = paused;
        self.resource_snapshot.user_profile_override = override_prof;
        self.resource_snapshot.timestamp = now_inst;
        self.relay_contribution_mode = normalized_mode.to_string();

        let profile = if let Ok(mut mgr) = self.resource_mgr.lock() {
            mgr.update_snapshot_with_control(&self.resource_snapshot, true)
        } else {
            RelayResourceProfile::Balanced
        };
        Ok(format!("{profile:?}"))
    }

    pub async fn record_ble_rssi(
        &mut self,
        short_id_hex: String,
        rssi_dbm: i8,
    ) -> Result<String, String> {
        let peer_short = hex_to_short_id(&short_id_hex)?;
        let link_key = peer_short.to_string();
        let now_inst = Instant::now();

        self.proximity_tracker
            .record_direct_link_rssi(&link_key, rssi_dbm, 1, now_inst);

        let verified_pid = self
            .resolve_verified_peer_id(&short_id_hex)
            .await?
            .map(|(pid, _)| pid);
        if let Some(pid) = verified_pid {
            self.proximity_tracker
                .bind_verified_peer(&link_key, pid, now_inst);
            let cat = self.proximity_tracker.peer_proximity(&pid, now_inst);
            Ok(format!("{cat:?}"))
        } else {
            Ok("Unknown".to_string())
        }
    }

    pub async fn get_peer_proximity(&mut self, short_id_hex: String) -> Result<String, String> {
        let now_inst = Instant::now();
        let verified_pid = self
            .resolve_verified_peer_id(&short_id_hex)
            .await?
            .map(|(pid, _)| pid);
        if let Some(pid) = verified_pid {
            let cat = self.proximity_tracker.peer_proximity(&pid, now_inst);
            Ok(format!("{cat:?}"))
        } else {
            Ok("Unknown".to_string())
        }
    }

    pub async fn set_volatile_session(
        &mut self,
        recipient_short_id: String,
        retention_secs: u32,
    ) -> Result<String, String> {
        let Some((recipient_peer_id, target)) =
            self.resolve_verified_peer_id(&recipient_short_id).await?
        else {
            return Err("Peer full identity not verified yet; session handshake required".into());
        };

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        if retention_secs == 0 {
            let close_frame = match self.volatile_mgr.session_state(&recipient_peer_id) {
                VolatileSessionState::PendingNegotiation { session_id, .. }
                | VolatileSessionState::Active { session_id, .. } => {
                    Some(VolatileNegotiationFrame::new_close(session_id))
                }
                _ => None,
            };
            self.volatile_mgr.close_conversation(
                &recipient_peer_id,
                &mut self.outbox_mgr,
                &mut self.delivery_coord,
            );
            if let Some(cf) = close_frame {
                if self.peer_mgr.is_session_ready(&target) {
                    let mut lid = [0u8; 16];
                    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut lid);
                    let app_frame = ApplicationFrame::new_message(
                        lid,
                        self.node.identity().peer_id(),
                        recipient_peer_id,
                        cf.encode().to_vec(),
                        Duration::from_secs(60),
                        now_secs,
                    );
                    if let Ok(padded) = pad_inner_payload(&app_frame.encode(), true) {
                        let _ = self
                            .peer_mgr
                            .send_encrypted(&mut self.node, target, &padded)
                            .await;
                    }
                }
            }
            return Ok("NotRequested".to_string());
        }

        let offer = self
            .volatile_mgr
            .initiate_volatile_session(recipient_peer_id, retention_secs);

        if self.peer_mgr.is_session_ready(&target) {
            let mut lid = [0u8; 16];
            rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut lid);
            let app_frame = ApplicationFrame::new_message(
                lid,
                self.node.identity().peer_id(),
                recipient_peer_id,
                offer.encode().to_vec(),
                Duration::from_secs(60),
                now_secs,
            );
            if let Ok(padded) = pad_inner_payload(&app_frame.encode(), true) {
                let _ = self
                    .peer_mgr
                    .send_encrypted(&mut self.node, target, &padded)
                    .await;
            }
        } else {
            let _ = self
                .peer_mgr
                .initiate_handshake(&mut self.node, target)
                .await;
        }

        Ok("PendingNegotiation".to_string())
    }

    pub async fn get_volatile_state(&self, recipient_short_id: String) -> Result<String, String> {
        let verified_pid = self
            .resolve_verified_peer_id(&recipient_short_id)
            .await?
            .map(|(pid, _)| pid);

        let (state_str, ret_secs) = match verified_pid {
            None => ("NotRequested", 0u64),
            Some(ref recipient_peer_id) => {
                match self.volatile_mgr.session_state(recipient_peer_id) {
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
                }
            }
        };

        let json = serde_json::json!({
            "state": state_str,
            "retention_secs": ret_secs,
        });
        serde_json::to_string(&json).map_err(|e| format!("Serialization error: {e}"))
    }
}

/// Notify Rust that a BLE peer connected.
pub async fn on_peer_connected(short_id: String) -> Result<(), String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.on_peer_connected(short_id).await
}

/// Notify Rust that a BLE peer disconnected.
pub async fn on_peer_disconnected(short_id: String) -> Result<(), String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.on_peer_disconnected(short_id).await
}

/// Deliver an incoming BLE frame payload from a connected peer into Rust.
pub async fn on_frame_received(short_id: String, bytes: Vec<u8>) -> Result<(), String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    svc.on_frame_received(short_id, bytes)
}

/// Poll next outbound frame from Rust to be transmitted across mobile BLE.
pub async fn poll_outbound() -> Result<Option<BridgeOutbound>, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    Ok(svc.poll_outbound())
}

/// Step node and poll next delivered inbound message for UI display.
pub async fn poll_inbound() -> Result<Option<BridgeMessage>, String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.poll_inbound().await
}

/// Return list of currently connected BLE peer ShortIds.
pub async fn connected_peers() -> Result<Vec<String>, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    Ok(svc.connected_peers())
}

/// Start internet gateway bridge on local node.
pub async fn gateway_start(relay_mode_str: Option<String>) -> Result<String, String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.gateway_start(relay_mode_str).await
}

/// Stop internet gateway bridge on local node.
pub async fn gateway_stop() -> Result<(), String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.gateway_stop().await
}

/// Export this node's gateway pairing contact string (URI) for out-of-band QR sharing.
/// Generates a bounded, single-use pairing invitation capability with 5-minute monotonic expiry.
pub async fn gateway_export_contact() -> Result<String, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    svc.gateway_export_contact().await
}

/// Import, verify, and approve a remote gateway contact string.
pub async fn gateway_import_and_approve_contact(contact_str: String) -> Result<String, String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.gateway_import_and_approve_contact(contact_str).await
}

/// Dial and establish connection to an approved remote gateway contact URI.
pub async fn gateway_connect_to_contact(contact_str: String) -> Result<(), String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    svc.gateway_connect_to_contact(contact_str).await
}

/// Set manual route mapping a destination ShortId hex to a target Gateway EndpointId hex.
pub async fn gateway_set_route(
    recipient_short_id: String,
    target_endpoint_id_hex: String,
) -> Result<(), String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    svc.gateway_set_route(recipient_short_id, target_endpoint_id_hex)
        .await
}

/// Get current gateway statistics snapshot as JSON string.
pub async fn gateway_get_stats() -> Result<String, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    svc.gateway_get_stats().await
}

/// Query list of remote recipient ShortIds currently reachable through active gateway bridges.
pub async fn gateway_get_routes() -> Result<Vec<String>, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    Ok(svc.gateway_get_routes().await)
}

/// List incoming gateway peers currently in `IdentityVerifiedPendingConsent` as a JSON array.
pub async fn gateway_list_pending() -> Result<String, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    svc.gateway_list_pending().await
}

/// Explicitly approve a pending or recognized gateway endpoint ID (`IdentityVerifiedPendingConsent -> Approved`).
pub async fn gateway_approve_peer(endpoint_id_hex: String) -> Result<(), String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.gateway_approve_peer(endpoint_id_hex).await
}

/// Revoke or permanently block a gateway endpoint ID, atomically withdrawing routes and closing sessions.
pub async fn gateway_revoke_or_block_peer(
    endpoint_id_hex: String,
    block: bool,
) -> Result<(), String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    svc.gateway_revoke_or_block_peer(endpoint_id_hex, block)
        .await
}

/// Set local trust state for a 64-hex `PeerId`: `"approve"`, `"introducer"`, `"revoke"`, or `"block"`.
pub async fn trust_set_peer_state(peer_id_hex: String, action: String) -> Result<(), String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.trust_set_peer_state(peer_id_hex, action).await
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
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.trust_issue_voucher(
        subject_peer_id_hex,
        audience_peer_id_hex,
        capability_str,
        lifetime_secs,
    )
    .await
}

/// Import and verify a signed one-hop capability voucher (`bitchat-voucher://<hex>`).
pub async fn trust_import_voucher(voucher_uri: String) -> Result<String, String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.trust_import_voucher(voucher_uri).await
}

/// Retrieve the verified 64-hex PeerId of a direct peer if Noise handshake is established.
pub async fn get_peer_id(short_id: String) -> Result<String, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    svc.get_peer_id(short_id)
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
    Ok(svc.update_resource_state(battery_pct, charging, power_save, thermal, user_paused))
}

/// Set user relay contribution mode (`"receive_only"`, `"balanced"`, `"help_network"`, or `"auto"`).
pub async fn set_relay_contribution_profile(mode: String) -> Result<String, String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.set_relay_contribution_profile(&mode)
}

/// Record a direct-link BLE RSSI sample (`rssi_dbm`) for `short_id_hex` and return its coarse proximity category.
pub async fn record_ble_rssi(short_id_hex: String, rssi_dbm: i8) -> Result<String, String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.record_ble_rssi(short_id_hex, rssi_dbm).await
}

/// Query coarse proximity category (`"Nearby"`, `"Further"`, `"Unknown"`) for a peer `ShortId`.
pub async fn get_peer_proximity(short_id_hex: String) -> Result<String, String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.get_peer_proximity(short_id_hex).await
}

/// Initiate or update an authenticated volatile conversation with `recipient_short_id`.
///
/// If `retention_secs == 0`, closes the volatile session and cancels pending volatile sends.
pub async fn set_volatile_session(
    recipient_short_id: String,
    retention_secs: u32,
) -> Result<String, String> {
    let mut lock = get_service().write().await;
    let svc = lock.as_mut().ok_or("Service not initialized")?;
    svc.set_volatile_session(recipient_short_id, retention_secs)
        .await
}

/// Query volatile conversation state for `recipient_short_id` as JSON (`{state, retention_secs}`).
pub async fn get_volatile_state(recipient_short_id: String) -> Result<String, String> {
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    svc.get_volatile_state(recipient_short_id).await
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

        // Connect neighbor (unverified ShortId)
        let neighbor_hex = "0102030405060708";
        on_peer_connected(neighbor_hex.to_string()).await.unwrap();
        let peers = connected_peers().await.unwrap();
        assert_eq!(peers, vec![neighbor_hex.to_string()]);

        // B2 invariant: unverified ShortId must not synthesize a fake PeerId
        assert!(send_message(neighbor_hex.to_string(), "hi".to_string(), 0)
            .await
            .is_err());
        assert!(set_volatile_session(neighbor_hex.to_string(), 300)
            .await
            .is_err());
        assert_eq!(
            get_peer_proximity(neighbor_hex.to_string()).await.unwrap(),
            "Unknown"
        );

        // Drain any auto-initiated handshake frame (if my_short < neighbor) before testing broadcast
        while poll_outbound().await.unwrap().is_some() {}

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

        // Register a cryptographically verified peer identity for Phase 4 & 5 API checks
        let real_pid = Identity::generate().peer_id();
        let verified_short = real_pid.short_id().to_string();
        get_service()
            .write()
            .await
            .as_mut()
            .unwrap()
            .register_verified_peer_id(real_pid);

        // Test Phase 4 Delivery & Diagnostics API
        let logical_id = send_message(
            verified_short.clone(),
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

        let _ = record_ble_rssi(verified_short.clone(), -58).await;
        let _ = record_ble_rssi(verified_short.clone(), -57).await;
        let prox = record_ble_rssi(verified_short.clone(), -56)
            .await
            .expect("Record RSSI");
        assert_eq!(prox, "Nearby");
        assert_eq!(
            get_peer_proximity(verified_short.clone()).await.unwrap(),
            "Nearby"
        );

        let vol_st = set_volatile_session(verified_short.clone(), 300)
            .await
            .expect("Set volatile session");
        assert_eq!(vol_st, "PendingNegotiation");
        let vol_json = get_volatile_state(verified_short)
            .await
            .expect("Get volatile state");
        assert!(vol_json.contains("\"PendingNegotiation\""));

        let subject_pk = Identity::generate().peer_id().to_string();
        let audience_pk = Identity::generate().peer_id().to_string();
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
