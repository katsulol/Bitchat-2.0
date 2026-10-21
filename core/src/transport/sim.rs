use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use super::Transport;
use crate::error::{Error, Result};
use crate::identity::ShortId;

pub type Frame = (ShortId, Vec<u8>);
pub type InboxReceiver = Arc<tokio::sync::Mutex<UnboundedReceiver<Frame>>>;

/// Configuration for a directed link between two simulated nodes.
#[derive(Clone, Debug)]
pub struct LinkConfig {
    pub latency: Duration,
    pub loss_rate: f64, // 0.0 = no loss, 1.0 = 100% loss
}

impl Default for LinkConfig {
    fn default() -> Self {
        Self {
            latency: Duration::from_millis(0),
            loss_rate: 0.0,
        }
    }
}

struct NetworkState {
    // Adjacency map: (From, To) -> LinkConfig
    links: HashMap<(ShortId, ShortId), LinkConfig>,
    // Node inboxes
    inboxes: HashMap<ShortId, UnboundedSender<(ShortId, Vec<u8>)>>,
    // Seeded PRNG for deterministic loss/latency evaluation
    rng: StdRng,
}

/// Simulated in-memory network topology stand-in for BLE radios.
#[derive(Clone)]
pub struct SimNetwork {
    inner: Arc<Mutex<NetworkState>>,
}

impl SimNetwork {
    /// Create a new simulated network with a deterministic seed.
    pub fn new(seed: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(NetworkState {
                links: HashMap::new(),
                inboxes: HashMap::new(),
                rng: StdRng::seed_from_u64(seed),
            })),
        }
    }

    /// Register a new node in the network and return its SimTransport handle.
    pub fn create_node(&self, id: ShortId) -> SimTransport {
        let (tx, rx) = unbounded_channel();
        if let Ok(mut state) = self.inner.lock() {
            state.inboxes.insert(id, tx);
        }
        SimTransport {
            id,
            network: self.clone(),
            rx: Arc::new(tokio::sync::Mutex::new(rx)),
        }
    }

    /// Remove a node and drop its connections.
    pub fn remove_node(&self, id: ShortId) {
        if let Ok(mut state) = self.inner.lock() {
            state.inboxes.remove(&id);
            state.links.retain(|(from, to), _| *from != id && *to != id);
        }
    }

    /// Configure a directed link from -> to.
    pub fn set_directed_link(&self, from: ShortId, to: ShortId, config: LinkConfig) {
        if let Ok(mut state) = self.inner.lock() {
            state.links.insert((from, to), config);
        }
    }

    /// Configure a bidirectional link between two nodes.
    pub fn set_bidirectional_link(
        &self,
        a: ShortId,
        b: ShortId,
        latency: Duration,
        loss_rate: f64,
    ) {
        let config = LinkConfig { latency, loss_rate };
        self.set_directed_link(a, b, config.clone());
        self.set_directed_link(b, a, config);
    }

    /// Remove a link between two nodes (out of range).
    pub fn remove_link(&self, from: ShortId, to: ShortId) {
        if let Ok(mut state) = self.inner.lock() {
            state.links.remove(&(from, to));
        }
    }

    /// Remove bidirectional link.
    pub fn disconnect(&self, a: ShortId, b: ShortId) {
        self.remove_link(a, b);
        self.remove_link(b, a);
    }

    /// Query current neighbors of a node.
    pub fn neighbors_of(&self, node: ShortId) -> Vec<ShortId> {
        if let Ok(state) = self.inner.lock() {
            state
                .links
                .keys()
                .filter(|(from, _)| *from == node)
                .map(|(_, to)| *to)
                .collect()
        } else {
            Vec::new()
        }
    }
}

/// Node transport handle bound to a SimNetwork.
pub struct SimTransport {
    id: ShortId,
    network: SimNetwork,
    rx: InboxReceiver,
}

impl Transport for SimTransport {
    async fn send(&self, to: ShortId, frame: Vec<u8>) -> Result<()> {
        let (tx, config, should_drop) = {
            let mut state = self
                .network
                .inner
                .lock()
                .map_err(|_| Error::Transport("SimNetwork lock poisoned".into()))?;
            let config = state.links.get(&(self.id, to)).cloned();
            let tx = state.inboxes.get(&to).cloned();

            let should_drop = if let Some(ref cfg) = config {
                if cfg.loss_rate >= 1.0 {
                    true
                } else if cfg.loss_rate <= 0.0 {
                    false
                } else {
                    state.rng.gen_bool(cfg.loss_rate)
                }
            } else {
                true // No link -> dropped
            };

            (tx, config, should_drop)
        };

        // If not in range or dropped by simulation RNG, quietly drop (wireless radio semantics)
        if should_drop {
            return Ok(());
        }

        if let (Some(tx), Some(cfg)) = (tx, config) {
            if cfg.latency > Duration::ZERO {
                tokio::time::sleep(cfg.latency).await;
            }
            let _ = tx.send((self.id, frame));
        }

        Ok(())
    }

    async fn broadcast(&self, frame: Vec<u8>) -> Result<()> {
        let deliveries = {
            let mut state = self
                .network
                .inner
                .lock()
                .map_err(|_| Error::Transport("SimNetwork lock poisoned".into()))?;
            let neighbors: Vec<(ShortId, LinkConfig)> = state
                .links
                .iter()
                .filter(|((from, _), _)| *from == self.id)
                .map(|((_, to), cfg)| (*to, cfg.clone()))
                .collect();

            let mut out = Vec::new();
            for (neighbor, cfg) in neighbors {
                let should_drop = if cfg.loss_rate >= 1.0 {
                    true
                } else if cfg.loss_rate <= 0.0 {
                    false
                } else {
                    state.rng.gen_bool(cfg.loss_rate)
                };

                if !should_drop {
                    if let Some(tx) = state.inboxes.get(&neighbor).cloned() {
                        out.push((tx, cfg.latency));
                    }
                }
            }
            out
        };

        for (tx, latency) in deliveries {
            let frame_clone = frame.clone();
            let sender_id = self.id;
            if latency > Duration::ZERO {
                tokio::spawn(async move {
                    tokio::time::sleep(latency).await;
                    let _ = tx.send((sender_id, frame_clone));
                });
            } else {
                let _ = tx.send((sender_id, frame_clone));
            }
        }

        Ok(())
    }

    async fn recv(&mut self) -> Option<(ShortId, Vec<u8>)> {
        let mut rx = self.rx.lock().await;
        rx.recv().await
    }

    fn neighbors(&self) -> Vec<ShortId> {
        self.network.neighbors_of(self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_two_nodes_exchange_frames() {
        let net = SimNetwork::new(42);
        let id_a = ShortId([1; 8]);
        let id_b = ShortId([2; 8]);

        let mut node_a = net.create_node(id_a);
        let mut node_b = net.create_node(id_b);

        net.set_bidirectional_link(id_a, id_b, Duration::from_millis(0), 0.0);

        // A -> B
        node_a.send(id_b, vec![10, 20]).await.unwrap();
        let (from, frame) = node_b.recv().await.unwrap();
        assert_eq!(from, id_a);
        assert_eq!(frame, vec![10, 20]);

        // B -> A
        node_b.send(id_a, vec![30, 40]).await.unwrap();
        let (from, frame) = node_a.recv().await.unwrap();
        assert_eq!(from, id_b);
        assert_eq!(frame, vec![30, 40]);
    }

    #[tokio::test]
    async fn test_three_nodes_line_topology_isolation() {
        // Line topology: A <-> B <-> C. No direct A <-> C link.
        let net = SimNetwork::new(100);
        let id_a = ShortId([1; 8]);
        let id_b = ShortId([2; 8]);
        let id_c = ShortId([3; 8]);

        let node_a = net.create_node(id_a);
        let mut node_b = net.create_node(id_b);
        let mut node_c = net.create_node(id_c);

        net.set_bidirectional_link(id_a, id_b, Duration::from_millis(0), 0.0);
        net.set_bidirectional_link(id_b, id_c, Duration::from_millis(0), 0.0);

        // A broadcasts
        node_a.broadcast(vec![99]).await.unwrap();

        // B must receive it
        let (from, frame) = node_b.recv().await.unwrap();
        assert_eq!(from, id_a);
        assert_eq!(frame, vec![99]);

        // C must NOT receive it (out of range, no direct link, no relaying in Phase 1)
        let c_recv = tokio::time::timeout(Duration::from_millis(20), node_c.recv()).await;
        assert!(
            c_recv.is_err(),
            "C should not receive A's broadcast directly"
        );
    }

    #[tokio::test]
    async fn test_link_removal_stops_delivery() {
        let net = SimNetwork::new(200);
        let id_a = ShortId([1; 8]);
        let id_b = ShortId([2; 8]);

        let node_a = net.create_node(id_a);
        let mut node_b = net.create_node(id_b);

        net.set_bidirectional_link(id_a, id_b, Duration::from_millis(0), 0.0);
        assert_eq!(node_a.neighbors(), vec![id_b]);

        // Break link (nodes move away)
        net.disconnect(id_a, id_b);
        assert!(node_a.neighbors().is_empty());

        node_a.send(id_b, vec![1, 2, 3]).await.unwrap();

        let recv_res = tokio::time::timeout(Duration::from_millis(20), node_b.recv()).await;
        assert!(recv_res.is_err(), "No frame should arrive after disconnect");
    }

    #[tokio::test]
    async fn test_seeded_loss_is_deterministic() {
        // Run test with fixed seed twice; count delivered packets under 50% loss
        async fn run_sim(seed: u64) -> Vec<u8> {
            let net = SimNetwork::new(seed);
            let id_a = ShortId([1; 8]);
            let id_b = ShortId([2; 8]);

            let node_a = net.create_node(id_a);
            let mut node_b = net.create_node(id_b);

            net.set_directed_link(
                id_a,
                id_b,
                LinkConfig {
                    latency: Duration::ZERO,
                    loss_rate: 0.5,
                },
            );

            let mut delivered = Vec::new();
            for i in 0..50 {
                node_a.send(id_b, vec![i as u8]).await.unwrap();
                if let Ok(Some((_, f))) =
                    tokio::time::timeout(Duration::from_millis(10), node_b.recv()).await
                {
                    delivered.push(f[0]);
                }
            }
            delivered
        }

        let run1 = run_sim(12345).await;
        let run2 = run_sim(12345).await;

        assert_eq!(
            run1, run2,
            "Identical seeds must yield identical packet delivery outcomes"
        );
        assert!(!run1.is_empty(), "Some packets must have delivered");
        assert!(
            run1.len() < 50,
            "Some packets must have been dropped at 50% loss"
        );
    }
}
