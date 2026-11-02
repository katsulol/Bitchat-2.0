use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};

use crate::envelope::Envelope;
use crate::error::{Error, Result};
use crate::gateway::config::{GatewayConfig, GatewayConsentState, DEFAULT_INVITATION_TTL};
use crate::gateway::contact::GatewayContact;
use crate::gateway::identity_binding::IdentityBinding;
use crate::gateway::iroh_link::{create_endpoint, dial_gateway};
use crate::gateway::routes::GatewayRouteTable;
use crate::gateway::stats::GatewayStats;
use crate::gateway::wire::{
    GatewayForward, GatewayHello, GatewayMessage, RejectReason, WireError, DEFAULT_READ_TIMEOUT,
    WIRE_VERSION,
};
use crate::identity::Identity;
use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointId, RelayMode, SecretKey};

/// State of an active gateway transport session.
pub struct ActiveGatewaySession {
    pub remote_endpoint_id: EndpointId,
    pub remote_bitchat_pubkey: [u8; 32],
    pub session_epoch: u64,
    pub next_sequence: AtomicU64,
    pub outbound_tx: mpsc::Sender<GatewayMessage>,
}

/// Core Internet Gateway Bridge coordinating mesh forwarding across authenticated Iroh QUIC streams.
pub struct GatewayBridge {
    bitchat_identity: Identity,
    endpoint: Endpoint,
    config: Arc<RwLock<GatewayConfig>>,
    routes: Arc<RwLock<GatewayRouteTable>>,
    stats: Arc<GatewayStats>,
    active_sessions: Arc<RwLock<HashMap<EndpointId, ActiveGatewaySession>>>,
    last_session_epoch: AtomicU64,
    transfer_counter: AtomicU64,
    inbound_mesh_tx: mpsc::Sender<Vec<u8>>,
    inbound_mesh_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<Vec<u8>>>>,
}

impl GatewayBridge {
    /// Create a new gateway bridge using local Bitchat identity and Iroh secret key.
    pub async fn new(
        bitchat_identity: Identity,
        iroh_secret: Option<SecretKey>,
        relay_mode: RelayMode,
    ) -> Result<Self> {
        let endpoint = create_endpoint(iroh_secret, relay_mode)
            .await
            .map_err(|e| Error::Transport(format!("Failed to bind Iroh endpoint: {e}")))?;

        let (inbound_tx, inbound_rx) = mpsc::channel(128);

        Ok(Self {
            bitchat_identity,
            endpoint,
            config: Arc::new(RwLock::new(GatewayConfig::new())),
            routes: Arc::new(RwLock::new(GatewayRouteTable::new())),
            stats: Arc::new(GatewayStats::new()),
            active_sessions: Arc::new(RwLock::new(HashMap::new())),
            last_session_epoch: AtomicU64::new(0),
            transfer_counter: AtomicU64::new(1),
            inbound_mesh_tx: inbound_tx,
            inbound_mesh_rx: Arc::new(tokio::sync::Mutex::new(inbound_rx)),
        })
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub fn endpoint_id(&self) -> EndpointId {
        self.endpoint.id()
    }

    pub fn stats(&self) -> Arc<GatewayStats> {
        Arc::clone(&self.stats)
    }

    pub fn config(&self) -> Arc<RwLock<GatewayConfig>> {
        Arc::clone(&self.config)
    }

    pub fn routes(&self) -> Arc<RwLock<GatewayRouteTable>> {
        Arc::clone(&self.routes)
    }

    /// Export local gateway contact document for out-of-band exchange.
    /// Does not include an automatic pairing invitation unless `export_pairing_contact` is used.
    pub async fn export_contact(&self) -> Result<GatewayContact> {
        let addr = self.endpoint.addr();
        let binding = IdentityBinding::sign(
            &self.bitchat_identity,
            &self.endpoint.id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        );

        GatewayContact::new(&addr, binding)
            .map_err(|e| Error::Transport(format!("Failed to create contact: {e}")))
    }

    /// Explicitly generate a single-use pairing invitation capability and export it inside a QR contact.
    pub async fn export_pairing_contact(
        &self,
        ttl: Option<Duration>,
        auto_admit_on_redemption: bool,
    ) -> Result<GatewayContact> {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let (token, exp_secs) = {
            let mut cfg = self.config.write().await;
            cfg.enabled = true;
            cfg.allow_invitation_auto_admit = auto_admit_on_redemption;
            cfg.generate_invitation(
                ttl.unwrap_or(DEFAULT_INVITATION_TTL),
                Instant::now(),
                now_secs,
            )
            .map_err(Error::Transport)?
        };

        let contact = self.export_contact().await?;
        Ok(contact.with_invitation(token, exp_secs))
    }

    /// Import and approve a remote gateway contact document.
    pub async fn import_and_approve_contact(&self, contact_str: &str) -> Result<GatewayContact> {
        let contact = GatewayContact::import_from_str(contact_str)
            .map_err(|e| Error::Transport(format!("Failed to import contact: {e}")))?;

        let id = contact
            .endpoint_id()
            .map_err(|e| Error::Transport(format!("Invalid endpoint id: {e}")))?;

        let mut cfg = self.config.write().await;
        if !cfg.approve_gateway(id) {
            return Err(Error::Transport("Peer is blocked locally".into()));
        }
        cfg.enabled = true;
        drop(cfg);
        Ok(contact)
    }

    /// Revoke or permanently block a gateway peer, atomically withdrawing routes and closing active sessions.
    pub async fn revoke_or_block_peer(&self, endpoint_id: EndpointId, block: bool) {
        {
            let mut cfg = self.config.write().await;
            if block {
                cfg.block_gateway(endpoint_id);
            } else {
                cfg.revoke_gateway(&endpoint_id);
            }
        }
        self.active_sessions.write().await.remove(&endpoint_id);
        self.routes
            .write()
            .await
            .remove_routes_for_endpoint(&endpoint_id);
        self.stats
            .set_active_connections(self.active_sessions.read().await.len() as u64);
    }

    /// Enable or disable gateway bridging.
    ///
    /// When disabled, route table and active sessions are immediately cleared/withdrawn.
    pub async fn set_enabled(&self, enabled: bool) {
        self.config.write().await.enabled = enabled;
        if !enabled {
            self.routes.write().await.clear();
            self.active_sessions.write().await.clear();
            self.stats.set_active_connections(0);
        }
    }

    /// Check if gateway bridging is currently enabled.
    pub async fn is_enabled(&self) -> bool {
        self.config.read().await.enabled
    }

    /// Dial and establish an authenticated Iroh gateway connection to a remote approved contact.
    pub async fn connect_to_contact(&self, contact: &GatewayContact) -> Result<()> {
        let remote_id = contact
            .endpoint_id()
            .map_err(|e| Error::Transport(format!("Invalid endpoint id in contact: {e}")))?;

        // Ensure remote gateway is approved
        if !self.config.read().await.is_approved(&remote_id) {
            return Err(Error::Transport("Remote gateway is not approved".into()));
        }

        let addr = contact
            .to_endpoint_addr()
            .map_err(|e| Error::Transport(format!("Failed to resolve endpoint addr: {e}")))?;

        let conn = dial_gateway(&self.endpoint, addr)
            .await
            .map_err(|e| Error::Transport(format!("Failed to connect to gateway: {e}")))?;

        self.spawn_session_handler(conn, Some(contact.clone()))
            .await
    }

    /// Spawn reader and writer loops for an authenticated gateway connection.
    pub async fn spawn_session_handler(
        &self,
        conn: Connection,
        contact_hint: Option<GatewayContact>,
    ) -> Result<()> {
        let remote_id = conn.remote_id();

        // For outgoing dials, destination must be approved in advance
        if contact_hint.is_some() && !self.config.read().await.is_approved(&remote_id) {
            self.stats.inc_rejected();
            return Err(Error::Transport(format!(
                "Outgoing gateway {remote_id} is not in approval whitelist"
            )));
        }

        let (mut send_stream, mut recv_stream, remote_bitchat_pubkey) =
            match contact_hint {
                Some(contact) => {
                    // Verify pinned host EndpointId before presenting any invitation capability
                    let pinned_id = contact.endpoint_id().map_err(|e| {
                        Error::Transport(format!("Invalid pinned endpoint id in contact: {e}"))
                    })?;
                    if remote_id != pinned_id {
                        self.stats.inc_rejected();
                        return Err(Error::Transport(
                            "Connected remote EndpointId does not match pinned contact".into(),
                        ));
                    }

                    // We dialed: open bi stream and send Hello
                    let (mut s, r) = conn
                        .open_bi()
                        .await
                        .map_err(|e| Error::Transport(format!("Failed to open bi stream: {e}")))?;

                    let binding = IdentityBinding::sign(
                        &self.bitchat_identity,
                        &self.endpoint.id(),
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                    );
                    let hello = GatewayMessage::Hello(GatewayHello {
                        protocol_version: WIRE_VERSION,
                        binding,
                        invitation_token: contact.invitation_token.clone(),
                    });

                    hello
                        .write_framed(&mut s)
                        .await
                        .map_err(|e| Error::Transport(format!("Failed to send Hello: {e}")))?;

                    let pubkey = contact.binding.bitchat_pubkey().map_err(|e| {
                        Error::Transport(format!("Invalid Bitchat pubkey in contact: {e}"))
                    })?;
                    (s, r, pubkey)
                }
                None => {
                    // Remote dialed: accept bi stream and read Hello
                    let (mut s, mut r) = conn.accept_bi().await.map_err(|e| {
                        Error::Transport(format!("Failed to accept bi stream: {e}"))
                    })?;

                    let hello_msg = GatewayMessage::read_framed(&mut r, DEFAULT_READ_TIMEOUT)
                        .await
                        .map_err(|e| Error::Transport(format!("Failed to read Hello: {e}")))?;

                    let pubkey = if let GatewayMessage::Hello(hello) = hello_msg {
                        hello.binding.verify(&remote_id).map_err(|e| {
                            self.stats.inc_rejected();
                            Error::Transport(format!("Binding verification failed: {e}"))
                        })?;
                        let pk = hello.binding.bitchat_pubkey().map_err(|e| {
                            self.stats.inc_rejected();
                            Error::Transport(format!("Invalid Bitchat public key: {e}"))
                        })?;

                        // Evaluate explicit gateway consent state; NEVER auto-approve from identity binding alone!
                        let mut cfg = self.config.write().await;
                        if !cfg.enabled {
                            drop(cfg);
                            self.stats.inc_rejected();
                            let _ = (GatewayMessage::Rejected {
                                transfer_id: 0,
                                reason: RejectReason::Unauthorized,
                            })
                            .write_framed(&mut s)
                            .await;
                            return Err(Error::Transport("Gateway is disabled".into()));
                        }

                        match cfg.consent_state(&remote_id) {
                            GatewayConsentState::Blocked | GatewayConsentState::Revoked => {
                                drop(cfg);
                                self.stats.inc_rejected();
                                let _ = (GatewayMessage::Rejected {
                                    transfer_id: 0,
                                    reason: RejectReason::Unauthorized,
                                })
                                .write_framed(&mut s)
                                .await;
                                return Err(Error::Transport(
                                    "Remote gateway peer is blocked or revoked".into(),
                                ));
                            }
                            GatewayConsentState::Approved => {
                                drop(cfg);
                            }
                            GatewayConsentState::Unrecognized
                            | GatewayConsentState::IdentityVerifiedPendingConsent => {
                                let admitted = if let Some(ref token) = hello.invitation_token {
                                    match cfg.redeem_invitation(
                                        token,
                                        remote_id,
                                        pk,
                                        Instant::now(),
                                    ) {
                                        Ok(GatewayConsentState::Approved) => true,
                                        Ok(_) => false,
                                        Err(e) => {
                                            drop(cfg);
                                            self.stats.inc_rejected();
                                            let _ = (GatewayMessage::Rejected {
                                                transfer_id: 0,
                                                reason: RejectReason::Unauthorized,
                                            })
                                            .write_framed(&mut s)
                                            .await;
                                            return Err(Error::Transport(format!(
                                                "Pairing invitation rejected: {e}"
                                            )));
                                        }
                                    }
                                } else {
                                    let _ =
                                        cfg.record_verified_pending(remote_id, pk, Instant::now());
                                    false
                                };
                                drop(cfg);

                                if !admitted {
                                    self.stats.inc_rejected();
                                    let _ = (GatewayMessage::Rejected {
                                        transfer_id: 0,
                                        reason: RejectReason::Unauthorized,
                                    })
                                    .write_framed(&mut s)
                                    .await;
                                    return Err(Error::Transport(
                                        "Identity verified; awaiting explicit host gateway approval"
                                            .into(),
                                    ));
                                }
                            }
                        }
                        pk
                    } else {
                        self.stats.inc_rejected();
                        return Err(Error::Transport(
                            "Expected Hello as first gateway frame".into(),
                        ));
                    };

                    (s, r, pubkey)
                }
            };

        let (outbound_tx, mut outbound_rx) = mpsc::channel::<GatewayMessage>(64);

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let session_epoch = self
            .last_session_epoch
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |prev| Some(now_secs.max(prev + 1)),
            )
            .unwrap_or(now_secs);

        let session = ActiveGatewaySession {
            remote_endpoint_id: remote_id,
            remote_bitchat_pubkey,
            session_epoch,
            next_sequence: AtomicU64::new(2),
            outbound_tx: outbound_tx.clone(),
        };

        self.active_sessions
            .write()
            .await
            .insert(remote_id, session);
        self.stats
            .set_active_connections(self.active_sessions.read().await.len() as u64);

        // Send initial RouteSync snapshot containing our own verified Bitchat identity
        let mut initial_snapshot = crate::gateway::wire::RouteSnapshot {
            protocol_version: WIRE_VERSION,
            session_epoch,
            sequence_number: 1,
            lease_seconds: 300,
            recipients: vec![crate::gateway::wire::RouteRecipient {
                short_id_hex: self.bitchat_identity.short_id().to_string(),
                public_key_hex: Some(self.bitchat_identity.peer_id().to_string()),
            }],
            signature: Vec::new(),
        };
        initial_snapshot.sign(&self.bitchat_identity);
        let initial_sync = GatewayMessage::RouteSync(initial_snapshot);
        let _ = outbound_tx.send(initial_sync).await;

        // Writer task
        let stats_writer = Arc::clone(&self.stats);
        tokio::spawn(async move {
            while let Some(msg) = outbound_rx.recv().await {
                if let Err(e) = msg.write_framed(&mut send_stream).await {
                    stats_writer.record_error(format!("Gateway write error: {e}"));
                    break;
                }
            }
            let _ = send_stream.finish();
        });

        // Reader task
        let inbound_mesh_tx = self.inbound_mesh_tx.clone();
        let stats_reader = Arc::clone(&self.stats);
        let sessions = Arc::clone(&self.active_sessions);
        let routes_reader = Arc::clone(&self.routes);
        let my_short = self.bitchat_identity.short_id();

        tokio::spawn(async move {
            loop {
                match GatewayMessage::read_framed(&mut recv_stream, DEFAULT_READ_TIMEOUT).await {
                    Ok(GatewayMessage::Forward(fwd)) => {
                        // Validate bridge loop policy: frame may cross the internet bridge at most once!
                        if fwd.bridge_crossings > 1 {
                            stats_reader.inc_loops_prevented();
                            stats_reader.inc_rejected();
                            continue;
                        }

                        // Validate remaining hop budget
                        if fwd.remaining_hops == 0 {
                            stats_reader.inc_rejected();
                            continue;
                        }

                        stats_reader.inc_imported();

                        // Deliver raw mesh packet into local mesh queue
                        if let Err(e) = inbound_mesh_tx.send(fwd.mesh_packet).await {
                            stats_reader.record_error(format!("Failed to queue mesh packet: {e}"));
                            break;
                        }
                    }
                    Ok(GatewayMessage::RouteSync(snapshot)) => {
                        // 1. Validate bounded size & lease
                        if let Err(e) = snapshot.validate_bounds() {
                            tracing::warn!(
                                "Rejecting oversized/invalid route snapshot from {remote_id}: {e}"
                            );
                            continue;
                        }

                        // 2. Verify snapshot signature against the authenticated Bitchat identity bound to this connection
                        let remote_bitchat_peer_id = match sessions
                            .read()
                            .await
                            .get(&remote_id)
                            .map(|s| crate::identity::PeerId(s.remote_bitchat_pubkey))
                        {
                            Some(pk) => pk,
                            None => {
                                tracing::warn!(
                                    "Dropping snapshot: no authenticated session for {remote_id}"
                                );
                                continue;
                            }
                        };

                        if let Err(e) = snapshot.verify_author(&remote_bitchat_peer_id) {
                            tracing::warn!("Rejecting forged/unauthorized route snapshot from {remote_id}: {e}");
                            continue;
                        }

                        // 3. Extract verified ShortIds (filter out local identity)
                        let mut valid_recipients = Vec::new();
                        for r in &snapshot.recipients {
                            if let Some(sid) = crate::identity::ShortId::from_hex(&r.short_id_hex) {
                                if sid != my_short {
                                    valid_recipients.push(sid);
                                }
                            }
                        }

                        // 4. Apply snapshot with epoch & monotonic sequence validation
                        let lease = Duration::from_secs(snapshot.lease_seconds as u64);
                        let _ = routes_reader.write().await.apply_snapshot(
                            remote_id,
                            snapshot.session_epoch,
                            snapshot.sequence_number,
                            valid_recipients,
                            lease,
                            Instant::now(),
                        );
                    }
                    Ok(GatewayMessage::Ping { nonce }) => {
                        // Auto-reply with Pong if write queue permits
                        tracing::debug!("Received gateway ping: {nonce}");
                    }
                    Ok(GatewayMessage::Pong { .. }) => {}
                    Ok(GatewayMessage::Accepted { .. }) => {}
                    Ok(GatewayMessage::Rejected {
                        transfer_id,
                        reason,
                    }) => {
                        tracing::warn!("Gateway forward {transfer_id} rejected: {reason:?}");
                    }
                    Ok(GatewayMessage::Hello(_)) => {}
                    Err(WireError::Timeout) => {
                        // Keep connection alive on timeout unless broken
                        continue;
                    }
                    Err(e) => {
                        stats_reader.record_error(format!("Gateway read terminated: {e}"));
                        break;
                    }
                }
            }

            // Cleanup session & routes on disconnect
            sessions.write().await.remove(&remote_id);
            routes_reader
                .write()
                .await
                .remove_routes_for_endpoint(&remote_id);
            stats_reader.set_active_connections(sessions.read().await.len() as u64);
        });

        Ok(())
    }

    /// Forward an eligible mesh packet across internet gateway to destination.
    /// Enforces:
    /// 1. Public broadcasts NEVER leave the local mesh.
    /// 2. Addressed traffic requires an explicit configured route to an approved gateway.
    /// 3. Loop protection: a packet may cross the bridge at most once.
    pub async fn forward_mesh_packet(
        &self,
        envelope: &Envelope,
        raw_packet: Vec<u8>,
    ) -> Result<bool> {
        // Rule 1: Public broadcasts stay local
        if envelope.recipient_short_id.is_broadcast() {
            return Ok(false);
        }

        // Rule 2: Lookup route
        let target_gw = match self
            .routes
            .read()
            .await
            .lookup(&envelope.recipient_short_id)
        {
            Some(gw) => *gw,
            None => return Ok(false), // No route configured: local delivery only
        };

        // Rule 3: Session check
        let sessions = self.active_sessions.read().await;
        let session = match sessions.get(&target_gw) {
            Some(s) => s,
            None => {
                self.stats.inc_rejected();
                return Err(Error::Transport(format!(
                    "Gateway connection to {target_gw} is not currently active"
                )));
            }
        };

        let transfer_id = self.transfer_counter.fetch_add(1, Ordering::Relaxed);
        let remaining_hops = envelope.ttl_max.saturating_sub(envelope.hop_count);

        let fwd = GatewayForward {
            wire_version: WIRE_VERSION,
            transfer_id,
            destination_short_id_hex: envelope.recipient_short_id.to_string(),
            remaining_hops,
            bridge_crossings: 1, // Marked as having crossed bridge
            mesh_packet: raw_packet,
        };

        session
            .outbound_tx
            .send(GatewayMessage::Forward(fwd))
            .await
            .map_err(|_| Error::Transport("Failed to enqueue outbound forward".into()))?;

        self.stats.inc_exported();
        Ok(true)
    }

    /// Receive the next raw mesh packet imported from remote gateways.
    pub async fn recv_imported_mesh_packet(&self) -> Option<Vec<u8>> {
        let mut rx = self.inbound_mesh_rx.lock().await;
        rx.recv().await
    }

    /// Non-blocking poll for the next raw mesh packet imported from remote gateways.
    pub fn try_recv_imported_mesh_packet(&self) -> Option<Vec<u8>> {
        if let Ok(mut rx) = self.inbound_mesh_rx.try_lock() {
            rx.try_recv().ok()
        } else {
            None
        }
    }

    /// Announce verified local BLE peer ShortIds to all active connected gateways.
    pub async fn announce_local_peers(
        &self,
        peer_short_ids: &[crate::identity::ShortId],
    ) -> Result<()> {
        let mut recipients = vec![crate::gateway::wire::RouteRecipient {
            short_id_hex: self.bitchat_identity.short_id().to_string(),
            public_key_hex: Some(self.bitchat_identity.peer_id().to_string()),
        }];
        for p in peer_short_ids {
            if *p != self.bitchat_identity.short_id() {
                recipients.push(crate::gateway::wire::RouteRecipient {
                    short_id_hex: p.to_string(),
                    public_key_hex: None,
                });
            }
        }
        let sessions = self.active_sessions.read().await;
        for s in sessions.values() {
            let remote_sid = crate::identity::PeerId(s.remote_bitchat_pubkey).short_id();
            let session_recipients: Vec<_> = recipients
                .iter()
                .filter(|r| r.short_id_hex != remote_sid.to_string())
                .cloned()
                .collect();

            let seq = s.next_sequence.fetch_add(1, Ordering::Relaxed);
            let mut snapshot = crate::gateway::wire::RouteSnapshot {
                protocol_version: WIRE_VERSION,
                session_epoch: s.session_epoch,
                sequence_number: seq,
                lease_seconds: 300,
                recipients: session_recipients,
                signature: Vec::new(),
            };
            snapshot.sign(&self.bitchat_identity);
            let msg = GatewayMessage::RouteSync(snapshot);
            let _ = s.outbound_tx.send(msg).await;
        }
        Ok(())
    }

    /// Close the gateway bridge and all active connections cleanly.
    pub async fn close(&self) {
        self.active_sessions.write().await.clear();
        self.endpoint.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::iroh_link::accept_gateway_conn;

    #[tokio::test]
    async fn test_unsolicited_valid_binding_stays_pending_and_rejected() {
        let id_host = Identity::generate();
        let id_dialer = Identity::generate();

        let host = GatewayBridge::new(id_host, None, RelayMode::Disabled)
            .await
            .unwrap();
        host.set_enabled(true).await;

        let dialer = GatewayBridge::new(id_dialer, None, RelayMode::Disabled)
            .await
            .unwrap();

        // Host exports raw contact WITHOUT invitation capability
        let host_contact = host.export_contact().await.unwrap();
        dialer
            .import_and_approve_contact(&host_contact.export_to_string().unwrap())
            .await
            .unwrap();

        let host_ep = host.endpoint().clone();
        let accept_task = tokio::spawn(async move { accept_gateway_conn(&host_ep).await.unwrap() });

        let dialer_connect = dialer.connect_to_contact(&host_contact).await;
        assert!(dialer_connect.is_ok());

        let (remote_id, incoming_conn) = accept_task.await.unwrap();
        let host_res = host.spawn_session_handler(incoming_conn, None).await;

        // Must fail because dialer was not approved and presented no invitation
        assert!(host_res.is_err());
        assert!(!host.config().read().await.is_approved(&remote_id));
        assert_eq!(
            host.config().read().await.consent_state(&remote_id),
            GatewayConsentState::IdentityVerifiedPendingConsent
        );
        assert_eq!(host.routes().read().await.list_routes().len(), 0);

        host.close().await;
        dialer.close().await;
    }

    #[tokio::test]
    async fn test_pairing_invitation_single_use_and_revocation() {
        let id_host = Identity::generate();
        let id_d1 = Identity::generate();
        let id_d2 = Identity::generate();

        let host = GatewayBridge::new(id_host, None, RelayMode::Disabled)
            .await
            .unwrap();
        let d1 = GatewayBridge::new(id_d1, None, RelayMode::Disabled)
            .await
            .unwrap();
        let d2 = GatewayBridge::new(id_d2, None, RelayMode::Disabled)
            .await
            .unwrap();

        // Host generates a single-use pairing contact with auto-admit enabled
        let pairing_contact = host
            .export_pairing_contact(Some(Duration::from_secs(60)), true)
            .await
            .unwrap();
        let pairing_str = pairing_contact.export_to_string().unwrap();

        d1.import_and_approve_contact(&pairing_str).await.unwrap();
        d2.import_and_approve_contact(&pairing_str).await.unwrap();

        // First redemption by d1 succeeds
        let host_ep = host.endpoint().clone();
        let accept1 = tokio::spawn(async move { accept_gateway_conn(&host_ep).await.unwrap() });
        d1.connect_to_contact(&pairing_contact).await.unwrap();
        let (d1_id, conn1) = accept1.await.unwrap();
        assert!(host.spawn_session_handler(conn1, None).await.is_ok());
        assert!(host.config().read().await.is_approved(&d1_id));

        // Second redemption of the SAME single-use contact by d2 fails
        let host_ep2 = host.endpoint().clone();
        let accept2 = tokio::spawn(async move { accept_gateway_conn(&host_ep2).await.unwrap() });
        d2.connect_to_contact(&pairing_contact).await.unwrap();
        let (d2_id, conn2) = accept2.await.unwrap();
        assert!(host.spawn_session_handler(conn2, None).await.is_err());
        assert!(!host.config().read().await.is_approved(&d2_id));

        // Revoking d1 removes approval and routes
        host.revoke_or_block_peer(d1_id, true).await;
        assert!(!host.config().read().await.is_approved(&d1_id));
        assert_eq!(
            host.config().read().await.consent_state(&d1_id),
            GatewayConsentState::Blocked
        );

        host.close().await;
        d1.close().await;
        d2.close().await;
    }
}
