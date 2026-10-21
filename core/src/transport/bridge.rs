use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::error::{Error, Result};
use crate::identity::ShortId;
use crate::transport::Transport;

// ponytail: memory-bridged Transport adapter decoupling Rust mesh logic from mobile BLE platform channels.

/// An outbound frame emitted by Rust mesh logic to be transmitted over BLE.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboundFrame {
    /// `None` indicates radio broadcast to all connected BLE peripherals/centrals.
    /// `Some(peer)` indicates direct unicast GATT write to a specific connected peer.
    pub to: Option<ShortId>,
    pub bytes: Vec<u8>,
}

struct BridgeShared {
    neighbors: RwLock<HashSet<ShortId>>,
    outbound_tx: UnboundedSender<OutboundFrame>,
    inbound_tx: UnboundedSender<(ShortId, Vec<u8>)>,
}

/// Host/Dart-side controller handle for injecting BLE radio events into Rust.
#[derive(Clone)]
pub struct BridgeHandle {
    shared: Arc<BridgeShared>,
}

impl BridgeHandle {
    /// Notify Rust that a new BLE peer has connected and completed GATT handshake.
    pub fn on_peer_connected(&self, peer: ShortId) {
        if let Ok(mut lock) = self.shared.neighbors.write() {
            lock.insert(peer);
        }
    }

    /// Notify Rust that a BLE peer disconnected.
    pub fn on_peer_disconnected(&self, peer: ShortId) {
        if let Ok(mut lock) = self.shared.neighbors.write() {
            lock.remove(&peer);
        }
    }

    /// Deliver an incoming BLE characteristic payload into Rust mesh input queue.
    pub fn on_frame_received(&self, from: ShortId, bytes: Vec<u8>) -> Result<()> {
        self.shared
            .inbound_tx
            .send((from, bytes))
            .map_err(|_| Error::Transport("Inbound bridge channel closed".into()))
    }

    /// Query the currently connected BLE peers.
    pub fn connected_peers(&self) -> Vec<ShortId> {
        self.shared
            .neighbors
            .read()
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    }
}

pub type InboundRx = Arc<tokio::sync::Mutex<UnboundedReceiver<(ShortId, Vec<u8>)>>>;
pub type OutboundRx = Arc<tokio::sync::Mutex<UnboundedReceiver<OutboundFrame>>>;

/// Radio-agnostic `Transport` implementation connecting `MeshNode` to Flutter / Android GATT layer.
pub struct BridgeTransport {
    id: ShortId,
    shared: Arc<BridgeShared>,
    inbound_rx: InboundRx,
    outbound_rx: OutboundRx,
}

impl BridgeTransport {
    /// Create a new paired `BridgeTransport` and `BridgeHandle`.
    pub fn new(id: ShortId) -> (Self, BridgeHandle) {
        let (inbound_tx, inbound_rx) = unbounded_channel();
        let (outbound_tx, outbound_rx) = unbounded_channel();

        let shared = Arc::new(BridgeShared {
            neighbors: RwLock::new(HashSet::new()),
            outbound_tx,
            inbound_tx,
        });

        let handle = BridgeHandle {
            shared: Arc::clone(&shared),
        };

        let transport = Self {
            id,
            shared,
            inbound_rx: Arc::new(tokio::sync::Mutex::new(inbound_rx)),
            outbound_rx: Arc::new(tokio::sync::Mutex::new(outbound_rx)),
        };

        (transport, handle)
    }

    pub fn id(&self) -> ShortId {
        self.id
    }

    pub fn handle(&self) -> BridgeHandle {
        BridgeHandle {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Receive the next outbound frame that needs to be transmitted by mobile BLE radios.
    pub async fn recv_outbound(&mut self) -> Option<OutboundFrame> {
        let mut rx = self.outbound_rx.lock().await;
        rx.recv().await
    }

    /// Non-blocking check for available outbound frame.
    pub fn try_recv_outbound(&self) -> Option<OutboundFrame> {
        if let Ok(mut rx) = self.outbound_rx.try_lock() {
            rx.try_recv().ok()
        } else {
            None
        }
    }

    /// Non-blocking check for available inbound frame.
    pub fn try_recv(&self) -> Option<(ShortId, Vec<u8>)> {
        if let Ok(mut rx) = self.inbound_rx.try_lock() {
            rx.try_recv().ok()
        } else {
            None
        }
    }
}

impl Transport for BridgeTransport {
    async fn send(&self, to: ShortId, frame: Vec<u8>) -> Result<()> {
        self.shared
            .outbound_tx
            .send(OutboundFrame {
                to: Some(to),
                bytes: frame,
            })
            .map_err(|_| Error::Transport("Outbound bridge channel closed".into()))
    }

    async fn broadcast(&self, frame: Vec<u8>) -> Result<()> {
        self.shared
            .outbound_tx
            .send(OutboundFrame {
                to: None,
                bytes: frame,
            })
            .map_err(|_| Error::Transport("Outbound bridge channel closed".into()))
    }

    async fn recv(&mut self) -> Option<(ShortId, Vec<u8>)> {
        let mut rx = self.inbound_rx.lock().await;
        rx.recv().await
    }

    fn neighbors(&self) -> Vec<ShortId> {
        self.shared
            .neighbors
            .read()
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use crate::mesh::policy::NaiveFlood;
    use crate::mesh::relay::MeshNode;
    use std::time::Duration;

    #[tokio::test]
    async fn test_bridge_transport_inbound_and_outbound() {
        let id = ShortId([1; 8]);
        let (mut transport, handle) = BridgeTransport::new(id);

        let peer = ShortId([2; 8]);
        handle.on_peer_connected(peer);
        assert_eq!(transport.neighbors(), vec![peer]);

        // Inject incoming frame from BLE via handle
        let test_payload = b"inbound_ble_frame".to_vec();
        handle
            .on_frame_received(peer, test_payload.clone())
            .unwrap();

        // Read from transport
        let (from, received) = transport.recv().await.unwrap();
        assert_eq!(from, peer);
        assert_eq!(received, test_payload);

        // Send broadcast from Rust core
        let out_payload = b"outbound_mesh_broadcast".to_vec();
        transport.broadcast(out_payload.clone()).await.unwrap();

        // Read outbound frame from handle
        let outbound = transport.recv_outbound().await.unwrap();
        assert_eq!(outbound.to, None);
        assert_eq!(outbound.bytes, out_payload);

        // Send addressed unicast
        transport
            .send(peer, b"direct_write".to_vec())
            .await
            .unwrap();
        let unicast_out = transport.recv_outbound().await.unwrap();
        assert_eq!(unicast_out.to, Some(peer));
        assert_eq!(unicast_out.bytes, b"direct_write");

        // Disconnect peer
        handle.on_peer_disconnected(peer);
        assert!(transport.neighbors().is_empty());
    }

    #[tokio::test]
    async fn test_mesh_node_on_bridge_transport() {
        tokio::time::pause();

        let id = Identity::generate();
        let (transport, handle) = BridgeTransport::new(id.short_id());
        let mut node = MeshNode::new(id, transport, Box::new(NaiveFlood));

        // Connect a neighbor via bridge handle
        let neighbor = ShortId([99; 8]);
        handle.on_peer_connected(neighbor);
        assert_eq!(node.transport().neighbors(), vec![neighbor]);

        // Broadcast from node -> produces OutboundFrame on bridge
        let msg_id = node
            .send_broadcast(b"Bridge broadcast".to_vec())
            .await
            .unwrap();
        assert_ne!(msg_id, [0u8; 8]);

        let outbound = node.transport_mut().recv_outbound().await.unwrap();
        assert_eq!(outbound.to, None);
        assert!(!outbound.bytes.is_empty());

        // Now simulate an inbound frame arriving from BLE
        let incoming_payload = outbound.bytes; // Loopback/simulate wire envelope
        handle
            .on_frame_received(neighbor, incoming_payload)
            .unwrap();

        // Step node
        let step_res = tokio::time::timeout(Duration::from_millis(5), node.step()).await;
        assert!(step_res.is_ok());
    }
}
