use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::envelope::{compute_msg_id, Envelope, Flags, MsgType, CURRENT_VERSION, MAX_HOPS};
use crate::error::Result;
use crate::identity::{Identity, ShortId};
use crate::mesh::fragment::{Fragmenter, Reassembler};
use crate::mesh::policy::{RelayContext, RelayPolicy};
use crate::mesh::seen::{LruSeenCache, SeenCache};
use crate::mesh::stats::{MeshStats, MeshStatsSnapshot};
use crate::mesh::MsgId;
use crate::transport::Transport;

// ponytail: mesh node routing engine with pluggable relay policy, seen-cache deduplication, and link-layer fragmentation.

pub const DEFAULT_MESH_MTU: usize = 512;
pub const DEFAULT_SEEN_CACHE_CAPACITY: usize = 1000;
pub const DEFAULT_SEEN_CACHE_TTL: Duration = Duration::from_secs(120);

/// An inbound envelope delivered to local application layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundMessage {
    pub origin_sender: ShortId,
    pub recipient: ShortId,
    pub msg_type: MsgType,
    pub flags: Flags,
    pub msg_id: MsgId,
    pub hop_count: u8,
    pub payload: Vec<u8>,
}

/// Tracking metadata for a pending relay waiting for its scheduled transmission time.
struct PendingRelay {
    envelope: Envelope,
    fire_at: Instant,
    suppress_after: u8,
    duplicates_heard: u32,
}

/// Decentralized mesh node coordinating identity, transport, deduplication, and forwarding.
pub struct MeshNode<T: Transport> {
    identity: Identity,
    transport: T,
    seen_cache: Box<dyn SeenCache>,
    reassembler: Reassembler,
    fragmenter: Fragmenter,
    policy: Box<dyn RelayPolicy>,
    stats: Arc<MeshStats>,
    mtu: usize,
    origin_counter: u64,
    pending_relays: HashMap<MsgId, PendingRelay>,
    inbound_queue: VecDeque<InboundMessage>,
}

impl<T: Transport> MeshNode<T> {
    /// Create a new mesh node with default LRU seen-cache and configuration.
    pub fn new(identity: Identity, transport: T, policy: Box<dyn RelayPolicy>) -> Self {
        Self {
            identity,
            transport,
            seen_cache: Box::new(LruSeenCache::new(
                DEFAULT_SEEN_CACHE_CAPACITY,
                DEFAULT_SEEN_CACHE_TTL,
            )),
            reassembler: Reassembler::default(),
            fragmenter: Fragmenter::default(),
            policy,
            stats: Arc::new(MeshStats::new()),
            mtu: DEFAULT_MESH_MTU,
            origin_counter: 0,
            pending_relays: HashMap::new(),
            inbound_queue: VecDeque::new(),
        }
    }

    /// Set custom deduplication cache.
    pub fn with_seen_cache(mut self, cache: Box<dyn SeenCache>) -> Self {
        self.seen_cache = cache;
        self
    }

    /// Set custom link-layer MTU.
    pub fn with_mtu(mut self, mtu: usize) -> Self {
        self.mtu = mtu;
        self
    }

    /// Set relay policy dynamically.
    pub fn set_policy(&mut self, policy: Box<dyn RelayPolicy>) {
        self.policy = policy;
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    pub fn short_id(&self) -> ShortId {
        self.identity.short_id()
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    pub fn stats(&self) -> MeshStatsSnapshot {
        self.stats.snapshot()
    }

    pub fn stats_handle(&self) -> Arc<MeshStats> {
        Arc::clone(&self.stats)
    }

    pub fn recv_inbound(&mut self) -> Option<InboundMessage> {
        self.inbound_queue.pop_front()
    }

    pub fn inbound_count(&self) -> usize {
        self.inbound_queue.len()
    }

    /// Broadcast payload to all peers in the mesh network.
    pub async fn send_broadcast(&mut self, payload: Vec<u8>) -> Result<MsgId> {
        self.send_broadcast_with_type(MsgType::Chat, Flags::default(), payload)
            .await
    }

    /// Broadcast payload with custom message type and header flags.
    pub async fn send_broadcast_with_type(
        &mut self,
        msg_type: MsgType,
        flags: Flags,
        payload: Vec<u8>,
    ) -> Result<MsgId> {
        self.send_envelope(ShortId::default(), msg_type, flags, payload)
            .await
    }

    /// Send addressed message to a specific peer over the mesh.
    pub async fn send_to(&mut self, recipient: ShortId, payload: Vec<u8>) -> Result<MsgId> {
        self.send_to_with_type(recipient, MsgType::Chat, Flags::default(), payload)
            .await
    }

    /// Send addressed message with custom message type and flags.
    pub async fn send_to_with_type(
        &mut self,
        recipient: ShortId,
        msg_type: MsgType,
        flags: Flags,
        payload: Vec<u8>,
    ) -> Result<MsgId> {
        self.send_envelope(recipient, msg_type, flags, payload)
            .await
    }

    async fn send_envelope(
        &mut self,
        recipient: ShortId,
        msg_type: MsgType,
        flags: Flags,
        payload: Vec<u8>,
    ) -> Result<MsgId> {
        self.origin_counter = self.origin_counter.saturating_add(1);
        let msg_id = compute_msg_id(&self.identity.peer_id(), self.origin_counter, &payload);

        let env = Envelope {
            version: CURRENT_VERSION,
            msg_type,
            flags,
            hop_count: 0,
            ttl_max: MAX_HOPS,
            msg_id,
            sender_short_id: self.identity.short_id(),
            recipient_short_id: recipient,
            payload,
        };

        // Insert into seen cache so we never accept our own reflected transmission back
        self.seen_cache.check_and_insert(msg_id, Instant::now());

        let encoded = env.encode()?;
        let fragments = self.fragmenter.split(&encoded, self.mtu)?;

        // Unicast shortcut: if addressed to an immediate neighbor, send directly
        if !recipient.is_broadcast() && self.transport.neighbors().contains(&recipient) {
            for frag in fragments {
                self.transport.send(recipient, frag).await?;
                self.stats.inc_transmissions();
            }
        } else {
            for frag in fragments {
                self.transport.broadcast(frag).await?;
                self.stats.inc_transmissions();
            }
        }

        Ok(msg_id)
    }

    /// Execute a single discrete event step (receive inbound frame or trigger matured relay timer).
    pub async fn step(&mut self) -> Result<bool> {
        self.step_at(Instant::now()).await
    }

    /// Execute a single step with an explicitly supplied clock reference for deterministic testing.
    pub async fn step_at(&mut self, now: Instant) -> Result<bool> {
        // Fire any matured pending relays first
        if self.fire_matured_relays(now).await? {
            return Ok(true);
        }

        let next_deadline = self.pending_relays.values().map(|p| p.fire_at).min();

        if let Some(deadline) = next_deadline {
            let sleep_dur = deadline.saturating_duration_since(now);
            tokio::select! {
                biased;
                _ = tokio::time::sleep(sleep_dur) => {
                    let wake_now = now.checked_add(sleep_dur).unwrap_or(now);
                    self.fire_matured_relays(wake_now).await?;
                    Ok(true)
                }
                recv_res = self.transport.recv() => {
                    match recv_res {
                        Some((sender, frame)) => {
                            self.handle_incoming_frame(sender, frame, now).await?;
                            Ok(true)
                        }
                        None => Ok(false),
                    }
                }
            }
        } else {
            match self.transport.recv().await {
                Some((sender, frame)) => {
                    self.handle_incoming_frame(sender, frame, now).await?;
                    Ok(true)
                }
                None => Ok(false),
            }
        }
    }

    async fn fire_matured_relays(&mut self, now: Instant) -> Result<bool> {
        let matured: Vec<MsgId> = self
            .pending_relays
            .iter()
            .filter(|(_, p)| p.fire_at <= now)
            .map(|(id, _)| *id)
            .collect();

        if matured.is_empty() {
            return Ok(false);
        }

        for id in matured {
            if let Some(pending) = self.pending_relays.remove(&id) {
                if pending.duplicates_heard >= pending.suppress_after as u32 {
                    self.stats.inc_relays_suppressed();
                } else {
                    self.do_relay(pending.envelope).await?;
                }
            }
        }

        Ok(true)
    }

    async fn handle_incoming_frame(
        &mut self,
        from_peer: ShortId,
        frame: Vec<u8>,
        now: Instant,
    ) -> Result<()> {
        // 1. Reassemble link-layer fragments
        let assembled = match self
            .reassembler
            .process_from(Some(from_peer.0), &frame, now)?
        {
            Some(bytes) => bytes,
            None => return Ok(()), // Awaiting more fragments
        };

        // 2. Decode wire envelope
        let envelope = match Envelope::decode(&assembled) {
            Ok(env) => env,
            Err(_) => return Ok(()), // Drop malformed envelope
        };

        // 3. Drop our own origin messages reflected back from neighbors
        if envelope.sender_short_id == self.identity.short_id() {
            return Ok(());
        }

        // 4. Seen cache deduplication
        let already_seen = self.seen_cache.check_and_insert(envelope.msg_id, now);
        if already_seen {
            self.stats.inc_duplicates_dropped();
            if let Some(pending) = self.pending_relays.get_mut(&envelope.msg_id) {
                pending.duplicates_heard = pending.duplicates_heard.saturating_add(1);
                if pending.duplicates_heard >= pending.suppress_after as u32 {
                    self.pending_relays.remove(&envelope.msg_id);
                    self.stats.inc_relays_suppressed();
                }
            }
            return Ok(());
        }

        // 5. Local delivery check (broadcast or addressed to this node)
        let for_us = envelope.recipient_short_id.is_broadcast()
            || envelope.recipient_short_id == self.identity.short_id();

        if for_us {
            self.stats.inc_deliveries();
            self.inbound_queue.push_back(InboundMessage {
                origin_sender: envelope.sender_short_id,
                recipient: envelope.recipient_short_id,
                msg_type: envelope.msg_type,
                flags: envelope.flags,
                msg_id: envelope.msg_id,
                hop_count: envelope.hop_count,
                payload: envelope.payload.clone(),
            });
        }

        // 6. Relay decision
        // If it was a unicast addressed to us, destination reached: do not relay further
        if envelope.recipient_short_id == self.identity.short_id() {
            return Ok(());
        }

        // Check hop limit
        if envelope.hop_count >= envelope.ttl_max {
            self.stats.inc_hop_count_exceeded();
            return Ok(());
        }

        let ctx = RelayContext {
            neighbor_count: self.transport.neighbors().len(),
            hop_count: envelope.hop_count,
            priority: envelope.flags.priority,
            duplicates_heard: 0,
        };

        if !self.policy.should_relay(&ctx) {
            return Ok(());
        }

        let delay = self.policy.delay(&ctx);
        let suppress_after = self.policy.suppress_after(&ctx);

        if delay.is_zero() {
            self.do_relay(envelope).await?;
        } else {
            let fire_at = now.checked_add(delay).unwrap_or(now);
            self.pending_relays.insert(
                envelope.msg_id,
                PendingRelay {
                    envelope,
                    fire_at,
                    suppress_after,
                    duplicates_heard: 0,
                },
            );
        }

        Ok(())
    }

    async fn do_relay(&mut self, mut envelope: Envelope) -> Result<()> {
        if envelope.increment_hop().is_err() {
            self.stats.inc_hop_count_exceeded();
            return Ok(());
        }

        let encoded = envelope.encode()?;
        let fragments = self.fragmenter.split(&encoded, self.mtu)?;

        for frag in fragments {
            self.transport.broadcast(frag).await?;
            self.stats.inc_transmissions();
        }

        Ok(())
    }
}

impl MeshNode<crate::transport::sim::SimTransport> {
    /// Non-blocking discrete step: processes pending relays that matured by `now`
    /// and immediately consumes any available incoming frame from the simulation inbox.
    /// Returns `true` if any event was processed.
    pub async fn poll_step(&mut self, now: Instant) -> Result<bool> {
        if self.fire_matured_relays(now).await? {
            return Ok(true);
        }

        if let Some((sender, frame)) = self.transport.try_recv() {
            self.handle_incoming_frame(sender, frame, now).await?;
            return Ok(true);
        }

        Ok(false)
    }
}

impl MeshNode<crate::transport::bridge::BridgeTransport> {
    /// Non-blocking discrete step for mobile BLE bridge transport: processes pending relays
    /// that matured by `now` and consumes any incoming frame queued from Flutter GATT.
    pub async fn poll_step(&mut self, now: Instant) -> Result<bool> {
        if self.fire_matured_relays(now).await? {
            return Ok(true);
        }

        if let Some((sender, frame)) = self.transport.try_recv() {
            self.handle_incoming_frame(sender, frame, now).await?;
            return Ok(true);
        }

        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::policy::NaiveFlood;
    use crate::transport::sim::SimNetwork;

    #[tokio::test]
    async fn test_abc_line_topology_one_relay() {
        tokio::time::pause();

        let net = SimNetwork::new(101);
        let id_a = Identity::generate();
        let id_b = Identity::generate();
        let id_c = Identity::generate();

        let t_a = net.create_node(id_a.short_id());
        let t_b = net.create_node(id_b.short_id());
        let t_c = net.create_node(id_c.short_id());

        // A <-> B <-> C (A cannot reach C directly)
        net.set_bidirectional_link(id_a.short_id(), id_b.short_id(), Duration::ZERO, 0.0);
        net.set_bidirectional_link(id_b.short_id(), id_c.short_id(), Duration::ZERO, 0.0);

        let mut node_a = MeshNode::new(id_a, t_a, Box::new(NaiveFlood));
        let mut node_b = MeshNode::new(id_b, t_b, Box::new(NaiveFlood));
        let mut node_c = MeshNode::new(id_c, t_c, Box::new(NaiveFlood));

        // A sends broadcast
        let payload = b"Broadcast from A to C".to_vec();
        let msg_id = node_a.send_broadcast(payload.clone()).await.unwrap();

        // Step B: receives from A, delivers locally, relays to C (and A)
        node_b.step().await.unwrap();
        assert_eq!(node_b.inbound_count(), 1);
        let b_msg = node_b.recv_inbound().unwrap();
        assert_eq!(b_msg.msg_id, msg_id);
        assert_eq!(b_msg.hop_count, 0);

        // Step C: receives relay from B, delivers locally, relays back to B
        node_c.step().await.unwrap();
        assert_eq!(node_c.inbound_count(), 1);
        let c_msg = node_c.recv_inbound().unwrap();
        assert_eq!(c_msg.msg_id, msg_id);
        assert_eq!(c_msg.hop_count, 1); // exactly 1 relay hop!
        assert_eq!(c_msg.payload, payload);

        // Step A: receives B's relay -> drops because sender is A
        node_a.step().await.unwrap();
        assert_eq!(node_a.inbound_count(), 0);

        // Step B: receives C's relay -> drops duplicate
        node_b.step().await.unwrap();
        assert_eq!(node_b.stats().duplicates_dropped, 1);

        // Confirm transmissions:
        // A: 1 origin send
        assert_eq!(node_a.stats().transmissions, 1);
        // B: 1 relay
        assert_eq!(node_b.stats().transmissions, 1);
        // C: 1 relay
        assert_eq!(node_c.stats().transmissions, 1);
    }

    #[tokio::test]
    async fn test_ring_topology_terminates() {
        tokio::time::pause();

        let net = SimNetwork::new(202);
        let ids: Vec<Identity> = (0..4).map(|_| Identity::generate()).collect();
        let s_ids: Vec<ShortId> = ids.iter().map(|id| id.short_id()).collect();

        let mut nodes: Vec<MeshNode<_>> = ids
            .into_iter()
            .map(|id| {
                let s_id = id.short_id();
                let t = net.create_node(s_id);
                MeshNode::new(id, t, Box::new(NaiveFlood))
            })
            .collect();

        // 4-node ring: 0-1-2-3-0
        for i in 0..4 {
            let next = (i + 1) % 4;
            net.set_bidirectional_link(s_ids[i], s_ids[next], Duration::ZERO, 0.0);
        }

        // Node 0 broadcasts
        let payload = b"Ring message".to_vec();
        nodes[0].send_broadcast(payload).await.unwrap();

        // Step until no more frames are in-flight in any node's inbox
        for _ in 0..20 {
            for node in nodes.iter_mut() {
                // Drain any pending frames
                while let Ok(Ok(true)) =
                    tokio::time::timeout(Duration::from_millis(1), node.step()).await
                {
                    // stepped
                }
            }
        }

        // Verify: every node 1, 2, 3 received exactly 1 local delivery
        for (i, node) in nodes.iter().enumerate().skip(1) {
            assert_eq!(
                node.stats().deliveries,
                1,
                "Node {i} should receive exactly 1 delivery"
            );
            assert!(
                node.stats().duplicates_dropped > 0,
                "Node {i} should drop duplicate ring frames"
            );
        }

        // Node 0 should not deliver its own broadcast
        assert_eq!(nodes[0].stats().deliveries, 0);
    }

    #[tokio::test]
    async fn test_ttl_stops_propagation_at_7_hops() {
        tokio::time::pause();

        let net = SimNetwork::new(303);
        // Line of 9 nodes: 0 to 8. Hop distances:
        // Node 0: hop 0 (origin)
        // Node 1: hop 0 received, relays hop 1
        // Node 2: hop 1 received, relays hop 2
        // Node 3: hop 2 received, relays hop 3
        // Node 4: hop 3 received, relays hop 4
        // Node 5: hop 4 received, relays hop 5
        // Node 6: hop 5 received, relays hop 6
        // Node 7: hop 6 received, relays hop 7
        // Node 8: hop 7 received, ttl_max=7 -> delivered, but NOT relayed!
        // Node 9: if present, receives nothing!
        let ids: Vec<Identity> = (0..10).map(|_| Identity::generate()).collect();
        let s_ids: Vec<ShortId> = ids.iter().map(|id| id.short_id()).collect();

        let mut nodes: Vec<MeshNode<_>> = ids
            .into_iter()
            .map(|id| {
                let s = id.short_id();
                let t = net.create_node(s);
                MeshNode::new(id, t, Box::new(NaiveFlood))
            })
            .collect();

        // Linear chain: 0-1-2-3-4-5-6-7-8-9
        for i in 0..9 {
            net.set_bidirectional_link(s_ids[i], s_ids[i + 1], Duration::ZERO, 0.0);
        }

        nodes[0].send_broadcast(b"TTL test".to_vec()).await.unwrap();

        // Drive the simulation through all nodes
        for _ in 0..30 {
            for node in nodes.iter_mut() {
                while let Ok(Ok(true)) =
                    tokio::time::timeout(Duration::from_millis(1), node.step()).await
                {
                    // step
                }
            }
        }

        // Nodes 1 through 8 must receive delivery
        for (i, node) in nodes.iter().enumerate().take(9).skip(1) {
            assert_eq!(
                node.stats().deliveries,
                1,
                "Node {i} should receive delivery"
            );
        }

        // Node 8 reached hop_count 7 == ttl_max -> hop_count_exceeded
        assert_eq!(nodes[8].stats().hop_count_exceeded, 1);

        // Node 9 must NEVER receive delivery because TTL stopped at Node 8!
        assert_eq!(
            nodes[9].stats().deliveries,
            0,
            "Node 9 must not receive message"
        );
    }

    #[test]
    fn test_adaptive_flood_delay_scales_with_neighbor_count() {
        let policy = crate::mesh::policy::AdaptiveFlood::default();
        let d0 = policy.window_upper_bound(0);
        let d5 = policy.window_upper_bound(5);
        let d10 = policy.window_upper_bound(10);
        let d100 = policy.window_upper_bound(100);

        assert_eq!(d0, Duration::from_millis(20));
        assert_eq!(d5, Duration::from_millis(20 + 5 * 15));
        assert_eq!(d10, Duration::from_millis(20 + 10 * 15));
        assert_eq!(d100, Duration::from_millis(300)); // capped at max_delay
    }

    #[tokio::test]
    async fn test_unicast_shortcut_skips_flood() {
        tokio::time::pause();

        let net = SimNetwork::new(404);
        let id_a = Identity::generate();
        let id_b = Identity::generate();
        let id_c = Identity::generate();

        let t_a = net.create_node(id_a.short_id());
        let t_b = net.create_node(id_b.short_id());
        let t_c = net.create_node(id_c.short_id());

        // A is connected to B and C
        net.set_bidirectional_link(id_a.short_id(), id_b.short_id(), Duration::ZERO, 0.0);
        net.set_bidirectional_link(id_a.short_id(), id_c.short_id(), Duration::ZERO, 0.0);

        let mut node_a = MeshNode::new(id_a, t_a, Box::new(NaiveFlood));
        let mut node_b = MeshNode::new(id_b, t_b, Box::new(NaiveFlood));
        let mut node_c = MeshNode::new(id_c, t_c, Box::new(NaiveFlood));

        // A sends direct unicast to B
        let payload = b"Direct unicast to B".to_vec();
        node_a
            .send_to(node_b.short_id(), payload.clone())
            .await
            .unwrap();

        // Step B: receives and delivers
        node_b.step().await.unwrap();
        assert_eq!(node_b.stats().deliveries, 1);
        let msg = node_b.recv_inbound().unwrap();
        assert_eq!(msg.payload, payload);

        // Step C: should receive NOTHING because unicast shortcut sent directly to B
        let step_c = tokio::time::timeout(Duration::from_millis(10), node_c.step()).await;
        assert!(step_c.is_err() || step_c.unwrap().is_err() || node_c.stats().deliveries == 0);
        assert_eq!(node_c.stats().deliveries, 0);
    }

    #[tokio::test]
    async fn test_adaptive_flood_suppression_in_dense_cluster() {
        tokio::time::pause();

        let net = SimNetwork::new(505);
        let count = 6;
        let ids: Vec<Identity> = (0..count).map(|_| Identity::generate()).collect();
        let s_ids: Vec<ShortId> = ids.iter().map(|id| id.short_id()).collect();

        // Create fully connected mesh (clique)
        for i in 0..count {
            for j in (i + 1)..count {
                net.set_bidirectional_link(s_ids[i], s_ids[j], Duration::ZERO, 0.0);
            }
        }

        let mut nodes: Vec<MeshNode<_>> = ids
            .into_iter()
            .map(|id| {
                let s = id.short_id();
                let t = net.create_node(s);
                let policy = crate::mesh::policy::AdaptiveFlood::new(
                    crate::mesh::policy::AdaptiveFloodConfig {
                        base_delay: Duration::from_millis(10),
                        per_neighbor_delay: Duration::from_millis(5),
                        max_delay: Duration::from_millis(50),
                        suppression_threshold: 2,
                        priority_delay: Duration::ZERO,
                    },
                );
                MeshNode::new(id, t, Box::new(policy))
            })
            .collect();

        let mut sim_time = Instant::now();

        // Node 0 broadcasts
        nodes[0]
            .send_broadcast(b"Dense cluster suppression test".to_vec())
            .await
            .unwrap();

        // Advance simulation time in 5ms slices
        for _ in 0..30 {
            sim_time += Duration::from_millis(5);
            for node in nodes.iter_mut() {
                while let Ok(Ok(true)) =
                    tokio::time::timeout(Duration::from_millis(1), node.step_at(sim_time)).await
                {
                    // drain
                }
            }
        }

        // All nodes 1..count must have delivered the message
        for (i, node) in nodes.iter().enumerate().skip(1) {
            assert_eq!(node.stats().deliveries, 1, "Node {i} must receive delivery");
        }

        // In a 6-node clique with threshold k=2, redundant rebroadcasts must be suppressed
        let total_suppressed: u64 = nodes.iter().map(|n| n.stats().relays_suppressed).sum();
        assert!(
            total_suppressed > 0,
            "Dense cluster should trigger counter-based relay suppression"
        );
    }

    #[tokio::test]
    async fn test_adaptive_flood_priority_messages_never_suppressed() {
        tokio::time::pause();

        let net = SimNetwork::new(606);
        let count = 4;
        let ids: Vec<Identity> = (0..count).map(|_| Identity::generate()).collect();
        let s_ids: Vec<ShortId> = ids.iter().map(|id| id.short_id()).collect();

        for i in 0..count {
            for j in (i + 1)..count {
                net.set_bidirectional_link(s_ids[i], s_ids[j], Duration::ZERO, 0.0);
            }
        }

        let mut nodes: Vec<MeshNode<_>> = ids
            .into_iter()
            .map(|id| {
                let s = id.short_id();
                let t = net.create_node(s);
                let policy = crate::mesh::policy::AdaptiveFlood::default();
                MeshNode::new(id, t, Box::new(policy))
            })
            .collect();

        // Node 0 broadcasts with priority flag = true
        let priority_flags = Flags {
            priority: true,
            ..Default::default()
        };
        nodes[0]
            .send_broadcast_with_type(
                MsgType::Chat,
                priority_flags,
                b"Urgent priority alert".to_vec(),
            )
            .await
            .unwrap();

        for _ in 0..20 {
            tokio::time::advance(Duration::from_millis(5)).await;
            for node in nodes.iter_mut() {
                while let Ok(Ok(true)) =
                    tokio::time::timeout(Duration::from_millis(1), node.step()).await
                {
                    // drain
                }
            }
        }

        // Check deliveries and zero suppressions
        for (i, node) in nodes.iter().enumerate().skip(1) {
            assert_eq!(node.stats().deliveries, 1);
            assert_eq!(
                node.stats().relays_suppressed,
                0,
                "Priority messages must never be suppressed for node {i}"
            );
        }
    }
}
