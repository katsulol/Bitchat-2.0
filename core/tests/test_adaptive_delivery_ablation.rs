use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use bitchat2_core::api::{BitchatCoreService, BridgeMessage};
use bitchat2_core::envelope::MsgType;
use bitchat2_core::identity::{Identity, PeerId, ShortId};
use bitchat2_core::mesh::delivery::{
    DeliveryAction, DeliveryCoordinator, DeliveryState, EnqueueMessage,
    MAX_CONSECUTIVE_URGENT_DISPATCHES,
};
use bitchat2_core::mesh::resource::{
    RelayResourceProfile, ResourceAdmission, ResourceBudgetManager, TrafficOrigin,
};
use bitchat2_core::storage::DEADLINE_PRESET_2M_SECS;

fn fresh_temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bitchat_ablation_{label}_{}",
        rand::random::<u64>()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

struct NodeHarness {
    svc: BitchatCoreService,
    inbox: Vec<BridgeMessage>,
    neighbors: HashSet<usize>,
    drop_all_outbound: bool,
    total_outbound_bytes: usize,
    dir: PathBuf,
}

impl NodeHarness {
    async fn new(label: &str) -> Self {
        let dir = fresh_temp_dir(label);
        let dek = [0x61u8; 32];
        let mut svc = BitchatCoreService::new_with_key(
            dir.to_str().expect("utf8 path").to_string(),
            Some(dek),
            1,
        )
        .await
        .expect("init BitchatCoreService");
        svc.set_ack_timeout(Duration::from_millis(80));
        Self {
            svc,
            inbox: Vec::new(),
            neighbors: HashSet::new(),
            drop_all_outbound: false,
            total_outbound_bytes: 0,
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

async fn pump_ble(nodes: &mut [NodeHarness], rounds: usize) {
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
                nodes[i].total_outbound_bytes += frame.bytes.len();
                if nodes[i].drop_all_outbound {
                    continue;
                }
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

async fn wait_for_active_connections(svc: &BitchatCoreService, expected: usize) {
    let needle = format!("\"active_connections\":{expected}");
    for _ in 0..80 {
        if let Ok(stats) = svc.gateway_get_stats().await {
            if stats.contains(&needle) {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    let stats = svc.gateway_get_stats().await.unwrap_or_default();
    panic!("Timed out waiting for {needle}, got: {stats}");
}

async fn setup_dual_gw_pair(label: &str) -> (Vec<NodeHarness>, PeerId) {
    let mut nodes = vec![
        NodeHarness::new(&format!("{label}_a")).await,
        NodeHarness::new(&format!("{label}_gw1")).await,
        NodeHarness::new(&format!("{label}_gw2")).await,
        NodeHarness::new(&format!("{label}_b")).await,
    ];

    nodes[0]
        .svc
        .gateway_start(Some("disabled".into()))
        .await
        .unwrap();
    nodes[1]
        .svc
        .gateway_start(Some("disabled".into()))
        .await
        .unwrap();
    nodes[2]
        .svc
        .gateway_start(Some("disabled".into()))
        .await
        .unwrap();

    let c1 = nodes[1].svc.gateway_export_contact().await.unwrap();
    let c2 = nodes[2].svc.gateway_export_contact().await.unwrap();
    nodes[0]
        .svc
        .gateway_import_and_approve_contact(c1.clone())
        .await
        .unwrap();
    nodes[0].svc.gateway_connect_to_contact(c1).await.unwrap();
    nodes[0]
        .svc
        .gateway_import_and_approve_contact(c2.clone())
        .await
        .unwrap();
    nodes[0].svc.gateway_connect_to_contact(c2).await.unwrap();

    wait_for_active_connections(&nodes[0].svc, 2).await;
    wait_for_active_connections(&nodes[1].svc, 1).await;
    wait_for_active_connections(&nodes[2].svc, 1).await;

    connect_ble(&mut nodes, 1, 3).await;
    connect_ble(&mut nodes, 2, 3).await;

    let b_pid = nodes[3].svc.my_peer_id();
    let a_pid = nodes[0].svc.my_peer_id();
    nodes[0].svc.register_verified_peer_id(b_pid);
    nodes[3].svc.register_verified_peer_id(a_pid);

    let b_short = b_pid.short_id().to_string();
    let bridge = nodes[0].svc.gateway_bridge_arc().unwrap();
    for _ in 0..80 {
        pump_ble(&mut nodes, 4).await;
        if bridge.authorized_candidates_for_peer(&b_pid).await.len() >= 2
            && nodes[1].svc.is_session_ready(&b_short).unwrap_or(false)
            && nodes[2].svc.is_session_ready(&b_short).unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }

    nodes[0]
        .svc
        .initiate_handshake(b_short.clone())
        .await
        .unwrap();
    for _ in 0..40 {
        pump_ble(&mut nodes, 5).await;
        if nodes[0].svc.is_session_ready(&b_short).unwrap_or(false)
            && nodes[3]
                .svc
                .is_session_ready(&a_pid.short_id().to_string())
                .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(nodes[0].svc.is_session_ready(&b_short).unwrap());
    (nodes, b_pid)
}

#[tokio::test]
async fn test_t5_combined_adaptive_delivery_story() {
    let (mut nodes, b_pid) = setup_dual_gw_pair("story").await;

    // 1. Send once with explicit 2m deadline
    let msg_id = nodes[0]
        .svc
        .send_message_with_deadline(
            b_pid.to_string(),
            "task5-combined-delivery".into(),
            0,
            Some(DEADLINE_PRESET_2M_SECS),
        )
        .await
        .unwrap();

    // 2. First attempt reaches B via GW1, ACK #1 is lost in flight
    nodes[3].drop_all_outbound = true;
    for _ in 0..40 {
        pump_ble(&mut nodes, 3).await;
        if !nodes[3].inbox.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    pump_ble(&mut nodes, 2).await;
    nodes[3].drop_all_outbound = false;
    assert_eq!(nodes[3].inbox.len(), 1);

    // 3. Automatic alternate gateway retry after ACK timeout -> events carry typed reason/attempts
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(nodes[0].svc.pump_outbox().await.unwrap() >= 1);

    // 4. B deduplicates retry (display remains 1) and returns ACK #2 over GW2 -> Delivered
    let mut delivered = false;
    for _ in 0..40 {
        pump_ble(&mut nodes, 4).await;
        let _ = nodes[0].svc.pump_outbox().await;
        if nodes[0]
            .svc
            .get_delivery_state(msg_id.clone())
            .await
            .unwrap()
            == "Delivered"
        {
            delivered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    assert!(delivered);
    assert_eq!(
        nodes[3].inbox.len(),
        1,
        "B must display once despite GW2 retry"
    );

    let events = nodes[0].svc.poll_delivery_events_vec();
    assert!(events
        .iter()
        .any(|e| e.logical_id == msg_id && e.reason == "RetryingAlternateGateway"));
    assert!(events
        .iter()
        .any(|e| e.logical_id == msg_id && e.state == "Delivered"));

    // 5. Separate short-deadline message to unreachable peer expires truthfully
    let unreachable_pid = Identity::generate().peer_id();
    nodes[0].svc.register_verified_peer_id(unreachable_pid);
    let exp_id = nodes[0]
        .svc
        .send_message_with_deadline(
            unreachable_pid.to_string(),
            "will-expire".into(),
            0,
            Some(1),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1150)).await;
    let _ = nodes[0].svc.pump_outbox().await.unwrap();
    assert_eq!(
        nodes[0]
            .svc
            .get_delivery_state(exp_id.clone())
            .await
            .unwrap(),
        "Expired"
    );

    // 6. Resource profile switching on a 3-node BLE chain affects transit without losing local work
    let mut line = vec![
        NodeHarness::new("t5_x").await,
        NodeHarness::new("t5_r").await,
        NodeHarness::new("t5_y").await,
    ];
    connect_ble(&mut line, 0, 1).await;
    connect_ble(&mut line, 1, 2).await;
    pump_ble(&mut line, 6).await;

    assert_eq!(
        line[1]
            .svc
            .set_relay_contribution_profile("receive_only")
            .unwrap(),
        "Paused"
    );
    line[0]
        .svc
        .send_broadcast("x-blocked".into())
        .await
        .unwrap();
    line[1]
        .svc
        .send_broadcast("r-local-kept".into())
        .await
        .unwrap();
    pump_ble(&mut line, 10).await;
    assert!(!line[2].inbox.iter().any(|m| m.text == "x-blocked"));
    assert!(line[2].inbox.iter().any(|m| m.text == "r-local-kept"));

    assert_eq!(
        line[1]
            .svc
            .set_relay_contribution_profile("balanced")
            .unwrap(),
        "Balanced"
    );
    line[0]
        .svc
        .send_broadcast("x-forwarded".into())
        .await
        .unwrap();
    pump_ble(&mut line, 12).await;
    assert!(line[2].inbox.iter().any(|m| m.text == "x-forwarded"));

    for n in nodes {
        n.cleanup().await;
    }
    for n in line {
        n.cleanup().await;
    }
}

#[tokio::test]
async fn test_t5_matched_ablation_metrics() {
    // Ablation 1: Single-route baseline vs Alternate-gateway recovery across 3 controlled primary-GW failures
    let (mut single_nodes, s_b_pid) = setup_dual_gw_pair("abl_single").await;
    let s_primary = single_nodes[0]
        .svc
        .gateway_bridge_arc()
        .unwrap()
        .authorized_candidates_for_peer(&s_b_pid)
        .await[0]
        .0;
    let s_secondary = single_nodes[0]
        .svc
        .gateway_bridge_arc()
        .unwrap()
        .authorized_candidates_for_peer(&s_b_pid)
        .await[1]
        .0;
    // Remove secondary gateway so only 1 route exists, then break primary outbound
    single_nodes[0]
        .svc
        .gateway_revoke_or_block_peer(s_secondary.to_string(), false)
        .await
        .unwrap();
    single_nodes[0]
        .svc
        .gateway_bridge_arc()
        .unwrap()
        .simulate_broken_outbound(&s_primary)
        .await;

    let mut single_delivered = 0usize;
    let mut single_ids = Vec::new();
    for idx in 0..3 {
        let lid = single_nodes[0]
            .svc
            .send_message_with_deadline(s_b_pid.to_string(), format!("single-{idx}"), 0, Some(1))
            .await
            .unwrap();
        single_ids.push(lid);
    }
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let _ = single_nodes[0].svc.pump_outbox().await;
    pump_ble(&mut single_nodes, 4).await;
    for lid in &single_ids {
        if single_nodes[0]
            .svc
            .get_delivery_state(lid.clone())
            .await
            .unwrap()
            == "Delivered"
        {
            single_delivered += 1;
        }
    }
    assert_eq!(
        single_delivered, 0,
        "Single failed gateway baseline delivers 0/3"
    );

    // Matched alternate-gateway configuration: primary GW fails before admission, GW2 recovers all 3
    let (mut alt_nodes, a_b_pid) = setup_dual_gw_pair("abl_alt").await;
    let a_primary = alt_nodes[0]
        .svc
        .gateway_bridge_arc()
        .unwrap()
        .authorized_candidates_for_peer(&a_b_pid)
        .await[0]
        .0;
    alt_nodes[0]
        .svc
        .gateway_bridge_arc()
        .unwrap()
        .simulate_broken_outbound(&a_primary)
        .await;

    let start_alt = Instant::now();
    let mut alt_delivered = 0usize;
    let mut alt_latencies_ms = Vec::new();
    let mut alt_attempts_total = 0u64;
    for idx in 0..3 {
        let msg_start = Instant::now();
        let lid = alt_nodes[0]
            .svc
            .send_message_with_deadline(
                a_b_pid.to_string(),
                format!("alt-{idx}"),
                0,
                Some(DEADLINE_PRESET_2M_SECS),
            )
            .await
            .unwrap();
        for _ in 0..40 {
            pump_ble(&mut alt_nodes, 4).await;
            let _ = alt_nodes[0].svc.pump_outbox().await;
            if alt_nodes[0]
                .svc
                .get_delivery_state(lid.clone())
                .await
                .unwrap()
                == "Delivered"
            {
                alt_delivered += 1;
                alt_latencies_ms.push(msg_start.elapsed().as_millis());
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let events = alt_nodes[0].svc.poll_delivery_events_vec();
        if let Some(ev) = events
            .iter()
            .rfind(|e| e.logical_id == lid && e.state == "Delivered")
        {
            alt_attempts_total += ev.attempt_count as u64;
        }
    }
    let alt_total_ms = start_alt.elapsed().as_millis();
    assert_eq!(
        alt_delivered, 3,
        "Alternate-gateway recovery must deliver 3/3"
    );

    // Collect encoded wire bytes from BLE + Iroh diagnostics
    let alt_wire_bytes: usize = alt_nodes.iter().map(|n| n.total_outbound_bytes).sum();

    // Ablation 2: Bounded 3:1 EDF Scheduler vs tight capacity
    let local_id = Identity::generate().peer_id();
    let mut edf_coord = DeliveryCoordinator::new(local_id);
    let t0 = Instant::now();
    let mut edf_urgent = Vec::new();
    let mut edf_ordinary = Vec::new();
    for i in 0..6 {
        let p = Identity::generate().peer_id();
        let lid = [0xC0 + i as u8; 16];
        edf_coord.enqueue(
            EnqueueMessage {
                logical_id: lid,
                recipient_peer_id: p,
                recipient_short_id: p.short_id(),
                body: vec![0xAA; 64],
                ttl: Duration::from_secs(30 + (i as u64) * 10),
                max_attempts: 5,
                is_privacy_mode: false,
            },
            t0 + Duration::from_millis(i as u64),
        );
        edf_urgent.push(lid);
    }
    for i in 0..3 {
        let p = Identity::generate().peer_id();
        let lid = [0xD0 + i as u8; 16];
        edf_coord.enqueue(
            EnqueueMessage {
                logical_id: lid,
                recipient_peer_id: p,
                recipient_short_id: p.short_id(),
                body: vec![0xBB; 64],
                ttl: Duration::from_secs(24 * 3600),
                max_attempts: 10,
                is_privacy_mode: false,
            },
            t0 + Duration::from_millis(10 + i as u64),
        );
        edf_ordinary.push(lid);
    }
    let edf_order: Vec<[u8; 16]> = edf_coord
        .check_timeouts(t0 + Duration::from_secs(1))
        .into_iter()
        .filter_map(|a| match a {
            DeliveryAction::ReadyToRetry(id) => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(edf_order.len(), 9);
    // Slot 3 (4th item) promotes ordinary[0] after MAX_CONSECUTIVE_URGENT_DISPATCHES (3)
    assert_eq!(MAX_CONSECUTIVE_URGENT_DISPATCHES, 3);
    assert_eq!(edf_order[3], edf_ordinary[0]);
    // Urgent messages occupy slots [0,1,2,4,5,6] (all dispatched before 2nd/3rd ordinary!)
    let urgent_positions: Vec<usize> = edf_urgent
        .iter()
        .map(|id| edf_order.iter().position(|x| x == id).unwrap())
        .collect();
    assert!(urgent_positions.iter().all(|&pos| pos <= 6));

    // Ablation 3: Resource budget profiles across identical 64 KiB transit burst + 4 KiB local work
    let mut res_rows = Vec::new();
    for profile in [
        RelayResourceProfile::Paused,
        RelayResourceProfile::Minimal,
        RelayResourceProfile::Balanced,
        RelayResourceProfile::Assisted,
    ] {
        let mut mgr = ResourceBudgetManager::new(profile, Duration::from_secs(10), t0);
        let mut transit_admitted_bytes = 0usize;
        let mut transit_dropped_bytes = 0usize;
        let mut local_admitted = 0usize;
        // Spread transit burst across 4 distinct neighbors (2048 B each, 32 packets = 64 KiB)
        for p in 0..32 {
            let nbr = ShortId([(0x10 + (p % 4)) as u8; 8]);
            match mgr.check_and_record(
                TrafficOrigin::TransitRelay { neighbor: nbr },
                MsgType::Chat,
                2048,
                t0,
            ) {
                ResourceAdmission::Admitted { .. } => {
                    transit_admitted_bytes += 2048;
                    mgr.release_queued_item();
                }
                _ => transit_dropped_bytes += 2048,
            }
        }
        // Control ACK admission must still succeed on Minimal/Balanced/Assisted!
        let ctrl_ok = matches!(
            mgr.check_and_record(
                TrafficOrigin::TransitRelay {
                    neighbor: ShortId([0x99; 8]),
                },
                MsgType::Ack,
                256,
                t0,
            ),
            ResourceAdmission::Admitted { .. }
        );
        // Local originated traffic must succeed 100% under ALL profiles including Paused
        for _ in 0..4 {
            if matches!(
                mgr.check_and_record(TrafficOrigin::LocalOriginated, MsgType::Chat, 1024, t0),
                ResourceAdmission::Admitted { .. }
            ) {
                local_admitted += 1;
            }
        }
        assert_eq!(local_admitted, 4);
        if profile != RelayResourceProfile::Paused {
            assert!(ctrl_ok);
        }
        res_rows.push((
            profile,
            transit_admitted_bytes,
            transit_dropped_bytes,
            local_admitted,
        ));
    }
    // Check monotonicity: Paused (0) < Minimal (<=6KiB data) < Balanced (<=24KiB data) < Assisted (<=48KiB data)
    assert_eq!(res_rows[0].1, 0);
    assert!(res_rows[1].1 < res_rows[2].1);
    assert!(res_rows[2].1 < res_rows[3].1);

    println!(
        "[ABLATION_METRICS] single_delivered={}/3 alt_delivered={}/3 alt_attempts={} alt_total_ms={} alt_latencies_ms={:?} alt_ble_wire_bytes={}",
        single_delivered, alt_delivered, alt_attempts_total, alt_total_ms, alt_latencies_ms, alt_wire_bytes
    );
    println!(
        "[ABLATION_EDF] edf_urgent_max_slot={} first_ordinary_slot=3 edf_state_ok=true",
        urgent_positions.iter().copied().max().unwrap_or(0)
    );
    for (prof, adm, drop_b, loc) in &res_rows {
        println!(
            "[ABLATION_RESOURCE] profile={prof:?} transit_admitted_B={adm} transit_dropped_B={drop_b} local_admitted={loc}/4"
        );
    }
    let _ = DeliveryState::Delivered;

    for n in single_nodes {
        n.cleanup().await;
    }
    for n in alt_nodes {
        n.cleanup().await;
    }
}
