use std::collections::{BTreeSet, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use bitchat2_core::api::CORE_GIT_COMMIT;
use bitchat2_core::envelope::{pad_inner_payload, unpad_bucket, MsgType, HEADER_LEN, MAX_HOPS};
use bitchat2_core::identity::{Identity, PeerId, ShortId};
use bitchat2_core::mesh::cover::{
    CoverFrame, CoverFrameKind, CoverTrafficConfig, CoverTrafficManager, DeterministicCoverRng,
    COVER_MAGIC,
};
use bitchat2_core::mesh::delivery::{
    ApplicationFrame, ApplicationFrameKind, DedupStatus, InboxDeduplicator,
};
use bitchat2_core::mesh::peer::PeerSessionManager;
use bitchat2_core::mesh::policy::{
    AdaptiveFlood, AdaptiveFloodConfig, CoverageAwareFlood, CoverageAwareFloodConfig, NaiveFlood,
};
use bitchat2_core::mesh::relay::MeshNode;
use bitchat2_core::mesh::resource::{
    estimate_fragmented_wire_bytes, RelayResourceProfile, ResourceAwareRelayPolicy,
    ResourceBudgetManager, ThermalState,
};
use bitchat2_core::storage::{InjectedKeyProvider, KeyProvider};
use bitchat2_core::sync::{
    conversation_id, SqliteSyncStore, SyncRecord, SyncRecordKind, SyncSessionManager,
    MAX_SYNC_SESSION_BYTES,
};
use bitchat2_core::transport::sim::SimNetwork;

const NUM_SEEDS: usize = 20;
const BASE_SEED: u64 = 1000;
const LINK_LOSS: f64 = 0.05;
const LINK_DELAY_MS: u64 = 25;
const TIMER_STEP_MS: u64 = 10;
const SIM_BLE_MTU: usize = 244;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TopologyType {
    DenseCluster30,
    SparseLine10,
    RandomGeometric50,
    PartitionedBridge31,
    Intermittent20,
    Grid7x7Historical,
}

impl TopologyType {
    fn name(&self) -> &'static str {
        match self {
            Self::DenseCluster30 => "Dense Cluster (30 nodes)",
            Self::SparseLine10 => "Sparse Line (10 nodes)",
            Self::RandomGeometric50 => "Random Geometric (50 nodes)",
            Self::PartitionedBridge31 => "Partitioned Bridge (31 nodes)",
            Self::Intermittent20 => "Intermittent Connectivity (20 nodes)",
            Self::Grid7x7Historical => "Grid (7x7, 49 nodes)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PolicyType {
    Naive,
    Adaptive,
    CoverageAware,
    CoverageResourceBalanced,
    CoverageResourceMinimal,
}

impl PolicyType {
    fn name(&self) -> &'static str {
        match self {
            Self::Naive => "NaiveFlood",
            Self::Adaptive => "AdaptiveFlood",
            Self::CoverageAware => "CoverageAware",
            Self::CoverageResourceBalanced => "Coverage+ResBalanced",
            Self::CoverageResourceMinimal => "Coverage+ResMinimal",
        }
    }
}

#[derive(Debug, Clone, Default)]
struct FloodRunResult {
    intended: usize,
    reachable_within_ttl: usize,
    delivered: usize,
    delivery_ratio_total: f64,
    delivery_ratio_reachable: f64,
    transmissions: u64,
    wire_bytes_total: usize,
    bytes_per_delivered_msg: f64,
    median_latency_ms: f64,
    p95_latency_ms: f64,
    duplicate_receptions: u64,
    duplicate_displays: u64,
    relays_suppressed: u64,
    max_device_tx: u64,
    mean_device_tx: f64,
}

#[derive(Debug, Clone, Default)]
struct RetryAblationRow {
    workload: &'static str,
    policy: &'static str,
    loss_pct: u32,
    seed: u64,
    messages_sent: usize,
    delivered_before_expiry: usize,
    delivery_ratio: f64,
    handshake_tx: u64,
    data_tx: u64,
    ack_tx: u64,
    retries: u64,
    wire_bytes_total: usize,
    max_queue_occupancy: usize,
    duplicate_receptions: u64,
    duplicate_displays: u64,
    median_latency_ms: f64,
    p95_latency_ms: f64,
}

#[derive(Debug, Clone, Default)]
struct CoverRunRow {
    mode: &'static str,
    seed: u64,
    real_frames: u64,
    dummy_frames: u64,
    total_wire_bytes: usize,
    dummy_wire_bytes: usize,
    mean_interval_ms: f64,
    cv_interval: f64,
    mean_real_latency_ms: f64,
}

fn deterministic_identities(count: usize, seed: u64) -> Vec<Identity> {
    let mut rng = StdRng::seed_from_u64(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    (0..count)
        .map(|_| {
            let mut key_bytes = [0u8; 32];
            rng.fill(&mut key_bytes);
            Identity::from_bytes(&key_bytes)
        })
        .collect()
}

fn count_reachable_within_ttl(net: &SimNetwork, origin: ShortId, max_hops: u8) -> usize {
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    visited.insert(origin);
    queue.push_back((origin, 0u8));

    while let Some((curr, dist)) = queue.pop_front() {
        if dist >= max_hops {
            continue;
        }
        for nbr in net.neighbors_of(curr) {
            if visited.insert(nbr) {
                queue.push_back((nbr, dist + 1));
            }
        }
    }
    visited.len().saturating_sub(1)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 * p).ceil() as usize).saturating_sub(1);
    sorted[idx.min(sorted.len() - 1)]
}

fn mean_std(vals: &[f64]) -> (f64, f64) {
    if vals.is_empty() {
        return (0.0, 0.0);
    }
    let n = vals.len() as f64;
    let mean = vals.iter().sum::<f64>() / n;
    let var = vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
    (mean, var.sqrt())
}

type BuiltTopology = (
    SimNetwork,
    Vec<Identity>,
    Vec<ShortId>,
    usize,
    Vec<(usize, usize)>,
);

fn build_topology(topo: TopologyType, seed: u64) -> BuiltTopology {
    let net = SimNetwork::new(seed);
    let link_delay = Duration::ZERO;
    let mut edge_pairs = Vec::new();

    match topo {
        TopologyType::SparseLine10 => {
            let count = 10;
            let ids = deterministic_identities(count, seed);
            let s_ids: Vec<ShortId> = ids.iter().map(Identity::short_id).collect();
            for i in 0..count - 1 {
                net.set_bidirectional_link(s_ids[i], s_ids[i + 1], link_delay, LINK_LOSS);
                edge_pairs.push((i, i + 1));
            }
            (net, ids, s_ids, count - 1, edge_pairs)
        }
        TopologyType::Grid7x7Historical => {
            let rows = 7;
            let cols = 7;
            let count = rows * cols;
            let ids = deterministic_identities(count, seed);
            let s_ids: Vec<ShortId> = ids.iter().map(Identity::short_id).collect();
            for r in 0..rows {
                for c in 0..cols {
                    let idx = r * cols + c;
                    if c + 1 < cols {
                        let right = r * cols + (c + 1);
                        net.set_bidirectional_link(s_ids[idx], s_ids[right], link_delay, LINK_LOSS);
                        edge_pairs.push((idx, right));
                    }
                    if r + 1 < rows {
                        let down = (r + 1) * cols + c;
                        net.set_bidirectional_link(s_ids[idx], s_ids[down], link_delay, LINK_LOSS);
                        edge_pairs.push((idx, down));
                    }
                }
            }
            (net, ids, s_ids, count - 1, edge_pairs)
        }
        TopologyType::DenseCluster30 => {
            let count = 30;
            let ids = deterministic_identities(count, seed);
            let s_ids: Vec<ShortId> = ids.iter().map(Identity::short_id).collect();
            for i in 0..count {
                for j in (i + 1)..count {
                    net.set_bidirectional_link(s_ids[i], s_ids[j], link_delay, LINK_LOSS);
                    edge_pairs.push((i, j));
                }
            }
            (net, ids, s_ids, count - 1, edge_pairs)
        }
        TopologyType::RandomGeometric50 => {
            let count = 50;
            let mut rng = StdRng::seed_from_u64(seed.wrapping_add(88888));
            let coords: Vec<(f64, f64)> = (0..count).map(|_| (rng.gen(), rng.gen())).collect();
            let ids = deterministic_identities(count, seed);
            let s_ids: Vec<ShortId> = ids.iter().map(Identity::short_id).collect();
            let radius = 0.28;
            for i in 0..count {
                for j in (i + 1)..count {
                    let dx = coords[i].0 - coords[j].0;
                    let dy = coords[i].1 - coords[j].1;
                    if (dx * dx + dy * dy).sqrt() <= radius {
                        net.set_bidirectional_link(s_ids[i], s_ids[j], link_delay, LINK_LOSS);
                        edge_pairs.push((i, j));
                    }
                }
            }
            if net.neighbors_of(s_ids[0]).is_empty() {
                net.set_bidirectional_link(s_ids[0], s_ids[1], link_delay, LINK_LOSS);
                edge_pairs.push((0, 1));
            }
            (net, ids, s_ids, count - 1, edge_pairs)
        }
        TopologyType::PartitionedBridge31 => {
            let count = 31;
            let ids = deterministic_identities(count, seed);
            let s_ids: Vec<ShortId> = ids.iter().map(Identity::short_id).collect();
            for i in 0..15 {
                for j in (i + 1)..15 {
                    net.set_bidirectional_link(s_ids[i], s_ids[j], link_delay, LINK_LOSS);
                    edge_pairs.push((i, j));
                }
            }
            for i in 16..31 {
                for j in (i + 1)..31 {
                    net.set_bidirectional_link(s_ids[i], s_ids[j], link_delay, LINK_LOSS);
                    edge_pairs.push((i, j));
                }
            }
            net.set_bidirectional_link(s_ids[14], s_ids[15], link_delay, LINK_LOSS);
            net.set_bidirectional_link(s_ids[15], s_ids[16], link_delay, LINK_LOSS);
            edge_pairs.push((14, 15));
            edge_pairs.push((15, 16));
            (net, ids, s_ids, count - 1, edge_pairs)
        }
        TopologyType::Intermittent20 => {
            let count = 20;
            let ids = deterministic_identities(count, seed);
            let s_ids: Vec<ShortId> = ids.iter().map(Identity::short_id).collect();
            for i in 0..count {
                if i + 1 < count {
                    net.set_bidirectional_link(s_ids[i], s_ids[i + 1], link_delay, LINK_LOSS);
                    edge_pairs.push((i, i + 1));
                }
                if i + 2 < count {
                    net.set_bidirectional_link(s_ids[i], s_ids[i + 2], link_delay, LINK_LOSS);
                    edge_pairs.push((i, i + 2));
                }
                if i + 4 < count && i % 2 == 0 {
                    net.set_bidirectional_link(s_ids[i], s_ids[i + 4], link_delay, LINK_LOSS);
                    edge_pairs.push((i, i + 4));
                }
            }
            (net, ids, s_ids, count - 1, edge_pairs)
        }
    }
}

async fn run_flood_simulation(
    topo: TopologyType,
    policy_type: PolicyType,
    seed: u64,
) -> FloodRunResult {
    let (net, ids, s_ids, intended_count, edge_pairs) = build_topology(topo, seed);
    let reachable_within_ttl = count_reachable_within_ttl(&net, s_ids[0], MAX_HOPS);
    let t0 = Instant::now();

    let mut nodes: Vec<MeshNode<_>> = ids
        .into_iter()
        .enumerate()
        .map(|(idx, id)| {
            let s = id.short_id();
            let t = net.create_node(s);
            let node_seed = seed.wrapping_add((idx as u64 + 1) * 1009);
            match policy_type {
                PolicyType::Naive => MeshNode::new(id, t, Box::new(NaiveFlood)),
                PolicyType::Adaptive => MeshNode::new(
                    id,
                    t,
                    Box::new(AdaptiveFlood::with_seed(
                        AdaptiveFloodConfig::default(),
                        node_seed,
                    )),
                ),
                PolicyType::CoverageAware => {
                    let pol = CoverageAwareFlood::with_seed(
                        CoverageAwareFloodConfig::default(),
                        node_seed,
                    );
                    for &nbr in &net.neighbors_of(s) {
                        pol.record_observed(nbr, t0);
                        pol.record_claimed_neighbors(nbr, net.neighbors_of(nbr), t0);
                    }
                    MeshNode::new(id, t, Box::new(pol))
                }
                PolicyType::CoverageResourceBalanced => {
                    let pol = CoverageAwareFlood::with_seed(
                        CoverageAwareFloodConfig::default(),
                        node_seed,
                    );
                    for &nbr in &net.neighbors_of(s) {
                        pol.record_observed(nbr, t0);
                        pol.record_claimed_neighbors(nbr, net.neighbors_of(nbr), t0);
                    }
                    let budget = ResourceBudgetManager::new(
                        RelayResourceProfile::Balanced,
                        Duration::from_secs(10),
                        t0,
                    );
                    MeshNode::new(id, t, Box::new(ResourceAwareRelayPolicy::new(pol, budget)))
                }
                PolicyType::CoverageResourceMinimal => {
                    let pol = CoverageAwareFlood::with_seed(
                        CoverageAwareFloodConfig::default(),
                        node_seed,
                    );
                    for &nbr in &net.neighbors_of(s) {
                        pol.record_observed(nbr, t0);
                        pol.record_claimed_neighbors(nbr, net.neighbors_of(nbr), t0);
                    }
                    let budget = ResourceBudgetManager::new(
                        RelayResourceProfile::Minimal,
                        Duration::from_secs(10),
                        t0,
                    );
                    MeshNode::new(id, t, Box::new(ResourceAwareRelayPolicy::new(pol, budget)))
                }
            }
        })
        .collect();

    let payload = b"EVALUATION_BROADCAST_PAYLOAD".to_vec();
    let env_wire_len = estimate_fragmented_wire_bytes(HEADER_LEN + payload.len(), SIM_BLE_MTU);
    if nodes[0].send_broadcast(payload).await.is_err() {
        return FloodRunResult::default();
    }

    let mut sim_time = t0;
    let mut delivered_set = HashSet::new();
    let mut display_seen: Vec<HashSet<[u8; 8]>> = vec![HashSet::new(); nodes.len()];
    let mut duplicate_displays = 0u64;
    let mut latencies: Vec<f64> = Vec::new();
    let link_delay = Duration::ZERO;

    for step in 0..120usize {
        sim_time += Duration::from_millis(TIMER_STEP_MS);

        if topo == TopologyType::Intermittent20 && step % 5 == 0 {
            let phase = (step / 5) + (seed as usize);
            for (e_idx, &(u, v)) in edge_pairs.iter().enumerate() {
                if (e_idx + phase).is_multiple_of(4) {
                    net.set_bidirectional_link(s_ids[u], s_ids[v], link_delay, 1.0);
                } else {
                    net.set_bidirectional_link(s_ids[u], s_ids[v], link_delay, LINK_LOSS);
                }
            }
        }

        let mut progress = true;
        while progress {
            progress = false;
            for (n_idx, node) in nodes.iter_mut().enumerate() {
                while let Ok(true) = node.poll_step(sim_time).await {
                    progress = true;
                }
                while let Some(msg) = node.recv_inbound() {
                    let id = node.short_id();
                    if id != s_ids[0] {
                        if !display_seen[n_idx].insert(msg.msg_id) {
                            duplicate_displays += 1;
                        }
                        if delivered_set.insert(id) {
                            let holdback_ms = sim_time.duration_since(t0).as_secs_f64() * 1000.0
                                - (TIMER_STEP_MS as f64);
                            let hop_transit_ms =
                                ((msg.hop_count as f64) + 1.0) * (LINK_DELAY_MS as f64);
                            latencies.push(holdback_ms.max(0.0) + hop_transit_ms);
                        }
                    }
                }
            }
        }
    }

    let per_device_tx: Vec<u64> = nodes.iter().map(|n| n.stats().transmissions).collect();
    let total_tx: u64 = per_device_tx.iter().sum();
    let max_device_tx: u64 = per_device_tx.iter().copied().max().unwrap_or(0);
    let mean_device_tx = total_tx as f64 / nodes.len().max(1) as f64;
    let total_dups: u64 = nodes.iter().map(|n| n.stats().duplicates_dropped).sum();
    let total_suppressed: u64 = nodes.iter().map(|n| n.stats().relays_suppressed).sum();
    let wire_bytes_total = (total_tx as usize).saturating_mul(env_wire_len);

    let delivered = delivered_set.len();
    let delivery_ratio_total = if intended_count > 0 {
        delivered as f64 / intended_count as f64
    } else {
        0.0
    };
    let delivery_ratio_reachable = if reachable_within_ttl > 0 {
        (delivered as f64 / reachable_within_ttl as f64).min(1.0)
    } else {
        0.0
    };
    let bytes_per_delivered_msg = if delivered > 0 {
        wire_bytes_total as f64 / delivered as f64
    } else {
        wire_bytes_total as f64
    };

    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_lat = percentile(&latencies, 0.50);
    let p95_lat = percentile(&latencies, 0.95);

    FloodRunResult {
        intended: intended_count,
        reachable_within_ttl,
        delivered,
        delivery_ratio_total,
        delivery_ratio_reachable,
        transmissions: total_tx,
        wire_bytes_total,
        bytes_per_delivered_msg,
        median_latency_ms: median_lat,
        p95_latency_ms: p95_lat,
        duplicate_receptions: total_dups,
        duplicate_displays,
        relays_suppressed: total_suppressed,
        max_device_tx,
        mean_device_tx,
    }
}

async fn run_unicast_retry_ablation(
    warm_session: bool,
    max_attempts: u32,
    loss_pct: u32,
    seed: u64,
) -> RetryAblationRow {
    let loss = (loss_pct as f64) / 100.0;
    let net = SimNetwork::new(seed);
    let mut ids_iter = deterministic_identities(3, seed).into_iter();
    let Some(id_a) = ids_iter.next() else {
        return RetryAblationRow::default();
    };
    let Some(id_r) = ids_iter.next() else {
        return RetryAblationRow::default();
    };
    let Some(id_b) = ids_iter.next() else {
        return RetryAblationRow::default();
    };
    let sa = id_a.short_id();
    let sr = id_r.short_id();
    let sb = id_b.short_id();
    let pa = id_a.peer_id();
    let pb = id_b.peer_id();

    let link_delay = Duration::ZERO;
    // Warm-up handshake phase uses reliable setup if warm_session == true
    let setup_loss = if warm_session { 0.0 } else { loss };
    net.set_bidirectional_link(sa, sr, link_delay, setup_loss);
    net.set_bidirectional_link(sr, sb, link_delay, setup_loss);

    let mut node_a = MeshNode::new(id_a, net.create_node(sa), Box::new(NaiveFlood));
    let mut node_r = MeshNode::new(id_r, net.create_node(sr), Box::new(NaiveFlood));
    let mut node_b = MeshNode::new(id_b, net.create_node(sb), Box::new(NaiveFlood));

    let mut pm_a = PeerSessionManager::new();
    let mut pm_b = PeerSessionManager::new();
    let mut sim_time = Instant::now();

    let mut handshake_tx = 0u64;
    let mut handshake_bytes = 0usize;

    let _ = pm_a.initiate_handshake(&mut node_a, sb).await;
    if !warm_session {
        handshake_tx += 1;
        handshake_bytes += 140;
    }

    for step in 0..80usize {
        sim_time += Duration::from_millis(TIMER_STEP_MS);
        while let Ok(true) = node_a.poll_step(sim_time).await {}
        while let Ok(true) = node_r.poll_step(sim_time).await {}
        while let Ok(true) = node_b.poll_step(sim_time).await {}

        while let Some(m) = node_a.recv_inbound() {
            if m.msg_type == MsgType::Handshake {
                let before = node_a.stats().transmissions;
                let _ = pm_a.handle_handshake(&mut node_a, &m).await;
                let delta = node_a.stats().transmissions.saturating_sub(before);
                if !warm_session {
                    handshake_tx += delta;
                    handshake_bytes += (delta as usize) * 160;
                }
            }
        }
        while let Some(m) = node_b.recv_inbound() {
            if m.msg_type == MsgType::Handshake {
                let before = node_b.stats().transmissions;
                let _ = pm_b.handle_handshake(&mut node_b, &m).await;
                let delta = node_b.stats().transmissions.saturating_sub(before);
                if !warm_session {
                    handshake_tx += delta;
                    handshake_bytes += (delta as usize) * 160;
                }
            }
        }
        while node_r.recv_inbound().is_some() {}

        if pm_a.is_session_ready(&sb) && pm_b.is_session_ready(&sa) {
            break;
        }
        if !warm_session && step % 15 == 14 && !pm_a.is_session_ready(&sb) {
            let before = node_a.stats().transmissions;
            let _ = pm_a.initiate_handshake(&mut node_a, sb).await;
            let delta = node_a.stats().transmissions.saturating_sub(before);
            handshake_tx += delta;
            handshake_bytes += (delta as usize) * 140;
        }
    }

    // Apply lossy links for the message delivery workload
    net.set_bidirectional_link(sa, sr, link_delay, loss);
    net.set_bidirectional_link(sr, sb, link_delay, loss);

    let messages_sent = 5usize;
    let mut dedup_b = InboxDeduplicator::new(1024, Duration::from_secs(3600));
    let mut display_set = HashSet::new();
    let mut duplicate_receptions = 0u64;
    let mut duplicate_displays = 0u64;
    let mut data_tx = 0u64;
    let mut ack_tx = 0u64;
    let mut retries = 0u64;
    let mut wire_bytes_total = handshake_bytes;
    let mut latencies = Vec::new();
    let mut delivered_before_expiry = 0usize;
    let max_queue_occupancy = messages_sent;

    for m_idx in 0..messages_sent {
        if !pm_a.is_session_ready(&sb) || !pm_b.is_session_ready(&sa) {
            break;
        }
        let mut lid = [0u8; 16];
        lid[0..8].copy_from_slice(&seed.to_be_bytes());
        lid[8..16].copy_from_slice(&(m_idx as u64).to_be_bytes());

        let send_t0 = sim_time;
        let mut acked = false;

        for attempt in 0..max_attempts {
            if attempt > 0 {
                retries += 1;
            }
            let app_frame = ApplicationFrame::new_message(
                lid,
                pa,
                pb,
                format!("eval-msg-{seed}-{m_idx}").into_bytes(),
                Duration::from_secs(60),
                1_700_000_000,
            );
            let Ok(padded) = pad_inner_payload(&app_frame.encode(), true) else {
                break;
            };
            let tx_before = node_a.stats().transmissions + node_r.stats().transmissions;
            let _ = pm_a.send_encrypted(&mut node_a, sb, &padded).await;

            // Step 12 ticks (120ms) waiting for delivery & end-to-end ACK
            for _ in 0..12 {
                sim_time += Duration::from_millis(TIMER_STEP_MS);
                while let Ok(true) = node_a.poll_step(sim_time).await {}
                while let Ok(true) = node_r.poll_step(sim_time).await {}
                while let Ok(true) = node_b.poll_step(sim_time).await {}
                while node_r.recv_inbound().is_some() {}

                while let Some(in_b) = node_b.recv_inbound() {
                    if let Ok(dec) = pm_b.decrypt_inbound(&in_b) {
                        if let Ok(unpad) = unpad_bucket(&dec.plaintext) {
                            if let Ok(af) = ApplicationFrame::decode(&unpad) {
                                if af.kind == ApplicationFrameKind::Message {
                                    match dedup_b.check_or_insert(&pa, &af.logical_id, sim_time) {
                                        DedupStatus::New => {
                                            if !display_set.insert(af.logical_id) {
                                                duplicate_displays += 1;
                                            }
                                        }
                                        DedupStatus::Duplicate => {
                                            duplicate_receptions += 1;
                                        }
                                    }
                                    let ack_f = ApplicationFrame::new_ack(
                                        af.logical_id,
                                        pb,
                                        pa,
                                        1_700_000_000,
                                    );
                                    if let Ok(p_ack) = pad_inner_payload(&ack_f.encode(), true) {
                                        let b_tx0 = node_b.stats().transmissions
                                            + node_r.stats().transmissions;
                                        let _ = pm_b.send_encrypted(&mut node_b, sa, &p_ack).await;
                                        let b_tx1 = node_b.stats().transmissions
                                            + node_r.stats().transmissions;
                                        let ack_delta = b_tx1.saturating_sub(b_tx0).max(1);
                                        ack_tx += ack_delta;
                                        wire_bytes_total += (ack_delta as usize) * 311;
                                    }
                                }
                            }
                        }
                    }
                }

                while let Some(in_a) = node_a.recv_inbound() {
                    if let Ok(dec) = pm_a.decrypt_inbound(&in_a) {
                        if let Ok(unpad) = unpad_bucket(&dec.plaintext) {
                            if let Ok(af) = ApplicationFrame::decode(&unpad) {
                                if af.kind == ApplicationFrameKind::Ack && af.logical_id == lid {
                                    acked = true;
                                }
                            }
                        }
                    }
                }

                if acked {
                    break;
                }
            }

            let tx_after = node_a.stats().transmissions + node_r.stats().transmissions;
            let d_tx = tx_after.saturating_sub(tx_before).max(1);
            data_tx += d_tx;
            wire_bytes_total += (d_tx as usize) * 311;

            if acked {
                delivered_before_expiry += 1;
                // 2 hops forward + 2 hops ACK = 4 link transits (100ms) + retry backoff time
                let retry_wait_ms = (attempt as f64) * 120.0;
                let rtt_transit_ms = 4.0 * (LINK_DELAY_MS as f64);
                let _ = send_t0;
                latencies.push(retry_wait_ms + rtt_transit_ms);
                break;
            }
        }
    }

    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    RetryAblationRow {
        workload: if warm_session {
            "WarmSession"
        } else {
            "FreshSession"
        },
        policy: if max_attempts == 1 {
            "NoRetry(1)"
        } else {
            "WithRetry(3)"
        },
        loss_pct,
        seed,
        messages_sent,
        delivered_before_expiry,
        delivery_ratio: delivered_before_expiry as f64 / messages_sent as f64,
        handshake_tx,
        data_tx,
        ack_tx,
        retries,
        wire_bytes_total,
        max_queue_occupancy,
        duplicate_receptions,
        duplicate_displays,
        median_latency_ms: percentile(&latencies, 0.50),
        p95_latency_ms: percentile(&latencies, 0.95),
    }
}

fn run_cover_experiment(cover_on: bool, seed: u64) -> CoverRunRow {
    let cfg = CoverTrafficConfig {
        enabled: cover_on,
        min_interval: Duration::from_millis(1500),
        max_interval: Duration::from_millis(4500),
        max_bytes_per_minute: 2048,
        ..CoverTrafficConfig::default()
    };

    let mut mgr = CoverTrafficManager::new(cfg, Box::new(DeterministicCoverRng::new(seed)));
    let peer = PeerId([0x55; 32]);
    let t0 = Instant::now();
    if cover_on {
        let _ = mgr.offer_peer(peer);
        let _ = mgr.handle_control(peer, &CoverFrame::control(CoverFrameKind::Accept), t0);
    }

    let real_send_steps = [45usize, 130, 290, 410, 520]; // out of 600 steps (60s @ 100ms step)
    let mut packet_timestamps_ms = Vec::new();
    let mut real_frames = 0u64;
    let mut dummy_frames = 0u64;
    let mut total_wire_bytes = 0usize;
    let mut dummy_wire_bytes = 0usize;
    let mut real_latencies = Vec::new();

    for step in 1..=600usize {
        let now = t0 + Duration::from_millis((step as u64) * 100);
        let has_real = real_send_steps.contains(&step);
        if has_real {
            real_frames += 1;
            total_wire_bytes += 311; // 256B bucket + 55B Noise/Envelope header
            packet_timestamps_ms.push((step as f64) * 100.0);
            real_latencies.push(LINK_DELAY_MS as f64);
        }
        let dummies = mgr.poll_due_dummies(
            now,
            &[peer],
            RelayResourceProfile::Balanced,
            ThermalState::Nominal,
            has_real,
        );
        for (_, d) in dummies {
            assert!(d.encode().starts_with(COVER_MAGIC));
            dummy_frames += 1;
            total_wire_bytes += 311;
            dummy_wire_bytes += 311;
            packet_timestamps_ms.push((step as f64) * 100.0);
        }
    }

    let mut intervals = Vec::new();
    for w in packet_timestamps_ms.windows(2) {
        intervals.push(w[1] - w[0]);
    }
    let (mean_int, std_int) = mean_std(&intervals);
    let cv = if mean_int > 0.0 {
        std_int / mean_int
    } else {
        0.0
    };
    let (mean_lat, _) = mean_std(&real_latencies);

    CoverRunRow {
        mode: if cover_on { "CoverON" } else { "CoverOFF" },
        seed,
        real_frames,
        dummy_frames,
        total_wire_bytes,
        dummy_wire_bytes,
        mean_interval_ms: mean_int,
        cv_interval: cv,
        mean_real_latency_ms: mean_lat,
    }
}

fn make_sync_record(
    author: &Identity,
    recipient: PeerId,
    seed: u64,
    created_at: u64,
    body_text: &str,
) -> Option<SyncRecord> {
    let h = blake3::hash(&seed.to_be_bytes());
    let mut lid = [0u8; 16];
    lid.copy_from_slice(&h.as_bytes()[0..16]);
    SyncRecord::create_signed(
        author,
        recipient,
        lid,
        created_at,
        created_at + 86_400,
        SyncRecordKind::Message,
        body_text.as_bytes().to_vec(),
    )
    .ok()
}

fn run_sync_eval_cell(n: usize, ov: usize, use_merkle: bool) -> (usize, u32, usize, &'static str) {
    let now_unix = 50_000u64;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("bitchat_eval_sync_{n}_{ov}_{use_merkle}_{ts}"));
    let _ = fs::create_dir_all(&dir);
    let id_a = Identity::from_bytes(&[0x31; 32]);
    let id_b = Identity::from_bytes(&[0x32; 32]);
    let kp: Arc<dyn KeyProvider> = Arc::new(InjectedKeyProvider::new([0x77; 32], 1));
    let Ok(st_a) = SqliteSyncStore::open(&dir.join("a.db"), id_a.peer_id(), Arc::clone(&kp)) else {
        let _ = fs::remove_dir_all(&dir);
        return (0, 0, 0, "STORE_ERR");
    };
    let Ok(st_b) = SqliteSyncStore::open(&dir.join("b.db"), id_b.peer_id(), kp) else {
        let _ = fs::remove_dir_all(&dir);
        return (0, 0, 0, "STORE_ERR");
    };

    let cid = conversation_id(&id_a.peer_id(), &id_b.peer_id());
    let shared_count = (n * ov) / 100;
    let diff_count = n - shared_count;

    let mut a_set = Vec::with_capacity(n);
    let mut b_set = Vec::with_capacity(n);
    for i in 0..shared_count {
        if let Some(r) = make_sync_record(
            &id_a,
            id_b.peer_id(),
            1_000 + i as u64,
            10_000 + i as u64,
            "shared-payload-0123456789abcdef",
        ) {
            a_set.push(r.clone());
            b_set.push(r);
        }
    }
    for i in 0..diff_count {
        if let Some(ra) = make_sync_record(
            &id_a,
            id_b.peer_id(),
            100_000 + i as u64,
            20_000 + i as u64,
            "only-a-payload-0123456789abcdef",
        ) {
            a_set.push(ra);
        }
        if let Some(rb) = make_sync_record(
            &id_b,
            id_a.peer_id(),
            200_000 + i as u64,
            30_000 + i as u64,
            "only-b-payload-0123456789abcdef",
        ) {
            b_set.push(rb);
        }
    }
    let _ = st_a.upsert_records_atomic(&cid, &a_set, now_unix);
    let _ = st_b.upsert_records_atomic(&cid, &b_set, now_unix);

    let mut mgr_a = SyncSessionManager::new(true);
    let mut mgr_b = SyncSessionManager::new(true);
    let now_inst = Instant::now();
    let mut q_ab = mgr_a
        .initiate_session(
            id_a.peer_id(),
            id_b.peer_id(),
            &st_a,
            use_merkle,
            now_inst,
            now_unix,
        )
        .unwrap_or_default();
    let mut q_ba = Vec::new();
    let mut total_bytes: usize = q_ab.iter().map(Vec::len).sum();

    for _ in 0..500 {
        if q_ab.is_empty() && q_ba.is_empty() {
            break;
        }
        for c in std::mem::take(&mut q_ab) {
            let o = mgr_b.handle_inbound_chunk(
                id_b.peer_id(),
                id_a.peer_id(),
                &c,
                Some(&st_b),
                now_inst,
                now_unix,
            );
            for oc in o.outbound_chunks {
                total_bytes += oc.len();
                q_ba.push(oc);
            }
        }
        for c in std::mem::take(&mut q_ba) {
            let o = mgr_a.handle_inbound_chunk(
                id_a.peer_id(),
                id_b.peer_id(),
                &c,
                Some(&st_a),
                now_inst,
                now_unix,
            );
            for oc in o.outbound_chunks {
                total_bytes += oc.len();
                q_ab.push(oc);
            }
        }
    }

    let ma = mgr_a
        .metrics_for(&id_b.peer_id())
        .cloned()
        .unwrap_or_default();
    let mb = mgr_b
        .metrics_for(&id_a.peer_id())
        .cloned()
        .unwrap_or_default();
    let outcome = if ma.aborted_reason.as_deref() == Some("SESSION_BYTE_QUOTA_EXCEEDED")
        || mb.aborted_reason.as_deref() == Some("SESSION_BYTE_QUOTA_EXCEEDED")
    {
        assert!(ma.wire_bytes_sent + ma.wire_bytes_recv <= MAX_SYNC_SESSION_BYTES + 2048);
        "QUOTA_ABORT_256KIB"
    } else {
        let ra: BTreeSet<_> = st_a
            .list_records(&cid, now_unix)
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.logical_id)
            .collect();
        let rb: BTreeSet<_> = st_b
            .list_records(&cid, now_unix)
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.logical_id)
            .collect();
        if ra == rb && ra.len() == n + diff_count {
            "CONVERGED"
        } else {
            "INCOMPLETE"
        }
    };

    drop(st_a);
    drop(st_b);
    let _ = fs::remove_dir_all(&dir);
    (
        total_bytes,
        ma.rounds.max(mb.rounds),
        ma.merkle_nodes_queried.max(mb.merkle_nodes_queried),
        outcome,
    )
}

fn write_svg_charts(
    flood_summary: &[(&str, &str, f64, f64)],
    sync_summary: &[(usize, usize, usize, usize)],
) -> std::io::Result<()> {
    // 1. Flood chart: Dense Cluster (30 nodes) & Random Geometric (50 nodes) transmissions vs reachable delivery
    let mut f1 = File::create("docs/eval/chart_flood_tx_vs_delivery.svg")?;
    writeln!(
        f1,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="760" height="360" viewBox="0 0 760 360">
  <rect width="100%" height="100%" fill="#121212"/>
  <text x="24" y="34" fill="#ffffff" font-family="monospace" font-size="15" font-weight="bold">Phase 6 Flood Policies — Mean Transmissions vs Reachable Delivery (20 Seeds)</text>"##
    )?;
    let mut y = 65;
    for &(topo, pol, avg_tx, avg_deliv) in flood_summary.iter().filter(|(t, _, _, _)| {
        *t == "Dense Cluster (30 nodes)" || *t == "Random Geometric (50 nodes)"
    }) {
        let bar_w = (avg_tx * 5.0).min(380.0) as usize;
        let deliv_pct = avg_deliv * 100.0;
        let short_t = if topo.starts_with("Dense") {
            "Dense30"
        } else {
            "RandGeo50"
        };
        writeln!(
            f1,
            r##"  <text x="24" y="{}" fill="#cccccc" font-family="monospace" font-size="11">{:<9} {:<20}</text>
  <rect x="240" y="{}" width="{}" height="16" fill="#4CAF50" rx="3"/>
  <text x="{}" y="{}" fill="#81C784" font-family="monospace" font-size="11">{:.1} tx | {:.1}% deliv(TTL)</text>"##,
            y + 12,
            short_t,
            pol,
            y,
            bar_w,
            248 + bar_w,
            y + 12,
            avg_tx,
            deliv_pct
        )?;
        y += 26;
    }
    writeln!(f1, "</svg>")?;

    // 2. Sync chart: IdInventory vs Merkle bytes at 100% and 95% overlap
    let mut f2 = File::create("docs/eval/chart_sync_bytes.svg")?;
    writeln!(
        f2,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="760" height="340" viewBox="0 0 760 340">
  <rect width="100%" height="100%" fill="#121212"/>
  <text x="24" y="34" fill="#ffffff" font-family="monospace" font-size="15" font-weight="bold">Phase 6 Sync — IdInventory vs Bounded Merkle Wire Bytes</text>"##
    )?;
    let mut y2 = 65;
    for &(n, ov, inv_b, mk_b) in sync_summary
        .iter()
        .filter(|(n, ov, _, _)| *n >= 32 && (*ov == 100 || *ov == 95))
    {
        let w_inv = ((inv_b as f64) / 120.0).clamp(4.0, 400.0) as usize;
        let w_mk = ((mk_b as f64) / 120.0).clamp(4.0, 400.0) as usize;
        writeln!(
            f2,
            r##"  <text x="24" y="{}" fill="#cccccc" font-family="monospace" font-size="11">n={:<3} ov={:>3}%</text>
  <rect x="145" y="{}" width="{}" height="10" fill="#29B6F6" rx="2"/>
  <text x="{}" y="{}" fill="#29B6F6" font-family="monospace" font-size="10">IdInv: {} B</text>
  <rect x="145" y="{}" width="{}" height="10" fill="#AB47BC" rx="2"/>
  <text x="{}" y="{}" fill="#CE93D8" font-family="monospace" font-size="10">Merkle: {} B</text>"##,
            y2 + 12,
            n,
            ov,
            y2,
            w_inv,
            152 + w_inv,
            y2 + 9,
            inv_b,
            y2 + 13,
            w_mk,
            152 + w_mk,
            y2 + 22,
            mk_b
        )?;
        y2 += 32;
    }
    writeln!(f2, "</svg>")?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all("docs/eval")?;

    let config_json = serde_json::json!({
        "git_commit": CORE_GIT_COMMIT,
        "seeds": (BASE_SEED..BASE_SEED + NUM_SEEDS as u64).collect::<Vec<u64>>(),
        "num_seeds": NUM_SEEDS,
        "link_loss_default": LINK_LOSS,
        "link_delay_ms": LINK_DELAY_MS,
        "timer_step_ms": TIMER_STEP_MS,
        "max_hops": MAX_HOPS,
        "sim_ble_mtu_bytes": SIM_BLE_MTU,
        "preserved_historical_artifact": "evaluation_results.csv",
        "comparator_scope": "NaiveFlood is Bitchat 2.0 internal baseline, NOT external native bitchat app.",
        "resource_scope": "Configured byte/work policy proxies in simulator; real energy measurements belong to Task 13."
    });
    fs::write(
        "docs/eval/eval_config.json",
        serde_json::to_string_pretty(&config_json)?,
    )?;

    println!("=== Bitchat 2.0 Phase 6 Final Quantitative Evaluation (Task 12) ===");
    println!(
        "Commit: {} | Seeds: {} ({}..{}) | Link Delay: {}ms | Tick: {}ms | Loss: 5% | MAX_HOPS: {}\n",
        CORE_GIT_COMMIT,
        NUM_SEEDS,
        BASE_SEED,
        BASE_SEED + NUM_SEEDS as u64 - 1,
        LINK_DELAY_MS,
        TIMER_STEP_MS,
        MAX_HOPS
    );

    // 1. Multi-Topology Broadcast Flood & Resource Evaluation
    let topologies = [
        TopologyType::DenseCluster30,
        TopologyType::SparseLine10,
        TopologyType::RandomGeometric50,
        TopologyType::PartitionedBridge31,
        TopologyType::Intermittent20,
        TopologyType::Grid7x7Historical,
    ];
    let policies = [
        PolicyType::Naive,
        PolicyType::Adaptive,
        PolicyType::CoverageAware,
        PolicyType::CoverageResourceBalanced,
        PolicyType::CoverageResourceMinimal,
    ];

    let mut flood_csv = File::create("docs/eval/phase6_flood_raw.csv")?;
    writeln!(
        flood_csv,
        "topology,policy,seed,intended,reachable_within_ttl,delivered,delivery_ratio_total,delivery_ratio_reachable,transmissions,wire_bytes_total,bytes_per_delivered_msg,median_latency_ms,p95_latency_ms,duplicate_receptions,duplicate_displays,relays_suppressed,max_device_tx,mean_device_tx"
    )?;

    let mut flood_chart_rows = Vec::new();
    println!(
        "{:<37} | {:<20} | {:>11} | {:>13} | {:>13} | {:>11} | {:>11} | {:>9} | {:>9}",
        "Topology",
        "Policy",
        "Tx (m±sd)",
        "DelivAll(m±sd)",
        "DelivTTL(m±sd)",
        "MedLat(ms)",
        "P95Lat(ms)",
        "Dups(Rx)",
        "Suppr"
    );
    println!("{:-<158}", "");

    for topo in topologies {
        for pol in policies {
            let mut runs = Vec::with_capacity(NUM_SEEDS);
            for s_idx in 0..NUM_SEEDS {
                let seed = BASE_SEED + s_idx as u64;
                let r = run_flood_simulation(topo, pol, seed).await;
                writeln!(
                    flood_csv,
                    "\"{}\",\"{}\",{},{},{},{},{:.4},{:.4},{},{},{:.1},{:.2},{:.2},{},{},{},{},{:.2}",
                    topo.name(),
                    pol.name(),
                    seed,
                    r.intended,
                    r.reachable_within_ttl,
                    r.delivered,
                    r.delivery_ratio_total,
                    r.delivery_ratio_reachable,
                    r.transmissions,
                    r.wire_bytes_total,
                    r.bytes_per_delivered_msg,
                    r.median_latency_ms,
                    r.p95_latency_ms,
                    r.duplicate_receptions,
                    r.duplicate_displays,
                    r.relays_suppressed,
                    r.max_device_tx,
                    r.mean_device_tx
                )?;
                runs.push(r);
            }
            let (tx_m, tx_sd) = mean_std(
                &runs
                    .iter()
                    .map(|r| r.transmissions as f64)
                    .collect::<Vec<_>>(),
            );
            let (da_m, da_sd) = mean_std(
                &runs
                    .iter()
                    .map(|r| r.delivery_ratio_total * 100.0)
                    .collect::<Vec<_>>(),
            );
            let (dr_m, dr_sd) = mean_std(
                &runs
                    .iter()
                    .map(|r| r.delivery_ratio_reachable * 100.0)
                    .collect::<Vec<_>>(),
            );
            let (ml_m, _) = mean_std(&runs.iter().map(|r| r.median_latency_ms).collect::<Vec<_>>());
            let (pl_m, _) = mean_std(&runs.iter().map(|r| r.p95_latency_ms).collect::<Vec<_>>());
            let (dup_m, _) = mean_std(
                &runs
                    .iter()
                    .map(|r| r.duplicate_receptions as f64)
                    .collect::<Vec<_>>(),
            );
            let (sup_m, _) = mean_std(
                &runs
                    .iter()
                    .map(|r| r.relays_suppressed as f64)
                    .collect::<Vec<_>>(),
            );
            let (b_m, _) = mean_std(
                &runs
                    .iter()
                    .map(|r| r.bytes_per_delivered_msg)
                    .collect::<Vec<_>>(),
            );
            let _ = b_m;

            flood_chart_rows.push((topo.name(), pol.name(), tx_m, dr_m / 100.0));
            println!(
                "{:<37} | {:<20} | {:>5.1}±{:<4.1} | {:>5.1}%±{:<4.1}% | {:>5.1}%±{:<4.1}% | {:>11.1} | {:>11.1} | {:>9.1} | {:>9.1}",
                topo.name(),
                pol.name(),
                tx_m,
                tx_sd,
                da_m,
                da_sd,
                dr_m,
                dr_sd,
                ml_m,
                pl_m,
                dup_m,
                sup_m
            );
        }
        println!("{:-<158}", "");
    }

    // 2. Delivery Retry & Warm vs Fresh Session Ablation
    let mut retry_csv = File::create("docs/eval/phase6_retry_ablation.csv")?;
    writeln!(
        retry_csv,
        "workload,policy,loss_pct,seed,messages_sent,delivered_before_expiry,delivery_ratio,handshake_tx,data_tx,ack_tx,retries,wire_bytes_total,max_queue_occupancy,duplicate_receptions,duplicate_displays,median_latency_ms,p95_latency_ms"
    )?;
    println!(
        "\n=== Unicast E2E Delivery Retry & Session Workload Ablation (2-Hop Relay, 20 Seeds) ==="
    );
    for &warm in &[true, false] {
        for &loss_pct in &[5u32, 20u32] {
            for &max_att in &[1u32, 3u32] {
                let mut rows = Vec::with_capacity(NUM_SEEDS);
                for s_idx in 0..NUM_SEEDS {
                    let seed = BASE_SEED + s_idx as u64;
                    let row = run_unicast_retry_ablation(warm, max_att, loss_pct, seed).await;
                    writeln!(
                        retry_csv,
                        "{},{},{},{},{},{},{:.4},{},{},{},{},{},{},{},{},{:.2},{:.2}",
                        row.workload,
                        row.policy,
                        row.loss_pct,
                        row.seed,
                        row.messages_sent,
                        row.delivered_before_expiry,
                        row.delivery_ratio,
                        row.handshake_tx,
                        row.data_tx,
                        row.ack_tx,
                        row.retries,
                        row.wire_bytes_total,
                        row.max_queue_occupancy,
                        row.duplicate_receptions,
                        row.duplicate_displays,
                        row.median_latency_ms,
                        row.p95_latency_ms
                    )?;
                    rows.push(row);
                }
                let (dr_m, dr_sd) = mean_std(
                    &rows
                        .iter()
                        .map(|r| r.delivery_ratio * 100.0)
                        .collect::<Vec<_>>(),
                );
                let (wb_m, _) = mean_std(
                    &rows
                        .iter()
                        .map(|r| r.wire_bytes_total as f64)
                        .collect::<Vec<_>>(),
                );
                let (ret_m, _) =
                    mean_std(&rows.iter().map(|r| r.retries as f64).collect::<Vec<_>>());
                let (hs_m, _) = mean_std(
                    &rows
                        .iter()
                        .map(|r| r.handshake_tx as f64)
                        .collect::<Vec<_>>(),
                );
                let (ml_m, _) =
                    mean_std(&rows.iter().map(|r| r.median_latency_ms).collect::<Vec<_>>());
                let (pl_m, _) =
                    mean_std(&rows.iter().map(|r| r.p95_latency_ms).collect::<Vec<_>>());
                let (dupr_m, _) = mean_std(
                    &rows
                        .iter()
                        .map(|r| r.duplicate_receptions as f64)
                        .collect::<Vec<_>>(),
                );
                let dup_disp: u64 = rows.iter().map(|r| r.duplicate_displays).sum();
                println!(
                    "{:<13} | loss={:>2}% | {:<13} | deliv={:>5.1}%±{:<4.1}% | hs_tx={:>4.1} | retries={:>4.2} | bytes={:>6.0} | dupRx={:>4.2} | dupUI={} | med={:>5.1}ms | p95={:>5.1}ms",
                    rows[0].workload,
                    loss_pct,
                    rows[0].policy,
                    dr_m,
                    dr_sd,
                    hs_m,
                    ret_m,
                    wb_m,
                    dupr_m,
                    dup_disp,
                    ml_m,
                    pl_m
                );
            }
        }
    }

    // 3. Cover Traffic Experiment
    let mut cover_csv = File::create("docs/eval/phase6_cover_raw.csv")?;
    writeln!(
        cover_csv,
        "mode,seed,real_frames,dummy_frames,total_wire_bytes,dummy_wire_bytes,mean_interval_ms,cv_interval,mean_real_latency_ms"
    )?;
    println!("\n=== Experimental Cover Traffic Evaluation (60s Window, 20 Seeds) ===");
    for &cover_on in &[false, true] {
        let mut rows = Vec::with_capacity(NUM_SEEDS);
        for s_idx in 0..NUM_SEEDS {
            let seed = BASE_SEED + s_idx as u64;
            let r = run_cover_experiment(cover_on, seed);
            writeln!(
                cover_csv,
                "{},{},{},{},{},{},{:.2},{:.4},{:.2}",
                r.mode,
                r.seed,
                r.real_frames,
                r.dummy_frames,
                r.total_wire_bytes,
                r.dummy_wire_bytes,
                r.mean_interval_ms,
                r.cv_interval,
                r.mean_real_latency_ms
            )?;
            rows.push(r);
        }
        let (df_m, df_sd) = mean_std(
            &rows
                .iter()
                .map(|r| r.dummy_frames as f64)
                .collect::<Vec<_>>(),
        );
        let (wb_m, _) = mean_std(
            &rows
                .iter()
                .map(|r| r.total_wire_bytes as f64)
                .collect::<Vec<_>>(),
        );
        let (mi_m, _) = mean_std(&rows.iter().map(|r| r.mean_interval_ms).collect::<Vec<_>>());
        let (cv_m, _) = mean_std(&rows.iter().map(|r| r.cv_interval).collect::<Vec<_>>());
        let (rl_m, _) = mean_std(
            &rows
                .iter()
                .map(|r| r.mean_real_latency_ms)
                .collect::<Vec<_>>(),
        );
        println!(
            "{:<9} | real=5.0 | dummy={:>5.1}±{:<4.1} | total_bytes={:>6.0} | mean_gap={:>7.1}ms | gap_CV={:.3} | real_lat={:.1}ms",
            rows[0].mode, df_m, df_sd, wb_m, mi_m, cv_m, rl_m
        );
    }

    // 4. Sync Protocol Experiment
    let mut sync_csv = File::create("docs/eval/phase6_sync_raw.csv")?;
    writeln!(
        sync_csv,
        "set_size,overlap_pct,inv_bytes,inv_rounds,inv_outcome,merkle_bytes,merkle_rounds,merkle_nodes,merkle_outcome"
    )?;
    println!("\n=== Experimental Conversation Sync Evaluation (IdInventory vs Bounded Merkle) ===");
    let mut sync_chart_rows = Vec::new();
    for &n in &[0usize, 8, 32, 128, 256, 512] {
        for &ov in &[100usize, 95, 50, 0] {
            if n == 0 && ov != 100 {
                continue;
            }
            let (ib, ir, _, io) = run_sync_eval_cell(n, ov, false);
            let (mb, mr, mn, mo) = run_sync_eval_cell(n, ov, true);
            writeln!(
                sync_csv,
                "{},{},{},{},{},{},{},{},{}",
                n, ov, ib, ir, io, mb, mr, mn, mo
            )?;
            sync_chart_rows.push((n, ov, ib, mb));
            println!(
                "n={:<3} ov={:>3}% | IdInv: {:>7} B ({:>2} rds, {:<18}) | Merkle: {:>7} B ({:>3} rds, {:>3} nodes, {:<18})",
                n, ov, ib, ir, io, mb, mr, mn, mo
            );
        }
    }

    write_svg_charts(&flood_chart_rows, &sync_chart_rows)?;
    println!("\nWrote raw CSVs, config, and SVG charts to docs/eval/.");
    Ok(())
}
