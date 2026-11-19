use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use bitchat2_core::api::{BitchatCoreService, BridgeMessage};
use bitchat2_core::identity::{Identity, ShortId};
use bitchat2_core::mesh::proximity::ProximityCategory;
use bitchat2_core::trust::{
    AuthOutcome, AuthReason, AuthorizationContext, Capability, PeerLinkIdentityState,
    SignedVoucher, TrustPolicy,
};

fn fresh_temp_dir(label: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("bitchat_p6_auth_{label}_{}", rand::random::<u64>()));
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

async fn setup_ble_pair(nodes: &mut [NodeHarness], u: usize, v: usize) {
    connect_ble(nodes, u, v).await;
    let u_short = nodes[u].svc.my_short_id().to_string();
    let v_short = nodes[v].svc.my_short_id().to_string();
    // `on_peer_connected` already initiates handshake from the lower ShortId side;
    // avoid clobbering `attempt_id` with a duplicate `initiate_handshake`.
    for _ in 0..35 {
        pump_ble_and_inbound(nodes, 4).await;
        if nodes[u].svc.is_session_ready(&v_short).unwrap_or(false)
            && nodes[v].svc.is_session_ready(&u_short).unwrap_or(false)
        {
            return;
        }
    }
    panic!("Timed out establishing BLE Noise XX session");
}

#[tokio::test]
async fn test_t1_blocked_peer_over_ble_and_gateway_rejected_without_fake_delivered() {
    tokio::time::timeout(Duration::from_secs(20), async {
        // Part A: BLE block enforcement
        let mut nodes = vec![
            NodeHarness::new("blk_alice").await,
            NodeHarness::new("blk_bob").await,
        ];
        setup_ble_pair(&mut nodes, 0, 1).await;

        let alice_pid_hex = nodes[0].svc.my_peer_id().to_string();
        let bob_pid_hex = nodes[1].svc.my_peer_id().to_string();

        // Bob blocks Alice
        nodes[1]
            .svc
            .trust_set_peer_state(alice_pid_hex.clone(), "block".to_string())
            .await
            .unwrap();

        let lid = nodes[0]
            .svc
            .send_message(bob_pid_hex.clone(), "blocked hello".to_string(), 0)
            .await
            .unwrap();
        pump_ble_and_inbound(&mut nodes, 15).await;

        assert!(
            nodes[1].inbox.is_empty(),
            "Blocked sender message must never appear on Bob"
        );
        let auth_evs = nodes[1].svc.poll_auth_events();
        assert!(
            auth_evs
                .iter()
                .any(|e| e.outcome == AuthOutcome::Deny && e.reason == AuthReason::LocalBlocked),
            "Bob must emit structured LocalBlocked deny event, got {auth_evs:?}"
        );
        let st = nodes[0].svc.get_delivery_state(lid.clone()).await.unwrap();
        assert_ne!(
            st, "Delivered",
            "Alice must NOT see fake Delivered after Bob blocked/rejected"
        );

        // Sending FROM Bob TO blocked Alice must also fail locally
        let send_to_blocked = nodes[1]
            .svc
            .send_message(alice_pid_hex.clone(), "to blocked".to_string(), 0)
            .await;
        assert!(send_to_blocked.is_err());

        for n in nodes {
            n.cleanup().await;
        }

        // Part B: Gateway block enforcement across shared TrustStore
        let mut gw_nodes = vec![
            NodeHarness::new("gblk_sender").await,
            NodeHarness::new("gblk_gw").await,
            NodeHarness::new("gblk_recip").await,
        ];
        gw_nodes[0]
            .svc
            .gateway_start(Some("disabled".to_string()))
            .await
            .unwrap();
        gw_nodes[1]
            .svc
            .gateway_start(Some("disabled".to_string()))
            .await
            .unwrap();

        setup_ble_pair(&mut gw_nodes, 1, 2).await;
        // Partition BLE, ensure gateway route via GW for recip exists
        let contact_gw = gw_nodes[1].svc.gateway_export_contact().await.unwrap();
        let sender_pid = gw_nodes[0].svc.my_peer_id();
        let recip_pid = gw_nodes[2].svc.my_peer_id();

        gw_nodes[0]
            .svc
            .gateway_import_and_approve_contact(contact_gw.clone())
            .await
            .unwrap();
        gw_nodes[0]
            .svc
            .gateway_connect_to_contact(contact_gw)
            .await
            .unwrap();
        wait_for_active_connections(&gw_nodes[1].svc, 1).await;

        // Establish E2E session between sender(0) and recip(2) over BLE briefly, then break BLE direct link
        setup_ble_pair(&mut gw_nodes, 0, 2).await;
        gw_nodes[0].neighbors.remove(&2);
        gw_nodes[2].neighbors.remove(&0);
        let recip_short = recip_pid.short_id().to_string();
        let sender_short = sender_pid.short_id().to_string();
        gw_nodes[0]
            .svc
            .on_peer_disconnected(recip_short)
            .await
            .unwrap();
        gw_nodes[2]
            .svc
            .on_peer_disconnected(sender_short)
            .await
            .unwrap();

        // Wait for GW to learn recip's PeerId and publish RouteSync to sender
        for _ in 0..30 {
            pump_ble_and_inbound(&mut gw_nodes, 3).await;
            if let Some(bridge) = gw_nodes[0].svc.gateway_bridge_arc() {
                if !bridge
                    .authorized_candidates_for_peer(&recip_pid)
                    .await
                    .is_empty()
                {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // Recip blocks sender in TrustStore; gateway forwards opaque payload to recip which MUST drop it
        gw_nodes[2]
            .svc
            .trust_set_peer_state(sender_pid.to_string(), "block".to_string())
            .await
            .unwrap();

        let lid_gw = gw_nodes[0]
            .svc
            .send_message(recip_pid.to_string(), "via gw blocked".to_string(), 0)
            .await
            .unwrap();
        pump_ble_and_inbound(&mut gw_nodes, 25).await;

        assert!(
            gw_nodes[2].inbox.is_empty(),
            "Recip must reject gateway-forwarded message from blocked sender"
        );
        assert_ne!(
            gw_nodes[0].svc.get_delivery_state(lid_gw).await.unwrap(),
            "Delivered"
        );

        for n in gw_nodes {
            n.cleanup().await;
        }
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn test_t1_fresh_peer_handshake_pending_request_and_approval() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut nodes = vec![
            NodeHarness::new("pend_alice").await,
            NodeHarness::new("pend_bob").await,
        ];
        setup_ble_pair(&mut nodes, 0, 1).await;

        let alice_pid = nodes[0].svc.my_peer_id();
        let bob_pid = nodes[1].svc.my_peer_id();

        // First message arrives from verified-but-unapproved peer -> PendingRequest (handshake was NOT deadlocked!)
        let lid1 = nodes[0]
            .svc
            .send_message(bob_pid.to_string(), "hi stranger".to_string(), 0)
            .await
            .unwrap();
        pump_ble_and_inbound(&mut nodes, 12).await;

        assert_eq!(nodes[1].inbox.len(), 1);
        assert_eq!(nodes[1].inbox[0].auth_status, "PendingRequest");
        assert_eq!(nodes[1].svc.pending_message_requests().len(), 1);
        assert_eq!(
            nodes[0].svc.get_delivery_state(lid1).await.unwrap(),
            "Delivered"
        );

        // Approving Alice clears pending request and future messages land as Allowed
        nodes[1]
            .svc
            .trust_set_peer_state(alice_pid.to_string(), "approve".to_string())
            .await
            .unwrap();
        assert!(nodes[1].svc.pending_message_requests().is_empty());

        let _lid2 = nodes[0]
            .svc
            .send_message(bob_pid.to_string(), "hi friend".to_string(), 0)
            .await
            .unwrap();
        pump_ble_and_inbound(&mut nodes, 12).await;

        assert_eq!(nodes[1].inbox.len(), 2);
        assert_eq!(nodes[1].inbox[1].auth_status, "Allowed");

        for n in nodes {
            n.cleanup().await;
        }
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn test_t1_vouched_scoped_admission_and_unrelated_capability_rejected() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut nodes = vec![
            NodeHarness::new("vouch_introducer").await,
            NodeHarness::new("vouch_dave").await,
            NodeHarness::new("vouch_bob").await,
        ];
        setup_ble_pair(&mut nodes, 1, 2).await;

        let introducer_pid = nodes[0].svc.my_peer_id();
        let dave_pid = nodes[1].svc.my_peer_id();
        let bob_pid = nodes[2].svc.my_peer_id();

        // Bob approves introducer with ActAsIntroducer
        nodes[2]
            .svc
            .trust_set_peer_state(introducer_pid.to_string(), "introducer".to_string())
            .await
            .unwrap();

        // Introducer issues 1-hop voucher for Dave -> Bob with scope ReceiveDirectMessage only
        let uri = nodes[0]
            .svc
            .trust_issue_voucher(
                dave_pid.to_string(),
                bob_pid.to_string(),
                "direct_message".to_string(),
                600,
            )
            .await
            .unwrap();
        nodes[2].svc.trust_import_voucher(uri).await.unwrap();

        // Non-delegable capabilities (PermitOutboundGateway, ActAsIntroducer) cannot be issued by voucher
        assert!(SignedVoucher::issue(
            &Identity::generate(),
            dave_pid,
            bob_pid,
            Capability::ActAsIntroducer,
            1_700_000_000,
            1_700_000_600,
            1,
        )
        .is_err());

        // Verify unrelated capabilities are NEVER granted by voucher
        {
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let ctx = AuthorizationContext {
                now_monotonic: Instant::now(),
                now_unix: Some(now_secs),
                gateway_opted_in: true,
                allow_vouched_gateway_forwarding: false,
            };
            let ts_arc = nodes[2].svc.trust_store_arc();
            let mut ts = ts_arc.write().await;
            let dec_dm =
                TrustPolicy::authorize(&mut ts, &dave_pid, Capability::ReceiveDirectMessage, &ctx);
            assert_eq!(dec_dm.outcome, AuthOutcome::Allow);
            assert_eq!(dec_dm.reason, AuthReason::ValidOneHopVoucher);

            let dec_gw =
                TrustPolicy::authorize(&mut ts, &dave_pid, Capability::PermitOutboundGateway, &ctx);
            assert_ne!(dec_gw.outcome, AuthOutcome::Allow);

            let dec_intro =
                TrustPolicy::authorize(&mut ts, &dave_pid, Capability::ActAsIntroducer, &ctx);
            assert_ne!(dec_intro.outcome, AuthOutcome::Allow);

            // An explicitly approved direct-message-only peer gets strict Deny(CapabilityNotGranted)
            let dm_only = Identity::generate().peer_id();
            let mut dm_caps = HashSet::new();
            dm_caps.insert(Capability::ReceiveDirectMessage);
            ts.approve_peer(dm_only, dm_caps, now_secs).unwrap();
            let dec_dm_only_intro =
                TrustPolicy::authorize(&mut ts, &dm_only, Capability::ActAsIntroducer, &ctx);
            assert_eq!(dec_dm_only_intro.outcome, AuthOutcome::Deny);
            assert_eq!(dec_dm_only_intro.reason, AuthReason::CapabilityNotGranted);
        }

        // Dave sends message to Bob over public runtime -> admitted as Allowed (NOT PendingRequest)
        let _lid = nodes[1]
            .svc
            .send_message(bob_pid.to_string(), "vouched hello".to_string(), 0)
            .await
            .unwrap();
        pump_ble_and_inbound(&mut nodes, 12).await;

        assert_eq!(nodes[2].inbox.len(), 1);
        assert_eq!(nodes[2].inbox[0].auth_status, "Allowed");
        assert!(nodes[2].svc.pending_message_requests().is_empty());

        for n in nodes {
            n.cleanup().await;
        }
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn test_t1_revoke_or_block_while_queued_stops_dispatch() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut nodes = vec![
            NodeHarness::new("qblk_alice").await,
            NodeHarness::new("qblk_bob").await,
        ];
        setup_ble_pair(&mut nodes, 0, 1).await;

        let bob_pid = nodes[1].svc.my_peer_id();
        // Disconnect Bob before enqueue so message stays queued
        nodes[0].neighbors.remove(&1);
        nodes[1].neighbors.remove(&0);
        nodes[0]
            .svc
            .on_peer_disconnected(bob_pid.short_id().to_string())
            .await
            .unwrap();
        let alice_short = nodes[0].svc.my_short_id().to_string();
        nodes[1]
            .svc
            .on_peer_disconnected(alice_short)
            .await
            .unwrap();

        let lid = nodes[0]
            .svc
            .send_message(bob_pid.to_string(), "queued before block".to_string(), 0)
            .await
            .unwrap();
        assert_eq!(
            nodes[0].svc.get_delivery_state(lid.clone()).await.unwrap(),
            "Queued"
        );

        // Alice blocks Bob while message is queued
        nodes[0]
            .svc
            .trust_set_peer_state(bob_pid.to_string(), "block".to_string())
            .await
            .unwrap();

        // Reconnect Bob and advance past retry backoff -> dispatch_record re-checks trust and fails without transmitting
        connect_ble(&mut nodes, 0, 1).await;
        tokio::time::sleep(Duration::from_millis(120)).await;
        let _ = nodes[0].svc.pump_outbox().await;
        pump_ble_and_inbound(&mut nodes, 10).await;

        assert_eq!(
            nodes[0].svc.get_delivery_state(lid.clone()).await.unwrap(),
            "Failed"
        );
        let events = nodes[0].svc.poll_delivery_events_vec();
        assert!(
            events
                .iter()
                .any(|e| e.logical_id == lid && e.reason == "RecipientBlocked"),
            "Expected RecipientBlocked event, got {events:?}"
        );
        assert!(
            nodes[1].inbox.is_empty(),
            "Blocked recipient must never receive queued message on reconnect"
        );

        for n in nodes {
            n.cleanup().await;
        }
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn test_t1_unbound_link_claim_and_multihop_rssi_isolation() {
    tokio::time::timeout(Duration::from_secs(15), async {
        // Line topology: Alice(0) <-> Relay(1) <-> Bob(2)
        // Alice and Bob have NO direct BLE connection!
        let mut nodes = vec![
            NodeHarness::new("rssi_alice").await,
            NodeHarness::new("rssi_relay").await,
            NodeHarness::new("rssi_bob").await,
        ];

        // 1. Unbound link RSSI stays Unknown and PeerLinkIdentityState tracks UnboundLink/ClaimedShortId
        let fake_link = "aa:bb:cc:dd:ee:ff".to_string();
        nodes[2].svc.on_unbound_link_connected(fake_link.clone());
        assert!(matches!(
            nodes[2].svc.link_identity_state(&fake_link),
            Some(PeerLinkIdentityState::UnboundLink { .. })
        ));
        assert_eq!(
            nodes[2].svc.record_link_rssi(&fake_link, -50),
            ProximityCategory::Unknown
        );

        let fake_short = ShortId([0xAB; 8]);
        nodes[2]
            .svc
            .on_peer_connected(fake_short.to_string())
            .await
            .unwrap();
        assert!(matches!(
            nodes[2].svc.link_identity_state(&fake_short.to_string()),
            Some(PeerLinkIdentityState::ClaimedShortId { .. })
        ));

        // 2. Connect Alice <-> Relay <-> Bob and complete multi-hop Noise handshake between Alice and Bob
        connect_ble(&mut nodes, 0, 1).await;
        connect_ble(&mut nodes, 1, 2).await;

        let alice_short = nodes[0].svc.my_short_id().to_string();
        let bob_short = nodes[2].svc.my_short_id().to_string();
        let bob_pid = nodes[2].svc.my_peer_id();
        nodes[0].svc.register_verified_peer_id(bob_pid);

        let _ = nodes[0].svc.initiate_handshake(bob_short.clone()).await;
        for _ in 0..35 {
            pump_ble_and_inbound(&mut nodes, 4).await;
            if nodes[0].svc.is_session_ready(&bob_short).unwrap_or(false)
                && nodes[2].svc.is_session_ready(&alice_short).unwrap_or(false)
            {
                break;
            }
        }
        assert!(nodes[2].svc.is_session_ready(&alice_short).unwrap());

        // Even if Relay link had RSSI samples or Bob checks Alice's proximity, Alice is multi-hop -> MUST stay Unknown!
        assert_eq!(
            nodes[2]
                .svc
                .get_peer_proximity(alice_short.clone())
                .await
                .unwrap(),
            "Unknown",
            "Multi-hop peer must never be promoted to direct BLE proximity"
        );
        assert!(
            nodes[2].svc.link_identity_state(&alice_short).is_none(),
            "Multi-hop sender must not fabricate direct VerifiedPeer link entry"
        );

        // Verify spoofing a ShortId collision on raw link does not promote an unverified PeerId
        let victim_id = Identity::generate();
        let impostor_short = victim_id.short_id();
        nodes[2]
            .svc
            .on_peer_connected(impostor_short.to_string())
            .await
            .unwrap();
        // Inject raw garbage handshake frame purporting to be impostor_short
        nodes[2]
            .svc
            .on_frame_received(impostor_short.to_string(), vec![0, 0, 1, 1, 9, 9])
            .unwrap();
        pump_ble_and_inbound(&mut nodes, 4).await;
        assert!(matches!(
            nodes[2]
                .svc
                .link_identity_state(&impostor_short.to_string()),
            Some(PeerLinkIdentityState::ClaimedShortId { .. })
        ));
        assert!(nodes[2]
            .svc
            .get_peer_id(impostor_short.to_string())
            .is_err());

        for n in nodes {
            n.cleanup().await;
        }
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn test_t1_disallow_immediate_relay_distinct_from_end_user_block() {
    tokio::time::timeout(Duration::from_secs(15), async {
        // Diamond without direct C<->D: Origin C(0) connected to Relay1(1) and Relay2(2); both connected to Dest D(3)
        let mut nodes = vec![
            NodeHarness::new("rel_c").await,
            NodeHarness::new("rel_b1").await,
            NodeHarness::new("rel_b2").await,
            NodeHarness::new("rel_d").await,
        ];
        connect_ble(&mut nodes, 0, 1).await;
        connect_ble(&mut nodes, 1, 3).await;
        connect_ble(&mut nodes, 0, 2).await;
        connect_ble(&mut nodes, 2, 3).await;

        let b1_short = nodes[1].svc.my_short_id();
        // Dest D disallows immediate relay B1 (relay policy only, NOT end-user block!)
        nodes[3].svc.disallow_immediate_relay(b1_short);

        // B1 sends its OWN origin broadcast -> D accepts B1's own originated message (not blocked as end user!)
        let _ = nodes[1]
            .svc
            .send_broadcast("direct from b1".to_string())
            .await
            .unwrap();
        pump_ble_and_inbound(&mut nodes, 10).await;
        assert!(
            nodes[3].inbox.iter().any(|m| m.text == "direct from b1"),
            "Disallowing immediate relay must not block origin messages from that peer"
        );

        // Now B1 attempts to relay C's packet to D while D only listens to B1 initially (disconnect B2 briefly)
        nodes[2].neighbors.remove(&3);
        nodes[3].neighbors.remove(&2);
        let dropped_before = nodes[3]
            .svc
            .get_diagnostics_snapshot()
            .await
            .transit_packets_dropped_policy;

        let _ = nodes[0]
            .svc
            .send_broadcast("from C relayed".to_string())
            .await
            .unwrap();
        pump_ble_and_inbound(&mut nodes, 10).await;

        assert!(
            !nodes[3].inbox.iter().any(|m| m.text == "from C relayed"),
            "Dest D must drop third-party relayed frames from disallowed immediate relay B1"
        );
        let dropped_after = nodes[3]
            .svc
            .get_diagnostics_snapshot()
            .await
            .transit_packets_dropped_policy;
        assert!(dropped_after > dropped_before);

        // Now reconnect B2 -> C sends another message -> D accepts C's message cleanly via B2!
        connect_ble(&mut nodes, 2, 3).await;
        let _ = nodes[0]
            .svc
            .send_broadcast("from C via B2".to_string())
            .await
            .unwrap();
        pump_ble_and_inbound(&mut nodes, 10).await;
        assert!(
            nodes[3].inbox.iter().any(|m| m.text == "from C via B2"),
            "Dest D must still accept C's origin traffic through allowed relay B2"
        );

        for n in nodes {
            n.cleanup().await;
        }
    })
    .await
    .expect("test timed out");
}
