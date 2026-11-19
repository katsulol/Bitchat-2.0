use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use bitchat2_core::api::{BitchatCoreService, BridgeMessage};
use bitchat2_core::envelope::{Envelope, Flags, MsgType, CURRENT_VERSION, MAX_HOPS};
use bitchat2_core::gateway::iroh_link::accept_gateway_conn;
use bitchat2_core::gateway::GatewayBridge;
use bitchat2_core::identity::Identity;
use bitchat2_core::mesh::policy::NaiveFlood;
use bitchat2_core::mesh::relay::MeshNode;
use bitchat2_core::mesh::resource::{
    estimate_fragmented_wire_bytes, BatteryBand, NetworkMetering, RelayResourceProfile,
    ResourceAdmission, ResourceBudgetManager, ResourceSnapshot, ThermalState, TrafficOrigin,
    TriState, MAX_TRACKED_NEIGHBOR_WINDOWS,
};
use bitchat2_core::transport::sim::SimNetwork;
use iroh::RelayMode;

fn fresh_temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bitchat_res_{label}_{}", rand::random::<u64>()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

struct NodeHarness {
    svc: BitchatCoreService,
    inbox: Vec<BridgeMessage>,
    neighbors: HashSet<usize>,
    dir: PathBuf,
}

impl NodeHarness {
    async fn new(label: &str) -> Self {
        let dir = fresh_temp_dir(label);
        let dek = [0x31u8; 32];
        let svc = BitchatCoreService::new_with_key(
            dir.to_str().expect("utf8 path").to_string(),
            Some(dek),
            1,
        )
        .await
        .expect("init BitchatCoreService");
        Self {
            svc,
            inbox: Vec::new(),
            neighbors: HashSet::new(),
            dir,
        }
    }

    async fn cleanup(mut self) {
        let _ = self.svc.gateway_stop().await;
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn connect_ble(nodes: &mut [NodeHarness], u: usize, v: usize) {
    nodes[u].neighbors.insert(v);
    nodes[v].neighbors.insert(u);
    let u_short = nodes[u].svc.my_short_id().to_string();
    let v_short = nodes[v].svc.my_short_id().to_string();
    nodes[u].svc.on_peer_connected(v_short).await.unwrap();
    nodes[v].svc.on_peer_connected(u_short).await.unwrap();
}

async fn pump_line(nodes: &mut [NodeHarness], rounds: usize) {
    let shorts: Vec<String> = nodes
        .iter()
        .map(|n| n.svc.my_short_id().to_string())
        .collect();
    let short_to_idx: HashMap<String, usize> = shorts
        .iter()
        .enumerate()
        .map(|(i, s)| (s.clone(), i))
        .collect();

    for _ in 0..rounds {
        for i in 0..nodes.len() {
            while let Some(frame) = nodes[i].svc.poll_outbound() {
                let from_short = shorts[i].clone();
                let nbrs: Vec<usize> = nodes[i].neighbors.iter().copied().collect();
                match frame.to {
                    Some(dest_short) => {
                        if let Some(&j) = short_to_idx.get(&dest_short) {
                            if nodes[i].neighbors.contains(&j) {
                                let _ = nodes[j].svc.on_frame_received(from_short, frame.bytes);
                            }
                        }
                    }
                    None => {
                        for j in nbrs {
                            if j != i {
                                let _ = nodes[j]
                                    .svc
                                    .on_frame_received(from_short.clone(), frame.bytes.clone());
                            }
                        }
                    }
                }
            }
        }

        for node in nodes.iter_mut() {
            for _ in 0..8 {
                match node.svc.poll_inbound().await {
                    Ok(Some(msg)) => node.inbox.push(msg),
                    _ => break,
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(8)).await;
    }
}

#[tokio::test]
async fn test_t4_three_node_line_profile_switch_and_local_originator_protection() {
    // Topology: A (0) <-> Relay R (1) <-> B (2); A and B have no direct link
    let mut nodes = vec![
        NodeHarness::new("line_a").await,
        NodeHarness::new("line_r").await,
        NodeHarness::new("line_b").await,
    ];
    connect_ble(&mut nodes, 0, 1).await;
    connect_ble(&mut nodes, 1, 2).await;
    pump_line(&mut nodes, 8).await;

    // 1. Set Relay R to Receive only (Paused): third-party transit from A must be dropped,
    //    while R's own local-originated message to B STILL delivers!
    let eff = nodes[1]
        .svc
        .set_relay_contribution_profile("receive_only")
        .unwrap();
    assert_eq!(eff, "Paused");

    nodes[0]
        .svc
        .send_broadcast("blocked-transit-broadcast-1".into())
        .await
        .unwrap();
    pump_line(&mut nodes, 12).await;

    assert!(
        !nodes[2]
            .inbox
            .iter()
            .any(|m| m.text == "blocked-transit-broadcast-1"),
        "Receive-only relay must drop A->B third-party transit"
    );
    let diag_paused = nodes[1].svc.get_diagnostics_snapshot().await;
    assert_eq!(diag_paused.relay_contribution_mode, "receive_only");
    assert_eq!(diag_paused.resource_profile, "Paused");
    assert!(
        diag_paused.transit_packets_dropped_policy >= 1,
        "Expected dropped transit policy count >= 1, got {diag_paused:?}"
    );

    // R's own local-originated message MUST still be sent and received by B!
    nodes[1]
        .svc
        .send_broadcast("local-from-relay-r".into())
        .await
        .unwrap();
    pump_line(&mut nodes, 10).await;
    assert!(
        nodes[2]
            .inbox
            .iter()
            .any(|m| m.text == "local-from-relay-r"),
        "Paused relay must NEVER drop local-originated messages"
    );

    // 2. Switch Relay R to Balanced -> third-party transit from A forwards to B with honest byte accounting
    let eff2 = nodes[1]
        .svc
        .set_relay_contribution_profile("balanced")
        .unwrap();
    assert_eq!(eff2, "Balanced");

    nodes[0]
        .svc
        .send_broadcast("allowed-transit-broadcast-2".into())
        .await
        .unwrap();
    pump_line(&mut nodes, 15).await;

    assert!(
        nodes[2]
            .inbox
            .iter()
            .any(|m| m.text == "allowed-transit-broadcast-2"),
        "Balanced relay must forward A->B transit"
    );
    let diag_bal = nodes[1].svc.get_diagnostics_snapshot().await;
    assert_eq!(diag_bal.relay_contribution_mode, "balanced");
    assert_eq!(diag_bal.resource_profile, "Balanced");
    assert!(diag_bal.transit_packets_forwarded >= 1);
    assert!(diag_bal.transit_bytes_forwarded > 0);

    for n in nodes {
        n.cleanup().await;
    }
}

#[tokio::test]
async fn test_t4_thermal_critical_overrides_charging_and_help_network() {
    let mut nodes = vec![
        NodeHarness::new("therm_a").await,
        NodeHarness::new("therm_r").await,
        NodeHarness::new("therm_b").await,
    ];
    connect_ble(&mut nodes, 0, 1).await;
    connect_ble(&mut nodes, 1, 2).await;
    pump_line(&mut nodes, 6).await;

    // User opts into "help_network", device is 100% charged & plugged in,
    // BUT thermal state = 3 (Critical): thermal safety cap MUST force Paused!
    let _ = nodes[1]
        .svc
        .set_relay_contribution_profile("help_network")
        .unwrap();
    let eff = nodes[1].svc.update_resource_state(100, 1, 0, 3, false);
    assert_eq!(
        eff, "Paused",
        "Critical thermal state must override charging=true and help_network override"
    );

    nodes[0]
        .svc
        .send_broadcast("thermal-blocked-transit".into())
        .await
        .unwrap();
    pump_line(&mut nodes, 12).await;
    assert!(
        !nodes[2]
            .inbox
            .iter()
            .any(|m| m.text == "thermal-blocked-transit"),
        "Critical thermal state must halt transit forwarding"
    );

    // Also test explicit Minimal/Balanced/Help profiles do not grant Assisted on charging+WiFi while Paused:
    let snap_wifi_charging_paused = ResourceSnapshot {
        charging: TriState::True,
        battery_band: BatteryBand::High,
        power_save_mode: TriState::False,
        thermal_state: ThermalState::Nominal,
        network_metering: NetworkMetering::Unmetered,
        ble_ready: true,
        is_foreground: true,
        user_paused_relaying: true,
        user_profile_override: Some(RelayResourceProfile::Assisted),
        timestamp: Instant::now(),
    };
    assert_eq!(
        snap_wifi_charging_paused.compute_raw_profile(),
        RelayResourceProfile::Paused
    );

    for n in nodes {
        n.cleanup().await;
    }
}

#[tokio::test]
async fn test_t4_control_reservation_and_gateway_rate_limiter() {
    // 1. Verify ResourceBudgetManager 25% control reservation under fragment/chat burst in Minimal
    let now = Instant::now();
    let mut mgr =
        ResourceBudgetManager::new(RelayResourceProfile::Minimal, Duration::from_secs(10), now);
    let nbr = bitchat2_core::identity::ShortId([0x44; 8]);
    let other_nbr = bitchat2_core::identity::ShortId([0x55; 8]);

    // Consume 75% of 8 KiB/s = 6 KiB across two neighbors
    assert!(matches!(
        mgr.check_and_record(
            TrafficOrigin::TransitRelay { neighbor: nbr },
            MsgType::Chat,
            3072,
            now
        ),
        ResourceAdmission::Admitted { .. }
    ));
    assert!(matches!(
        mgr.check_and_record(
            TrafficOrigin::TransitRelay {
                neighbor: other_nbr
            },
            MsgType::Chat,
            3072,
            now
        ),
        ResourceAdmission::Admitted { .. }
    ));
    assert_eq!(
        mgr.check_and_record(
            TrafficOrigin::TransitRelay {
                neighbor: other_nbr
            },
            MsgType::Chat,
            512,
            now
        ),
        ResourceAdmission::RejectedControlCapacityReserved
    );
    // Reserved 25% control capacity STILL admits Handshake & Ack!
    assert!(matches!(
        mgr.check_and_record(
            TrafficOrigin::TransitRelay {
                neighbor: other_nbr
            },
            MsgType::Handshake,
            512,
            now
        ),
        ResourceAdmission::Admitted { .. }
    ));
    assert!(matches!(
        mgr.check_and_record(
            TrafficOrigin::TransitRelay {
                neighbor: other_nbr
            },
            MsgType::Ack,
            256,
            now
        ),
        ResourceAdmission::Admitted { .. }
    ));

    // 2. Verify live GatewayBridge rate limiter rejects excess > 16 Chat envelopes/min burst
    let id_host = Identity::generate();
    let id_peer = Identity::generate();
    let host = GatewayBridge::new(id_host, None, RelayMode::Disabled)
        .await
        .unwrap();
    host.set_enabled(true).await;

    let peer_bridge = GatewayBridge::new(
        Identity::from_bytes(&id_peer.to_bytes()),
        None,
        RelayMode::Disabled,
    )
    .await
    .unwrap();
    let c_host = host.export_pairing_contact(None, true).await.unwrap();
    let c_host_uri = c_host.export_to_string().unwrap();
    peer_bridge
        .import_and_approve_contact(&c_host_uri)
        .await
        .unwrap();

    let ep = host.endpoint().clone();
    let host_accept = tokio::spawn(async move {
        let (_, conn) = accept_gateway_conn(&ep).await.unwrap();
        conn
    });
    peer_bridge.connect_to_contact(&c_host).await.unwrap();
    let conn = host_accept.await.unwrap();
    host.spawn_session_handler(conn, None).await.unwrap();

    // Send 20 Forward Chat packets rapidly via peer_bridge -> host's GatewayRateLimiter allows 16 and rejects 4
    let sample_env = Envelope {
        version: CURRENT_VERSION,
        msg_type: MsgType::Chat,
        flags: Flags::default(),
        hop_count: 0,
        ttl_max: MAX_HOPS,
        msg_id: [0x11; 8],
        sender_short_id: id_peer.short_id(),
        recipient_short_id: bitchat2_core::identity::ShortId([0x77; 8]),
        payload: b"burst".to_vec(),
    };
    let sample_bytes = sample_env.encode().unwrap();

    for _ in 0..20 {
        peer_bridge
            .forward_mesh_packet_via(host.endpoint_id(), &sample_env, sample_bytes.clone())
            .await
            .unwrap();
    }

    let mut rejected = 0u64;
    let mut imported = 0u64;
    for _ in 0..40 {
        let snap = host.stats().snapshot();
        rejected = snap.forwards_rejected;
        imported = snap.forwards_imported;
        if imported >= 16 && rejected >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }

    assert!(
        imported >= 16,
        "Expected >=16 admitted forwards, got {imported}"
    );
    assert!(
        rejected >= 1,
        "Live GatewayRateLimiter must reject burst > 16 Chat envelopes/min (rejected={rejected})"
    );

    peer_bridge.close().await;
    host.close().await;
}

#[tokio::test]
async fn test_t3_small_mtu_fragment_wire_byte_metering() {
    // 1. Exact fragment byte helper matches Fragmenter split counts (FRAGMENT_HEADER_LEN = 4)
    // mtu=80 => chunk_size=76 => 160B envelope splits into 3 fragments (80 + 80 + 12 = 172 wire bytes)
    assert_eq!(estimate_fragmented_wire_bytes(160, 80), 172);
    assert_eq!(estimate_fragmented_wire_bytes(50, 512), 54);

    // 2. Real MeshNode with small BLE MTU (80B) meters fragmented wire bytes across 3-node chain
    let net = SimNetwork::new(6031);
    let id_a = Identity::generate();
    let id_r = Identity::generate();
    let id_b = Identity::generate();
    let s_a = id_a.short_id();
    let s_r = id_r.short_id();
    let s_b = id_b.short_id();
    net.set_bidirectional_link(s_a, s_r, Duration::ZERO, 0.0);
    net.set_bidirectional_link(s_r, s_b, Duration::ZERO, 0.0);

    let now = Instant::now();
    let budget = std::sync::Arc::new(std::sync::Mutex::new(ResourceBudgetManager::new(
        RelayResourceProfile::Minimal,
        Duration::from_secs(10),
        now,
    )));

    let mut node_a = MeshNode::new(id_a, net.create_node(s_a), Box::new(NaiveFlood)).with_mtu(80);
    let mut node_r = MeshNode::new(id_r, net.create_node(s_r), Box::new(NaiveFlood)).with_mtu(80);
    node_r.set_resource_budget(std::sync::Arc::clone(&budget));
    let mut node_b = MeshNode::new(id_b, net.create_node(s_b), Box::new(NaiveFlood)).with_mtu(80);

    // Send 4 large multi-fragment broadcasts (~400B payload => ~431B encoded => 6 frags * 4B + 431 = 455B wire each).
    // Minimal per-neighbor cap is 4096 B/s: 9 x 455 = 4095 B (all 9 admitted), 10th hits 4550 B > 4096 B!
    for i in 0..10u8 {
        node_a
            .send_broadcast(vec![b'X' + (i % 4); 400])
            .await
            .unwrap();
    }
    for _ in 0..150 {
        while node_r.step_at(now).await.unwrap_or(false) {}
        while node_b.step_at(now).await.unwrap_or(false) {}
    }

    let r_stats = node_r.stats();
    assert!(
        r_stats.transit_bytes_forwarded > 0 && r_stats.transit_packets_dropped_policy >= 1,
        "Small-MTU fragmented forwarding must meter exact fragmented wire bytes and enforce cap: {r_stats:?}"
    );
    assert!(node_b.stats().deliveries <= 9);
}

#[test]
fn test_t3_ack_starvation_resistance_bounded_control_and_churn_cap() {
    let now = Instant::now();
    let mut mgr =
        ResourceBudgetManager::new(RelayResourceProfile::Minimal, Duration::from_secs(10), now);
    let n1 = bitchat2_core::identity::ShortId([0xA1; 8]);
    let n2 = bitchat2_core::identity::ShortId([0xA2; 8]);

    // Exhaust 75% data cap (6144 B in Minimal) with Chat
    assert!(matches!(
        mgr.check_and_record(
            TrafficOrigin::TransitRelay { neighbor: n1 },
            MsgType::Chat,
            3072,
            now
        ),
        ResourceAdmission::Admitted { .. }
    ));
    assert!(matches!(
        mgr.check_and_record(
            TrafficOrigin::TransitRelay { neighbor: n2 },
            MsgType::Chat,
            3072,
            now
        ),
        ResourceAdmission::Admitted { .. }
    ));
    // Data is now rejected to preserve 25% control capacity
    assert_eq!(
        mgr.check_and_record(
            TrafficOrigin::TransitRelay { neighbor: n2 },
            MsgType::Chat,
            256,
            now
        ),
        ResourceAdmission::RejectedControlCapacityReserved
    );

    // Genuine Ack passes without starving
    assert!(matches!(
        mgr.check_and_record(
            TrafficOrigin::TransitRelay { neighbor: n1 },
            MsgType::Ack,
            256,
            now
        ),
        ResourceAdmission::Admitted { .. }
    ));

    // Spoofed control-frame flood from a single neighbor CANNOT bypass per-neighbor fairness indefinitely!
    // Even if total budget is raised to Balanced (32 KiB/s, 16 KiB/s per neighbor), saturating control cap fails cleanly.
    let mut bal_mgr =
        ResourceBudgetManager::new(RelayResourceProfile::Balanced, Duration::from_secs(10), now);
    let attacker = bitchat2_core::identity::ShortId([0xEE; 8]);
    let mut admitted_ctrl = 0usize;
    let mut rejected_ctrl = false;
    for _ in 0..40 {
        match bal_mgr.check_and_record(
            TrafficOrigin::TransitRelay { neighbor: attacker },
            MsgType::Handshake,
            1024,
            now,
        ) {
            ResourceAdmission::Admitted { .. } => {
                admitted_ctrl += 1;
                bal_mgr.release_queued_item();
            }
            ResourceAdmission::RejectedNeighborFairnessExceeded => {
                rejected_ctrl = true;
                break;
            }
            other => panic!("Unexpected rejection: {other:?}"),
        }
    }
    assert!(
        rejected_ctrl && admitted_ctrl == 16,
        "Single-neighbor control flood must be capped (admitted={admitted_ctrl}, rejected={rejected_ctrl})"
    );

    // Identity churn of > 256 neighbors stays cleanly bounded without panicking or growing unboundedly
    for i in 0..(MAX_TRACKED_NEIGHBOR_WINDOWS + 64) {
        let sid = bitchat2_core::identity::ShortId((i as u64).to_be_bytes());
        let _ = bal_mgr.check_and_record(
            TrafficOrigin::TransitRelay { neighbor: sid },
            MsgType::Chat,
            64,
            now + Duration::from_millis(i as u64),
        );
        bal_mgr.release_queued_item();
    }
}

#[test]
fn test_t3_stale_telemetry_automatic_conservative_degradation() {
    let t0 = Instant::now();
    let mut mgr =
        ResourceBudgetManager::new(RelayResourceProfile::Balanced, Duration::from_secs(10), t0);
    mgr.set_telemetry_stale_after(Duration::from_secs(30));

    // Fresh telemetry: charging + high battery + nominal thermal -> Assisted
    let mut snap = ResourceSnapshot::default_at(t0 + Duration::from_secs(15));
    snap.charging = TriState::True;
    snap.battery_band = BatteryBand::High;
    assert_eq!(mgr.update_snapshot(&snap), RelayResourceProfile::Assisted);
    assert_eq!(mgr.active_profile(), RelayResourceProfile::Assisted);

    // Advance past 30s staleness threshold without new OS updates -> conservative downgrade from Assisted -> Balanced
    let stale_now = t0 + Duration::from_secs(50);
    assert_eq!(
        mgr.refresh_stale_telemetry(stale_now),
        RelayResourceProfile::Balanced
    );
}

#[tokio::test]
async fn test_t3_pause_resume_preserves_identity_runtime_and_single_delivery() {
    let mut nodes = vec![
        NodeHarness::new("t3_pr_a").await,
        NodeHarness::new("t3_pr_r").await,
        NodeHarness::new("t3_pr_b").await,
    ];
    connect_ble(&mut nodes, 0, 1).await;
    connect_ble(&mut nodes, 1, 2).await;
    pump_line(&mut nodes, 10).await;

    let r_pid_before = nodes[1].svc.my_peer_id();
    let r_sid_before = nodes[1].svc.my_short_id();

    assert_eq!(
        nodes[1]
            .svc
            .set_relay_contribution_profile("receive_only")
            .unwrap(),
        "Paused"
    );
    nodes[0]
        .svc
        .send_broadcast("t3-paused-drop".into())
        .await
        .unwrap();
    pump_line(&mut nodes, 12).await;
    assert!(!nodes[2].inbox.iter().any(|m| m.text == "t3-paused-drop"));

    assert_eq!(
        nodes[1]
            .svc
            .set_relay_contribution_profile("balanced")
            .unwrap(),
        "Balanced"
    );
    assert_eq!(nodes[1].svc.my_peer_id(), r_pid_before);
    assert_eq!(nodes[1].svc.my_short_id(), r_sid_before);

    nodes[0]
        .svc
        .send_broadcast("t3-resumed-deliver".into())
        .await
        .unwrap();
    pump_line(&mut nodes, 12).await;
    let delivered_count = nodes[2]
        .inbox
        .iter()
        .filter(|m| m.text == "t3-resumed-deliver")
        .count();
    assert_eq!(
        delivered_count, 1,
        "Resumed runtime must deliver exactly once without duplicate runtime or duplicate display"
    );

    for n in nodes {
        n.cleanup().await;
    }
}
