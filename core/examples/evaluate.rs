use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::time::{Duration, Instant};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use bitchat2_core::identity::{Identity, ShortId};
use bitchat2_core::mesh::policy::{AdaptiveFlood, NaiveFlood};
use bitchat2_core::mesh::relay::MeshNode;
use bitchat2_core::transport::sim::SimNetwork;

const NUM_SEEDS: usize = 20;
const LINK_LOSS: f64 = 0.05; // 5% link loss

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TopologyType {
    Line10,
    Grid7x7,
    DenseCluster30,
    RandomGeometric50,
    PartitionedBridge31,
}

impl TopologyType {
    fn name(&self) -> &'static str {
        match self {
            TopologyType::Line10 => "Line (10 nodes)",
            TopologyType::Grid7x7 => "Grid (7x7, 49 nodes)",
            TopologyType::DenseCluster30 => "Dense Cluster (30 nodes)",
            TopologyType::RandomGeometric50 => "Random Geometric (50 nodes)",
            TopologyType::PartitionedBridge31 => "Partitioned Bridge (31 nodes)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PolicyType {
    Naive,
    Adaptive,
}

impl PolicyType {
    fn name(&self) -> &'static str {
        match self {
            PolicyType::Naive => "NaiveFlood",
            PolicyType::Adaptive => "AdaptiveFlood",
        }
    }
}

#[derive(Debug, Clone, Default)]
struct RunResult {
    transmissions: u64,
    delivery_ratio: f64,
    mean_latency_ms: f64,
    p95_latency_ms: f64,
    duplicates: u64,
    suppressed: u64,
}

#[derive(Debug, Clone, Default)]
struct AggregateResult {
    avg_transmissions: f64,
    avg_delivery_ratio: f64,
    avg_mean_latency_ms: f64,
    avg_p95_latency_ms: f64,
    avg_duplicates: f64,
    avg_suppressed: f64,
}

fn build_topology(
    topo: TopologyType,
    seed: u64,
) -> (SimNetwork, Vec<Identity>, Vec<ShortId>, usize) {
    let net = SimNetwork::new(seed);
    let zero_latency = Duration::ZERO;

    match topo {
        TopologyType::Line10 => {
            let count = 10;
            let ids: Vec<Identity> = (0..count).map(|_| Identity::generate()).collect();
            let s_ids: Vec<ShortId> = ids.iter().map(|id| id.short_id()).collect();
            for i in 0..count - 1 {
                net.set_bidirectional_link(s_ids[i], s_ids[i + 1], zero_latency, LINK_LOSS);
            }
            (net, ids, s_ids, count - 1)
        }
        TopologyType::Grid7x7 => {
            let rows = 7;
            let cols = 7;
            let count = rows * cols; // 49
            let ids: Vec<Identity> = (0..count).map(|_| Identity::generate()).collect();
            let s_ids: Vec<ShortId> = ids.iter().map(|id| id.short_id()).collect();

            for r in 0..rows {
                for c in 0..cols {
                    let idx = r * cols + c;
                    // Horizontal link
                    if c + 1 < cols {
                        let right = r * cols + (c + 1);
                        net.set_bidirectional_link(
                            s_ids[idx],
                            s_ids[right],
                            zero_latency,
                            LINK_LOSS,
                        );
                    }
                    // Vertical link
                    if r + 1 < rows {
                        let down = (r + 1) * cols + c;
                        net.set_bidirectional_link(
                            s_ids[idx],
                            s_ids[down],
                            zero_latency,
                            LINK_LOSS,
                        );
                    }
                }
            }
            (net, ids, s_ids, count - 1)
        }
        TopologyType::DenseCluster30 => {
            let count = 30;
            let ids: Vec<Identity> = (0..count).map(|_| Identity::generate()).collect();
            let s_ids: Vec<ShortId> = ids.iter().map(|id| id.short_id()).collect();
            for i in 0..count {
                for j in (i + 1)..count {
                    net.set_bidirectional_link(s_ids[i], s_ids[j], zero_latency, LINK_LOSS);
                }
            }
            (net, ids, s_ids, count - 1)
        }
        TopologyType::RandomGeometric50 => {
            let count = 50;
            let mut rng = StdRng::seed_from_u64(seed.wrapping_add(88888));
            let coords: Vec<(f64, f64)> = (0..count).map(|_| (rng.gen(), rng.gen())).collect();
            let ids: Vec<Identity> = (0..count).map(|_| Identity::generate()).collect();
            let s_ids: Vec<ShortId> = ids.iter().map(|id| id.short_id()).collect();

            let radius = 0.28;
            for i in 0..count {
                for j in (i + 1)..count {
                    let dx = coords[i].0 - coords[j].0;
                    let dy = coords[i].1 - coords[j].1;
                    let dist = (dx * dx + dy * dy).sqrt();
                    if dist <= radius {
                        net.set_bidirectional_link(s_ids[i], s_ids[j], zero_latency, LINK_LOSS);
                    }
                }
            }
            // Ensure origin (0) has at least one neighbor
            if net.neighbors_of(s_ids[0]).is_empty() {
                net.set_bidirectional_link(s_ids[0], s_ids[1], zero_latency, LINK_LOSS);
            }
            (net, ids, s_ids, count - 1)
        }
        TopologyType::PartitionedBridge31 => {
            // Cluster A: 0..15 (15 nodes)
            // Bridge: node 15
            // Cluster B: 16..31 (15 nodes)
            let count = 31;
            let ids: Vec<Identity> = (0..count).map(|_| Identity::generate()).collect();
            let s_ids: Vec<ShortId> = ids.iter().map(|id| id.short_id()).collect();

            for i in 0..15 {
                for j in (i + 1)..15 {
                    net.set_bidirectional_link(s_ids[i], s_ids[j], zero_latency, LINK_LOSS);
                }
            }
            for i in 16..31 {
                for j in (i + 1)..31 {
                    net.set_bidirectional_link(s_ids[i], s_ids[j], zero_latency, LINK_LOSS);
                }
            }
            net.set_bidirectional_link(s_ids[14], s_ids[15], zero_latency, LINK_LOSS);
            net.set_bidirectional_link(s_ids[15], s_ids[16], zero_latency, LINK_LOSS);

            (net, ids, s_ids, count - 1)
        }
    }
}

async fn run_single_simulation(
    topo: TopologyType,
    policy_type: PolicyType,
    seed: u64,
) -> RunResult {
    let (net, ids, s_ids, intended_count) = build_topology(topo, seed);

    let mut nodes: Vec<MeshNode<_>> = ids
        .into_iter()
        .map(|id| {
            let s = id.short_id();
            let t = net.create_node(s);
            match policy_type {
                PolicyType::Naive => MeshNode::new(id, t, Box::new(NaiveFlood)),
                PolicyType::Adaptive => MeshNode::new(id, t, Box::new(AdaptiveFlood::default())),
            }
        })
        .collect();

    let mut sim_time = Instant::now();
    let t0 = sim_time;

    // Origin node (index 0) sends broadcast
    let payload = b"EVALUATION_BROADCAST_PAYLOAD".to_vec();
    if nodes[0].send_broadcast(payload).await.is_err() {
        return RunResult::default();
    }

    let mut delivered_set = HashSet::new();
    let mut latencies: Vec<f64> = Vec::new();

    // Step simulation across 80 discrete time slices (each tick = 10ms of mesh time)
    for _ in 0..80 {
        sim_time += Duration::from_millis(10);
        let mut progress = true;

        // Drain active events at current simulation timestamp
        while progress {
            progress = false;
            for node in nodes.iter_mut() {
                while let Ok(true) = node.poll_step(sim_time).await {
                    progress = true;
                }

                while node.recv_inbound().is_some() {
                    let id = node.short_id();
                    if id != s_ids[0] && delivered_set.insert(id) {
                        let lat = sim_time.duration_since(t0).as_secs_f64() * 1000.0;
                        latencies.push(lat);
                    }
                }
            }
        }
    }

    let total_tx: u64 = nodes.iter().map(|n| n.stats().transmissions).sum();
    let total_dups: u64 = nodes.iter().map(|n| n.stats().duplicates_dropped).sum();
    let total_suppressed: u64 = nodes.iter().map(|n| n.stats().relays_suppressed).sum();

    let delivery_ratio = if intended_count > 0 {
        delivered_set.len() as f64 / intended_count as f64
    } else {
        0.0
    };

    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mean_lat = if !latencies.is_empty() {
        latencies.iter().sum::<f64>() / latencies.len() as f64
    } else {
        0.0
    };

    let p95_lat = if !latencies.is_empty() {
        let idx = ((latencies.len() as f64 * 0.95).ceil() as usize).saturating_sub(1);
        latencies[idx.min(latencies.len() - 1)]
    } else {
        0.0
    };

    RunResult {
        transmissions: total_tx,
        delivery_ratio,
        mean_latency_ms: mean_lat,
        p95_latency_ms: p95_lat,
        duplicates: total_dups,
        suppressed: total_suppressed,
    }
}

fn aggregate(runs: &[RunResult]) -> AggregateResult {
    let n = runs.len() as f64;
    AggregateResult {
        avg_transmissions: runs.iter().map(|r| r.transmissions as f64).sum::<f64>() / n,
        avg_delivery_ratio: runs.iter().map(|r| r.delivery_ratio).sum::<f64>() / n,
        avg_mean_latency_ms: runs.iter().map(|r| r.mean_latency_ms).sum::<f64>() / n,
        avg_p95_latency_ms: runs.iter().map(|r| r.p95_latency_ms).sum::<f64>() / n,
        avg_duplicates: runs.iter().map(|r| r.duplicates as f64).sum::<f64>() / n,
        avg_suppressed: runs.iter().map(|r| r.suppressed as f64).sum::<f64>() / n,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Bitchat 2.0 Evaluation Harness ===");
    println!("Topologies: 5 | Seeds: {} | Link Loss: 5%\n", NUM_SEEDS);

    let topologies = [
        TopologyType::Line10,
        TopologyType::Grid7x7,
        TopologyType::DenseCluster30,
        TopologyType::RandomGeometric50,
        TopologyType::PartitionedBridge31,
    ];

    let policies = [PolicyType::Naive, PolicyType::Adaptive];

    let mut csv_file = File::create("evaluation_results.csv")?;
    writeln!(
        csv_file,
        "topology,policy,seed,transmissions,delivery_ratio,mean_latency_ms,p95_latency_ms,duplicates,relays_suppressed"
    )?;

    println!(
        "{:<28} | {:<14} | {:>10} | {:>10} | {:>10} | {:>10} | {:>10} | {:>10}",
        "Topology",
        "Policy",
        "Avg Tx",
        "Deliv %",
        "Mean Lat",
        "P95 Lat",
        "Duplicates",
        "Suppressed"
    );
    println!("{:-<120}", "");

    for topo in topologies {
        for policy in policies {
            let mut runs = Vec::with_capacity(NUM_SEEDS);

            for seed_idx in 0..NUM_SEEDS {
                let seed = 1000 + seed_idx as u64;
                let res = run_single_simulation(topo, policy, seed).await;

                writeln!(
                    csv_file,
                    "\"{}\",\"{}\",{},{},{:.4},{:.2},{:.2},{},{}",
                    topo.name(),
                    policy.name(),
                    seed,
                    res.transmissions,
                    res.delivery_ratio,
                    res.mean_latency_ms,
                    res.p95_latency_ms,
                    res.duplicates,
                    res.suppressed
                )?;

                runs.push(res);
            }

            let agg = aggregate(&runs);
            println!(
                "{:<28} | {:<14} | {:>10.1} | {:>9.1}% | {:>8.1}ms | {:>8.1}ms | {:>10.1} | {:>10.1}",
                topo.name(),
                policy.name(),
                agg.avg_transmissions,
                agg.avg_delivery_ratio * 100.0,
                agg.avg_mean_latency_ms,
                agg.avg_p95_latency_ms,
                agg.avg_duplicates,
                agg.avg_suppressed
            );
        }
        println!("{:-<120}", "");
    }

    println!("\nEvaluation suite complete. Results written to evaluation_results.csv");
    Ok(())
}
