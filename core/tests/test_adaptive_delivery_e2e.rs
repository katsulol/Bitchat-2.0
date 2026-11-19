use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bitchat2_core::api::{BitchatCoreService, BridgeMessage};
use bitchat2_core::gateway::wire::{RouteRecipient, RouteSnapshot, WIRE_VERSION};
use bitchat2_core::gateway::GatewayBridge;
use bitchat2_core::identity::{Identity, PeerId};
use bitchat2_core::mesh::delivery::{
    ApplicationFrame, DedupStatus, DeliveryAction, DeliveryCoordinator, DeliveryState,
    EnqueueMessage, InboxDeduplicator, MAX_CONSECUTIVE_URGENT_DISPATCHES,
};
use bitchat2_core::storage::{
    InjectedKeyProvider, MemoryOutboxStore, MessageMode, OutboxLimits, OutboxManager, OutboxRecord,
    OutboxStore, SqliteOutboxStore, DEADLINE_PRESET_15M_SECS, DEADLINE_PRESET_1H_SECS,
    DEADLINE_PRESET_2M_SECS,
};
use iroh::RelayMode;

fn fresh_temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bitchat_adaptive_{label}_{}",
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
    dir: PathBuf,
}

impl NodeHarness {
    async fn new(label: &str) -> Self {
        let dir = fresh_temp_dir(label);
        let dek = [0x42u8; 32];
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

async fn pump_ble_and_inbound(nodes: &mut [NodeHarness], rounds: usize) {
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
        let mut activity = false;
        for i in 0..nodes.len() {
            while let Some(frame) = nodes[i].svc.poll_outbound() {
                activity = true;
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
                    Ok(Some(msg)) => {
                        activity = true;
                        node.inbox.push(msg);
                    }
                    Ok(None) => {
                        if !activity {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }

        tokio::task::yield_now().await;
        if !activity {
            tokio::time::sleep(Duration::from_millis(8)).await;
        }
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

async fn wait_for_dual_gateway_candidates(
    nodes: &mut [NodeHarness],
    recipient: &PeerId,
) -> Vec<iroh::EndpointId> {
    let b_short = recipient.short_id().to_string();
    for _ in 0..80 {
        pump_ble_and_inbound(nodes, 4).await;
        if let Some(bridge) = nodes[0].svc.gateway_bridge_arc() {
            let candidates = bridge.authorized_candidates_for_peer(recipient).await;
            if candidates.len() >= 2
                && nodes[1].svc.is_session_ready(&b_short).unwrap_or(false)
                && nodes[2].svc.is_session_ready(&b_short).unwrap_or(false)
            {
                return candidates.into_iter().map(|(ep, _, _)| ep).collect();
            }
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    let candidates = nodes[0]
        .svc
        .gateway_bridge_arc()
        .unwrap()
        .authorized_candidates_for_peer(recipient)
        .await;
    panic!(
        "Timed out waiting for >= 2 gateway candidates, got {}",
        candidates.len()
    );
}

/// Shared 4-node harness:
/// nodes[0] = Sender A (gateway to GW1 & GW2, no direct BLE to B)
/// nodes[1] = GW1 (gateway + direct BLE to B)
/// nodes[2] = GW2 (gateway + direct BLE to B)
/// nodes[3] = Recipient B (direct BLE to GW1 & GW2)
async fn setup_dual_gateway_topology(
    label: &str,
) -> (Vec<NodeHarness>, PeerId, Vec<iroh::EndpointId>) {
    let mut nodes = vec![
        NodeHarness::new(&format!("{label}_a")).await,
        NodeHarness::new(&format!("{label}_gw1")).await,
        NodeHarness::new(&format!("{label}_gw2")).await,
        NodeHarness::new(&format!("{label}_b")).await,
    ];

    nodes[0]
        .svc
        .gateway_start(Some("disabled".to_string()))
        .await
        .unwrap();
    nodes[1]
        .svc
        .gateway_start(Some("disabled".to_string()))
        .await
        .unwrap();
    nodes[2]
        .svc
        .gateway_start(Some("disabled".to_string()))
        .await
        .unwrap();

    let c_gw1 = nodes[1].svc.gateway_export_contact().await.unwrap();
    let c_gw2 = nodes[2].svc.gateway_export_contact().await.unwrap();

    nodes[0]
        .svc
        .gateway_import_and_approve_contact(c_gw1.clone())
        .await
        .unwrap();
    nodes[0]
        .svc
        .gateway_connect_to_contact(c_gw1)
        .await
        .unwrap();

    nodes[0]
        .svc
        .gateway_import_and_approve_contact(c_gw2.clone())
        .await
        .unwrap();
    nodes[0]
        .svc
        .gateway_connect_to_contact(c_gw2)
        .await
        .unwrap();

    // Wait until both GW1 and GW2 finish server-side session registration before BLE announcements
    wait_for_active_connections(&nodes[0].svc, 2).await;
    wait_for_active_connections(&nodes[1].svc, 1).await;
    wait_for_active_connections(&nodes[2].svc, 1).await;

    // Connect BLE links GW1 <-> B and GW2 <-> B so real Noise XX verifies B on both gateways
    connect_ble(&mut nodes, 1, 3).await;
    connect_ble(&mut nodes, 2, 3).await;

    let b_peer_id = nodes[3].svc.my_peer_id();
    let a_peer_id = nodes[0].svc.my_peer_id();
    nodes[0].svc.register_verified_peer_id(b_peer_id);
    nodes[3].svc.register_verified_peer_id(a_peer_id);

    let gws = wait_for_dual_gateway_candidates(&mut nodes, &b_peer_id).await;

    // Pre-establish end-to-end Noise XX session A <-> B via GW2 -> verify ready before data tests
    nodes[0]
        .svc
        .initiate_handshake(b_peer_id.short_id().to_string())
        .await
        .unwrap();
    let mut session_ok = false;
    for _ in 0..40 {
        pump_ble_and_inbound(&mut nodes, 6).await;
        if nodes[0]
            .svc
            .is_session_ready(&b_peer_id.short_id().to_string())
            .unwrap()
            && nodes[3]
                .svc
                .is_session_ready(&a_peer_id.short_id().to_string())
                .unwrap()
        {
            session_ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    assert!(
        session_ok,
        "A <-> B end-to-end Noise XX session must complete via gateways"
    );

    (nodes, b_peer_id, gws)
}

#[tokio::test]
async fn test_t1_gw1_fails_before_admission_gw2_succeeds() {
    let (mut nodes, b_peer_id, gws) = setup_dual_gateway_topology("t1_1").await;
    let primary_gw = gws[0];

    // Simulate primary gateway outbound writer breaking before admission
    nodes[0]
        .svc
        .gateway_bridge_arc()
        .unwrap()
        .simulate_broken_outbound(&primary_gw)
        .await;

    let logical_id = nodes[0]
        .svc
        .send_message(b_peer_id.to_string(), "via fallback gw2".into(), 0)
        .await
        .expect("send_message");

    // Pump B & A through remaining healthy gateway until Delivered
    let mut delivered = false;
    for _ in 0..40 {
        pump_ble_and_inbound(&mut nodes, 5).await;
        let _ = nodes[0].svc.pump_outbox().await;
        if nodes[0]
            .svc
            .get_delivery_state(logical_id.clone())
            .await
            .unwrap()
            == "Delivered"
        {
            delivered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }

    assert!(delivered, "Message should succeed via secondary gateway");
    assert_eq!(nodes[3].inbox.len(), 1);
    assert_eq!(nodes[3].inbox[0].text, "via fallback gw2");

    for n in nodes {
        n.cleanup().await;
    }
}

#[tokio::test]
async fn test_t1_gw1_delivers_ack_lost_gw2_retry_deduped_single_display() {
    let (mut nodes, b_peer_id, _gws) = setup_dual_gateway_topology("t1_2").await;

    let logical_id = nodes[0]
        .svc
        .send_message(b_peer_id.to_string(), "ack-loss-dedup-test".into(), 0)
        .await
        .expect("send_message");

    // Deliver attempt 1 (GW1) to B, but drop B's ACK frames on BLE so A times out
    nodes[3].drop_all_outbound = true;
    let mut b_received = false;
    for _ in 0..40 {
        pump_ble_and_inbound(&mut nodes, 3).await;
        if !nodes[3].inbox.is_empty() {
            b_received = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    // Flush B's queued ACK #1 while drop_all_outbound is still active, then restore
    pump_ble_and_inbound(&mut nodes, 2).await;
    nodes[3].drop_all_outbound = false;
    assert!(b_received, "B must receive first attempt via primary GW");
    assert_eq!(nodes[3].inbox.len(), 1);
    assert_eq!(
        nodes[0]
            .svc
            .get_delivery_state(logical_id.clone())
            .await
            .unwrap(),
        "InFlight"
    );

    // Wait past ACK timeout -> pump_outbox retries sequentially on alternate GW2
    tokio::time::sleep(Duration::from_millis(120)).await;
    let pumped = nodes[0].svc.pump_outbox().await.unwrap();
    assert!(pumped >= 1, "Retry should fire on GW2 after ACK timeout");

    let events = nodes[0].svc.poll_delivery_events_vec();
    assert!(
        events
            .iter()
            .any(|e| e.logical_id == logical_id && e.reason.contains("RetryingAlternateGateway")),
        "Expected RetryingAlternateGateway event, got {events:?}"
    );

    // Now let ACK #2 succeed over GW2; B must suppress duplicate display
    let mut delivered = false;
    for _ in 0..40 {
        pump_ble_and_inbound(&mut nodes, 5).await;
        let _ = nodes[0].svc.pump_outbox().await;
        if nodes[0]
            .svc
            .get_delivery_state(logical_id.clone())
            .await
            .unwrap()
            == "Delivered"
        {
            delivered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }

    assert!(
        delivered,
        "Second attempt over GW2 must deliver and receive ACK"
    );
    assert_eq!(
        nodes[3].inbox.len(),
        1,
        "Duplicate reception at B must display at most once"
    );

    for n in nodes {
        n.cleanup().await;
    }
}

#[tokio::test]
async fn test_t1_both_gateways_unavailable_queued_no_busy_loop() {
    let mut sender = NodeHarness::new("t1_3_a").await;
    let target = Identity::generate().peer_id();
    sender.svc.register_verified_peer_id(target);
    sender
        .svc
        .gateway_start(Some("disabled".to_string()))
        .await
        .unwrap();

    let logical_id = sender
        .svc
        .send_message(target.to_string(), "offline queued".into(), 0)
        .await
        .unwrap();

    assert_eq!(
        sender
            .svc
            .get_delivery_state(logical_id.clone())
            .await
            .unwrap(),
        "Queued"
    );
    let first_events = sender.svc.poll_delivery_events_vec();
    assert!(!first_events.is_empty());

    // Repeated pumps during backoff must stay Queued, burn 0 attempts, and avoid duplicate event spam
    for _ in 0..5 {
        let handled = sender.svc.pump_outbox().await.unwrap();
        assert_eq!(handled, 0);
        assert_eq!(
            sender
                .svc
                .get_delivery_state(logical_id.clone())
                .await
                .unwrap(),
            "Queued"
        );
        let more_events = sender.svc.poll_delivery_events_vec();
        assert!(
            more_events.is_empty(),
            "No duplicate Queued events should spam during backoff: {more_events:?}"
        );
    }

    sender.cleanup().await;
}

#[tokio::test]
async fn test_t1_gw1_revoked_during_pending_attempt_switches_to_gw2() {
    let (mut nodes, b_peer_id, gws) = setup_dual_gateway_topology("t1_4").await;
    let primary_gw = gws[0];

    let logical_id = nodes[0]
        .svc
        .send_message(b_peer_id.to_string(), "revoke-gw1-switch".into(), 0)
        .await
        .unwrap();

    // Deliver attempt 1 to B while dropping B's first ACK so attempt stays pending
    nodes[3].drop_all_outbound = true;
    for _ in 0..40 {
        pump_ble_and_inbound(&mut nodes, 3).await;
        if !nodes[3].inbox.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    pump_ble_and_inbound(&mut nodes, 2).await;
    nodes[3].drop_all_outbound = false;
    assert_eq!(nodes[3].inbox.len(), 1);

    // Revoke primary gateway during pending attempt
    nodes[0]
        .svc
        .gateway_revoke_or_block_peer(primary_gw.to_string(), false)
        .await
        .unwrap();

    let remaining = nodes[0]
        .svc
        .gateway_bridge_arc()
        .unwrap()
        .authorized_candidates_for_peer(&b_peer_id)
        .await;
    assert_eq!(remaining.len(), 1);
    assert_ne!(remaining[0].0, primary_gw);

    // Wait past ACK timeout -> pump_outbox switches cleanly to GW2
    tokio::time::sleep(Duration::from_millis(120)).await;
    let pumped = nodes[0].svc.pump_outbox().await.unwrap();
    assert!(pumped >= 1);

    let mut delivered = false;
    for _ in 0..40 {
        pump_ble_and_inbound(&mut nodes, 5).await;
        let _ = nodes[0].svc.pump_outbox().await;
        if nodes[0]
            .svc
            .get_delivery_state(logical_id.clone())
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
    assert_eq!(nodes[3].inbox.len(), 1);

    for n in nodes {
        n.cleanup().await;
    }
}

#[tokio::test]
async fn test_t1_route_expiry_forged_snapshot_and_wrong_peer_ack_rejected() {
    let id_gw = Identity::generate();
    let gw = GatewayBridge::new(id_gw, None, RelayMode::Disabled)
        .await
        .unwrap();
    let gw_ep = gw.endpoint_id();

    let victim_id = Identity::generate();
    let fake_author = Identity::generate();
    let recipient_pid = Identity::generate().peer_id();

    // 1. Expired route lease must not be returned by candidates_for_peer_at
    let now = Instant::now();
    {
        let routes = gw.routes();
        let mut table = routes.write().await;
        table
            .apply_snapshot_with_peers(
                gw_ep,
                1,
                1,
                vec![(recipient_pid.short_id(), Some(recipient_pid))],
                Duration::from_secs(5),
                now,
            )
            .unwrap();
        assert_eq!(
            table
                .candidates_for_peer_at(&recipient_pid, now + Duration::from_secs(1), |_| true)
                .len(),
            1
        );
        assert!(table
            .candidates_for_peer_at(&recipient_pid, now + Duration::from_secs(6), |_| true)
            .is_empty());
    }

    // 2. Forged RouteSnapshot (signed by wrong identity / tampered signature) must be rejected
    let mut forged = RouteSnapshot {
        protocol_version: WIRE_VERSION,
        session_epoch: 2,
        sequence_number: 1,
        lease_seconds: 120,
        recipients: vec![RouteRecipient {
            short_id_hex: victim_id.short_id().to_string(),
            public_key_hex: Some(victim_id.peer_id().to_string()),
        }],
        signature: Vec::new(),
    };
    forged.sign(&fake_author);
    assert!(forged.verify_author(&victim_id.peer_id()).is_err());

    // 3. Wrong-peer ACK must not mark Bob's message Delivered in coordinator or SQLite
    let bob_id = Identity::generate().peer_id();
    let eve_id = Identity::generate().peer_id();
    let alice_id = Identity::generate().peer_id();
    let kp = Arc::new(InjectedKeyProvider::random(1));
    let sqlite = SqliteOutboxStore::in_memory(kp, OutboxLimits::default()).unwrap();
    let mut outbox = OutboxManager::new(
        alice_id,
        sqlite,
        MemoryOutboxStore::default(),
        OutboxLimits::default(),
    );
    let mut coord = DeliveryCoordinator::new(alice_id);
    let logical_id =
        match outbox.send_to(bob_id, b"secret".to_vec(), MessageMode::Normal, None, 100) {
            bitchat2_core::storage::SendResult::Accepted { logical_id, .. } => logical_id,
            other => panic!("unexpected: {other:?}"),
        };
    coord.enqueue(
        EnqueueMessage {
            logical_id,
            recipient_peer_id: bob_id,
            recipient_short_id: bob_id.short_id(),
            body: b"secret".to_vec(),
            ttl: Duration::from_secs(600),
            max_attempts: 5,
            is_privacy_mode: false,
        },
        now,
    );
    coord
        .mark_in_flight(&logical_id, Duration::from_secs(5), now)
        .unwrap();
    outbox.mark_in_flight(&logical_id, 101).unwrap();

    // Eve signs/sends ACK claiming logical_id addressed to Bob
    let forged_ack = ApplicationFrame::new_ack(logical_id, eve_id, alice_id, 102);
    assert!(coord.handle_ack(&forged_ack, &eve_id).is_err());
    assert_eq!(coord.get_state(&logical_id), Some(DeliveryState::InFlight));
    assert_eq!(
        outbox.get(&logical_id).unwrap().unwrap().state,
        DeliveryState::InFlight
    );
}

#[tokio::test]
async fn test_t1_sender_restart_rehydrates_normal_and_privacy_zero_disk_plus_receiver_dedup_scope()
{
    let sender_dir = fresh_temp_dir("t1_6_sender");
    let dek = [0x99u8; 32];
    let normal_secret_text = "normal-persist-secret-payload-991";
    let privacy_secret_text = "privacy-zero-disk-secret-774";

    let mut gw = NodeHarness::new("t1_6_gw").await;
    let b_node = NodeHarness::new("t1_6_b").await;
    let b_peer_id = b_node.svc.my_peer_id();

    let (normal_id, privacy_id) = {
        let mut sender = BitchatCoreService::new_with_key(
            sender_dir.to_str().unwrap().to_string(),
            Some(dek),
            1,
        )
        .await
        .unwrap();
        sender.register_verified_peer_id(b_peer_id);

        let n_id = sender
            .send_message(b_peer_id.to_string(), normal_secret_text.into(), 0)
            .await
            .unwrap();
        let p_id = sender
            .send_message(b_peer_id.to_string(), privacy_secret_text.into(), 1)
            .await
            .unwrap();

        assert_eq!(
            sender.get_delivery_state(n_id.clone()).await.unwrap(),
            "Queued"
        );
        assert_eq!(
            sender.get_delivery_state(p_id.clone()).await.unwrap(),
            "Queued"
        );
        (n_id, p_id)
    };

    // Inspect raw SQLite bytes on disk: neither plaintext normal nor ANY privacy record may exist in clear
    let db_bytes = std::fs::read(sender_dir.join("bitchat_outbox.db")).unwrap();
    assert!(
        !db_bytes
            .windows(normal_secret_text.len())
            .any(|w| w == normal_secret_text.as_bytes()),
        "Normal plaintext must be AEAD encrypted at rest in SQLite"
    );
    assert!(
        !db_bytes
            .windows(privacy_secret_text.len())
            .any(|w| w == privacy_secret_text.as_bytes()),
        "Privacy payload must never touch disk"
    );

    // Restart sender service: Normal message rehydrates; Privacy message is gone (zero disk)
    let mut restarted = NodeHarness {
        svc: BitchatCoreService::new_with_key(
            sender_dir.to_str().unwrap().to_string(),
            Some(dek),
            1,
        )
        .await
        .unwrap(),
        inbox: Vec::new(),
        neighbors: HashSet::new(),
        drop_all_outbound: false,
        dir: sender_dir.clone(),
    };
    restarted.svc.set_ack_timeout(Duration::from_millis(80));

    assert_eq!(
        restarted
            .svc
            .get_delivery_state(normal_id.clone())
            .await
            .unwrap(),
        "Queued"
    );
    assert!(
        restarted.svc.get_delivery_state(privacy_id).await.is_err(),
        "Privacy message must not persist across restart"
    );

    // Connect restarted sender -> GW -> B over BLE/Iroh and deliver rehydrated Normal message
    let gw_ep = gw
        .svc
        .gateway_start(Some("disabled".to_string()))
        .await
        .unwrap();
    let _ = gw_ep;
    restarted
        .svc
        .gateway_start(Some("disabled".to_string()))
        .await
        .unwrap();
    let c_gw = gw.svc.gateway_export_contact().await.unwrap();
    restarted
        .svc
        .gateway_import_and_approve_contact(c_gw.clone())
        .await
        .unwrap();
    restarted
        .svc
        .gateway_connect_to_contact(c_gw)
        .await
        .unwrap();
    wait_for_active_connections(&gw.svc, 1).await;

    let mut nodes = vec![restarted, gw, b_node];
    connect_ble(&mut nodes, 1, 2).await;
    pump_ble_and_inbound(&mut nodes, 12).await;

    let sender_peer_id = nodes[0].svc.my_peer_id();
    nodes[2].svc.register_verified_peer_id(sender_peer_id);

    // Wait for B's route on sender
    let bridge = nodes[0].svc.gateway_bridge_arc().unwrap();
    for _ in 0..100 {
        pump_ble_and_inbound(&mut nodes, 4).await;
        if !bridge
            .authorized_candidates_for_peer(&b_peer_id)
            .await
            .is_empty()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let mut delivered = false;
    for _ in 0..100 {
        let _ = nodes[0].svc.pump_outbox().await;
        pump_ble_and_inbound(&mut nodes, 5).await;
        if nodes[0]
            .svc
            .get_delivery_state(normal_id.clone())
            .await
            .unwrap()
            == "Delivered"
        {
            delivered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        delivered,
        "Rehydrated Normal message must deliver after restart"
    );
    assert_eq!(nodes[2].inbox.len(), 1);
    assert_eq!(nodes[2].inbox[0].text, normal_secret_text);

    // Verify receiver InboxDeduplicator is in-memory per-process (same process deduplicates;
    // fresh InboxDeduplicator instance after receiver restart treats unseen ID as New).
    let mut dedup = InboxDeduplicator::default();
    let lid_bytes = [0xABu8; 16];
    let now = Instant::now();
    assert_eq!(
        dedup.check_or_insert(&sender_peer_id, &lid_bytes, now),
        DedupStatus::New
    );
    assert_eq!(
        dedup.check_or_insert(&sender_peer_id, &lid_bytes, now),
        DedupStatus::Duplicate
    );
    let mut restarted_dedup = InboxDeduplicator::default();
    assert_eq!(
        restarted_dedup.check_or_insert(&sender_peer_id, &lid_bytes, now),
        DedupStatus::New
    );

    for n in nodes {
        n.cleanup().await;
    }
}

#[tokio::test]
async fn test_t1_privacy_mode_forbids_relay_gateways_no_silent_downgrade() {
    let mut sender = NodeHarness::new("t1_7_a").await;
    let mut gw = NodeHarness::new("t1_7_gw").await;

    // RelayMode::Default -> relay enabled (gateway_relay_disabled == false)
    sender.svc.gateway_start(None).await.unwrap();
    gw.svc.gateway_start(None).await.unwrap();

    let diag = sender.svc.get_diagnostics_snapshot().await;
    assert!(
        !diag.gateway_relay_disabled,
        "Default RelayMode must report gateway_relay_disabled=false"
    );

    let c_gw = gw.svc.gateway_export_contact().await.unwrap();
    sender
        .svc
        .gateway_import_and_approve_contact(c_gw.clone())
        .await
        .unwrap();
    sender.svc.gateway_connect_to_contact(c_gw).await.unwrap();

    let target = Identity::generate().peer_id();
    sender.svc.register_verified_peer_id(target);
    let gw_ep = gw.svc.gateway_bridge_arc().unwrap().endpoint_id();
    sender
        .svc
        .gateway_set_route_for_peer(target, gw_ep)
        .await
        .unwrap();

    let privacy_id = sender
        .svc
        .send_message(target.to_string(), "privacy-no-relay".into(), 1)
        .await
        .unwrap();

    assert_eq!(
        sender
            .svc
            .get_delivery_state(privacy_id.clone())
            .await
            .unwrap(),
        "Queued"
    );
    let events = sender.svc.poll_delivery_events_vec();
    assert!(
        events
            .iter()
            .any(|e| e.logical_id == privacy_id && e.reason.contains("PrivacyRelayRestricted")),
        "Expected PrivacyRelayRestricted event, got {events:?}"
    );
    let gw_stats = sender.svc.gateway_get_stats().await.unwrap();
    assert!(
        gw_stats.contains("\"messages_exported\":0"),
        "Privacy message must not silently downgrade over relay gateway: {gw_stats}"
    );

    sender.cleanup().await;
    gw.cleanup().await;
}

#[tokio::test]
async fn test_t1_unverified_short_id_cannot_fabricate_synthetic_peer_id() {
    let mut sender = NodeHarness::new("t1_8_a").await;
    let unverified_short = "deadbeefcafebabe".to_string();
    sender
        .svc
        .on_peer_connected(unverified_short.clone())
        .await
        .unwrap();

    let send_err = sender
        .svc
        .send_message(unverified_short.clone(), "blocked".into(), 0)
        .await;
    assert!(send_err.is_err());

    let vol_err = sender
        .svc
        .set_volatile_session(unverified_short.clone(), 120)
        .await;
    assert!(vol_err.is_err());

    let prox = sender
        .svc
        .get_peer_proximity(unverified_short)
        .await
        .unwrap();
    assert_eq!(prox, "Unknown");

    sender.cleanup().await;
}

#[tokio::test]
async fn test_t2_deadline_expiry_queued_inflight_late_ack_immutable_and_cancel() {
    // (a) Queued message expires on short deadline (1s) -> SQLite becomes Expired
    let mut offline_a = NodeHarness::new("t2_9_a_off").await;
    let b_pid = Identity::generate().peer_id();
    offline_a.svc.register_verified_peer_id(b_pid);

    let q_id = offline_a
        .svc
        .send_message_with_deadline(b_pid.to_string(), "short-queued".into(), 0, Some(1))
        .await
        .unwrap();
    assert_eq!(
        offline_a
            .svc
            .get_delivery_state(q_id.clone())
            .await
            .unwrap(),
        "Queued"
    );

    tokio::time::sleep(Duration::from_millis(1150)).await;
    let _ = offline_a.svc.pump_outbox().await.unwrap();
    assert_eq!(
        offline_a
            .svc
            .get_delivery_state(q_id.clone())
            .await
            .unwrap(),
        "Expired"
    );
    let q_events = offline_a.svc.poll_delivery_events_vec();
    assert!(q_events
        .iter()
        .any(|e| e.logical_id == q_id && e.state == "Expired"));

    // Cancel test (non-terminal -> Cancelled, immune to dispatch & late ACK)
    let cancel_id = offline_a
        .svc
        .send_message_with_deadline(
            b_pid.to_string(),
            "cancel-before-send".into(),
            0,
            Some(DEADLINE_PRESET_2M_SECS),
        )
        .await
        .unwrap();
    assert!(offline_a
        .svc
        .cancel_message(cancel_id.clone())
        .await
        .unwrap());
    assert_eq!(
        offline_a
            .svc
            .get_delivery_state(cancel_id.clone())
            .await
            .unwrap(),
        "Cancelled"
    );
    // Repeated cancel returns Ok(false)
    assert!(!offline_a.svc.cancel_message(cancel_id).await.unwrap());
    offline_a.cleanup().await;

    // (b) In-flight expiry across restart/live dual-gateway + late ACK immutability
    let (mut nodes, b_peer_id, _gws) = setup_dual_gateway_topology("t2_9_b").await;
    let inf_id = nodes[0]
        .svc
        .send_message_with_deadline(b_peer_id.to_string(), "expire-inflight".into(), 0, Some(1))
        .await
        .unwrap();

    // Deliver to B while holding/dropping B's outbound ACK briefly? Even better: drop first ACK,
    // wait > 1s so InFlight expires at A, then trigger duplicate or verify late ACK at OutboxManager!
    // Here we deliver to B with ACK dropped, wait > 1.1s, expire on A, then verify terminal Expired!
    nodes[3].drop_all_outbound = true;
    for _ in 0..40 {
        pump_ble_and_inbound(&mut nodes, 3).await;
        if !nodes[3].inbox.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    pump_ble_and_inbound(&mut nodes, 2).await;
    nodes[3].drop_all_outbound = false;
    assert_eq!(
        nodes[0]
            .svc
            .get_delivery_state(inf_id.clone())
            .await
            .unwrap(),
        "InFlight"
    );

    tokio::time::sleep(Duration::from_millis(1150)).await;
    let _ = nodes[0].svc.pump_outbox().await.unwrap();
    assert_eq!(
        nodes[0]
            .svc
            .get_delivery_state(inf_id.clone())
            .await
            .unwrap(),
        "Expired"
    );

    // Direct SQLite + Coordinator immutability verification: late ACK cannot overwrite Expired
    let mut id_bytes = [0u8; 16];
    for i in 0..16 {
        id_bytes[i] = u8::from_str_radix(&inf_id[i * 2..i * 2 + 2], 16).unwrap();
    }
    let kp = Arc::new(InjectedKeyProvider::random(1));
    let mut sqlite = SqliteOutboxStore::in_memory(kp, OutboxLimits::default()).unwrap();
    let rec = OutboxRecord {
        logical_id: id_bytes,
        sender: nodes[0].svc.my_peer_id(),
        recipient: b_peer_id,
        body: b"x".to_vec(),
        mode: MessageMode::Normal,
        created_at: 100,
        expires_at: 101,
        state: DeliveryState::InFlight,
        attempt_count: 1,
        last_attempt_at: Some(100),
    };
    sqlite.enqueue(rec).unwrap();
    sqlite.mark_expired(&id_bytes).unwrap();
    assert_eq!(
        sqlite.get(&id_bytes).unwrap().unwrap().state,
        DeliveryState::Expired
    );
    sqlite.mark_delivered(&id_bytes).unwrap();
    assert_eq!(
        sqlite.get(&id_bytes).unwrap().unwrap().state,
        DeliveryState::Expired,
        "Late ACK must NEVER overwrite terminal Expired in SQLite"
    );

    for n in nodes {
        n.cleanup().await;
    }
}

#[tokio::test]
async fn test_t2_clock_rollback_and_forward_across_restart_never_extends_ttl() {
    let dir = fresh_temp_dir("t2_10_clock");
    let dek = [0x55u8; 32];
    let local_id = Identity::generate().peer_id();
    let remote_id = Identity::generate().peer_id();

    let mut coord = DeliveryCoordinator::new(local_id);
    let now_inst = Instant::now();
    let created_at = 10_000u64;
    let ttl_secs = 120u64;
    let logical_id = [0x33u8; 16];

    let rec = OutboxRecord {
        logical_id,
        sender: local_id,
        recipient: remote_id,
        body: b"clock-test".to_vec(),
        mode: MessageMode::Normal,
        created_at,
        expires_at: created_at + ttl_secs,
        state: DeliveryState::Queued,
        attempt_count: 1,
        last_attempt_at: Some(created_at + 10),
    };

    // 1. Clock rollback to 1_000 (< created_at=10_000): remaining TTL must clamp to original 120s!
    let act_rollback = coord.rehydrate_record(rec.clone(), now_inst, 1_000, Duration::from_secs(4));
    assert!(act_rollback.is_none());
    let rehydrated = coord.get_record(&logical_id).unwrap();
    assert!(
        rehydrated.remaining_ttl(now_inst) <= Duration::from_secs(ttl_secs),
        "Clock rollback must NEVER extend original TTL ({:?})",
        rehydrated.remaining_ttl(now_inst)
    );

    // 2. Clock jump forward past expires_at -> immediate expiry on rehydration
    let act_fwd =
        coord.rehydrate_record(rec, now_inst, created_at + ttl_secs, Duration::from_secs(4));
    assert_eq!(act_fwd, Some(DeliveryAction::Expired(logical_id)));

    // 3. Full SQLite restart expiration check: pre-write an already-expired record in SQLite,
    //    then boot BitchatCoreService -> rehydration immediately marks SQLite record Expired.
    let db_path = dir.join("bitchat_outbox.db");
    let kp = Arc::new(InjectedKeyProvider::new(dek, 1));
    {
        let mut sqlite = SqliteOutboxStore::open(&db_path, kp, OutboxLimits::default()).unwrap();
        sqlite
            .enqueue(OutboxRecord {
                logical_id,
                sender: local_id,
                recipient: remote_id,
                body: b"past-due".to_vec(),
                mode: MessageMode::Normal,
                created_at: 100,
                expires_at: 200,
                state: DeliveryState::Queued,
                attempt_count: 0,
                last_attempt_at: None,
            })
            .unwrap();
    }

    let svc = BitchatCoreService::new_with_key(dir.to_str().unwrap().to_string(), Some(dek), 1)
        .await
        .unwrap();
    let id_hex: String = logical_id.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(svc.get_delivery_state(id_hex).await.unwrap(), "Expired");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_t2_invalid_deadline_presets_rejected_not_clamped() {
    let mut sender = NodeHarness::new("t2_11").await;
    let b_pid = Identity::generate().peer_id();
    sender.svc.register_verified_peer_id(b_pid);

    // 0s rejected for both Normal & Privacy
    assert!(sender
        .svc
        .send_message_with_deadline(b_pid.to_string(), "zero-norm".into(), 0, Some(0))
        .await
        .is_err());
    assert!(sender
        .svc
        .send_message_with_deadline(b_pid.to_string(), "zero-priv".into(), 1, Some(0))
        .await
        .is_err());

    // Privacy > 1h (3601s) strictly rejected (not silently clamped!)
    assert!(sender
        .svc
        .send_message_with_deadline(b_pid.to_string(), "over-priv".into(), 1, Some(3601))
        .await
        .is_err());

    // Normal > 7d strictly rejected (not silently clamped!)
    assert!(sender
        .svc
        .send_message_with_deadline(
            b_pid.to_string(),
            "over-norm".into(),
            0,
            Some(7 * 86400 + 1)
        )
        .await
        .is_err());

    // Valid presets (2m, 15m, 1h) accepted across both modes
    for preset in [
        DEADLINE_PRESET_2M_SECS,
        DEADLINE_PRESET_15M_SECS,
        DEADLINE_PRESET_1H_SECS,
    ] {
        let id_norm = sender
            .svc
            .send_message_with_deadline(b_pid.to_string(), format!("p-{preset}"), 0, Some(preset))
            .await
            .expect("Normal preset accepted");
        assert_eq!(id_norm.len(), 32);

        let id_priv = sender
            .svc
            .send_message_with_deadline(b_pid.to_string(), format!("pp-{preset}"), 1, Some(preset))
            .await
            .expect("Privacy preset accepted");
        assert_eq!(id_priv.len(), 32);
    }

    sender.cleanup().await;
}

#[test]
fn test_t2_edf_bounded_fairness_prevents_ordinary_starvation() {
    let local_id = Identity::generate().peer_id();
    let mut coord = DeliveryCoordinator::new(local_id);
    let now = Instant::now();

    // 5 urgent messages (<= 120s) to 5 distinct peers, sorted deadliest first: [40s, 50s, 60s, 70s, 80s]
    let mut urgent_ids = Vec::new();
    for (idx, ttl_secs) in [80u64, 40, 60, 50, 70].iter().enumerate() {
        let p = Identity::generate().peer_id();
        let lid = [0xA0 + idx as u8; 16];
        coord.enqueue(
            EnqueueMessage {
                logical_id: lid,
                recipient_peer_id: p,
                recipient_short_id: p.short_id(),
                body: vec![idx as u8],
                ttl: Duration::from_secs(*ttl_secs),
                max_attempts: 5,
                is_privacy_mode: false,
            },
            now + Duration::from_millis(idx as u64),
        );
        urgent_ids.push((*ttl_secs, lid));
    }
    urgent_ids.sort_by_key(|(ttl, _)| *ttl);

    // 2 ordinary messages (24h TTL) to 2 distinct peers
    let mut ordinary_ids = Vec::new();
    for idx in 0..2 {
        let p = Identity::generate().peer_id();
        let lid = [0xB0 + idx as u8; 16];
        coord.enqueue(
            EnqueueMessage {
                logical_id: lid,
                recipient_peer_id: p,
                recipient_short_id: p.short_id(),
                body: vec![idx as u8],
                ttl: Duration::from_secs(24 * 3600),
                max_attempts: 10,
                is_privacy_mode: false,
            },
            now + Duration::from_millis(10 + idx as u64),
        );
        ordinary_ids.push(lid);
    }

    let actions = coord.check_timeouts(now + Duration::from_secs(1));
    let dispatched: Vec<[u8; 16]> = actions
        .into_iter()
        .map(|a| match a {
            DeliveryAction::ReadyToRetry(id) => id,
            other => panic!("Unexpected action: {other:?}"),
        })
        .collect();

    assert_eq!(dispatched.len(), 7);
    assert_eq!(MAX_CONSECUTIVE_URGENT_DISPATCHES, 3);
    // First 3 must be earliest-deadline urgent [40s, 50s, 60s]
    assert_eq!(dispatched[0], urgent_ids[0].1);
    assert_eq!(dispatched[1], urgent_ids[1].1);
    assert_eq!(dispatched[2], urgent_ids[2].1);
    // 4th slot MUST promote ordinary_ids[0] after 3 consecutive urgent dispatches (starvation bound)
    assert_eq!(dispatched[3], ordinary_ids[0]);
    // Remaining urgent [70s, 80s] follow, then ordinary_ids[1]
    assert_eq!(dispatched[4], urgent_ids[3].1);
    assert_eq!(dispatched[5], urgent_ids[4].1);
    assert_eq!(dispatched[6], ordinary_ids[1]);
}
