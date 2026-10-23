use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

use crate::envelope::Envelope;
use crate::error::{Error, Result};
use crate::gateway::config::GatewayConfig;
use crate::gateway::contact::GatewayContact;
use crate::gateway::identity_binding::IdentityBinding;
use crate::gateway::iroh_link::{create_endpoint, dial_gateway};
use crate::gateway::routes::GatewayRouteTable;
use crate::gateway::stats::GatewayStats;
use crate::gateway::wire::{
    GatewayForward, GatewayHello, GatewayMessage, WireError, DEFAULT_READ_TIMEOUT, WIRE_VERSION,
};
use crate::identity::Identity;
use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointId, RelayMode, SecretKey};

/// State of an active gateway transport session.
pub struct ActiveGatewaySession {
    pub remote_endpoint_id: EndpointId,
    pub remote_bitchat_pubkey: [u8; 32],
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

    /// Import and approve a remote gateway contact document.
    pub async fn import_and_approve_contact(&self, contact_str: &str) -> Result<GatewayContact> {
        let contact = GatewayContact::import_from_str(contact_str)
            .map_err(|e| Error::Transport(format!("Failed to import contact: {e}")))?;

        let id = contact
            .endpoint_id()
            .map_err(|e| Error::Transport(format!("Invalid endpoint id: {e}")))?;

        self.config.write().await.approve_gateway(id);
        Ok(contact)
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

        // Check approval whitelist
        if !self.config.read().await.is_approved(&remote_id) {
            self.stats.inc_rejected();
            return Err(Error::Transport(format!(
                "Incoming gateway {remote_id} is not in approval whitelist"
            )));
        }

        let (mut send_stream, mut recv_stream) = match contact_hint {
            Some(_) => {
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
                });

                hello
                    .write_framed(&mut s)
                    .await
                    .map_err(|e| Error::Transport(format!("Failed to send Hello: {e}")))?;

                (s, r)
            }
            None => {
                // Remote dialed: accept bi stream and read Hello
                let (s, mut r) = conn
                    .accept_bi()
                    .await
                    .map_err(|e| Error::Transport(format!("Failed to accept bi stream: {e}")))?;

                let hello_msg = GatewayMessage::read_framed(&mut r, DEFAULT_READ_TIMEOUT)
                    .await
                    .map_err(|e| Error::Transport(format!("Failed to read Hello: {e}")))?;

                if let GatewayMessage::Hello(hello) = hello_msg {
                    hello.binding.verify(&remote_id).map_err(|e| {
                        Error::Transport(format!("Binding verification failed: {e}"))
                    })?;
                } else {
                    return Err(Error::Transport(
                        "Expected Hello as first gateway frame".into(),
                    ));
                }

                (s, r)
            }
        };

        let (outbound_tx, mut outbound_rx) = mpsc::channel::<GatewayMessage>(64);

        let session = ActiveGatewaySession {
            remote_endpoint_id: remote_id,
            remote_bitchat_pubkey: [0u8; 32], // Bound in identity statement
            outbound_tx,
        };

        self.active_sessions
            .write()
            .await
            .insert(remote_id, session);
        self.stats
            .set_active_connections(self.active_sessions.read().await.len() as u64);

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

            // Cleanup session on disconnect
            sessions.write().await.remove(&remote_id);
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

    /// Close the gateway bridge and all active connections cleanly.
    pub async fn close(&self) {
        self.active_sessions.write().await.clear();
        self.endpoint.close().await;
    }
}
