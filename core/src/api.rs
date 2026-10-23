use std::path::PathBuf;
use std::sync::OnceLock;
use tokio::sync::RwLock;

use crate::envelope::MsgType;
use crate::identity::{FileKeyStore, Identity, KeyStore, ShortId};
use crate::mesh::peer::PeerSessionManager;
use crate::mesh::policy::AdaptiveFlood;
use crate::mesh::relay::MeshNode;
use crate::transport::bridge::{BridgeHandle, BridgeTransport};

// ponytail: flutter_rust_bridge API surface wrapping core identity, mesh routing, and BLE bridge transport.

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
    let mut lock = get_service().write().await;
    if let Some(svc) = lock.as_ref() {
        return Ok(svc.node.identity().short_id().to_string());
    }

    let path = PathBuf::from(storage_path).join("identity.key");
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
    let (transport, handle) = BridgeTransport::new(short_id);
    let node = MeshNode::new(identity, transport, Box::new(AdaptiveFlood::default()));

    let service = BitchatCoreService {
        node,
        handle,
        peer_mgr: PeerSessionManager::new(),
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

    // If an E2E session is established, encrypt payload
    if let Some(session) = svc.peer_mgr.get_session(&target) {
        if session.is_handshake_finished() {
            let msg_id = svc
                .peer_mgr
                .send_encrypted(&mut svc.node, target, text.as_bytes())
                .await
                .map_err(|e| format!("E2E encrypted send failed: {e}"))?;
            return Ok(hex::encode(msg_id));
        }
    }

    // Fail closed: Do NOT silently send plaintext
    Err("Secure E2E session not established with peer. Initiate handshake first.".to_string())
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
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    svc.handle.on_peer_connected(peer);
    Ok(())
}

/// Notify Rust that a BLE peer disconnected.
pub async fn on_peer_disconnected(short_id: String) -> Result<(), String> {
    let peer = hex_to_short_id(&short_id)?;
    let lock = get_service().read().await;
    let svc = lock.as_ref().ok_or("Service not initialized")?;
    svc.handle.on_peer_disconnected(peer);
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

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
