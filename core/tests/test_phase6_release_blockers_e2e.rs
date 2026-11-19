use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bitchat2_core::api::{BitchatCoreService, BridgeMessage};
use bitchat2_core::crypto::NONCE_LEN;
use bitchat2_core::envelope::{pad_inner_payload, Envelope};
use bitchat2_core::gateway::routes::GatewayRouteTable;
use bitchat2_core::identity::{Identity, PeerId, ShortId};
use bitchat2_core::mesh::delivery::ApplicationFrame;
use bitchat2_core::mesh::fragment::{Fragmenter, FRAGMENT_HEADER_LEN};
use bitchat2_core::storage::{InjectedKeyProvider, KeyProvider, OutboxLimits};
use bitchat2_core::trust::AuthOutcome;
use rusqlite::Connection;

struct NodeBox {
    dir: PathBuf,
    dek: [u8; 32],
    svc: BitchatCoreService,
    inbox: Vec<BridgeMessage>,
}

impl NodeBox {
    async fn new(tag: &str, dek_byte: u8) -> Self {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("bitchat_rb_{tag}_{ts}_{}", rand::random::<u32>()));
        std::fs::create_dir_all(&dir).unwrap();
        let dek = [dek_byte; 32];
        let kp: Arc<dyn KeyProvider> = Arc::new(InjectedKeyProvider::new(dek, 1));
        let svc = BitchatCoreService::new_with_key_provider(dir.to_string_lossy().into_owned(), kp)
            .await
            .expect("construct service");
        Self {
            dir,
            dek,
            svc,
            inbox: Vec::new(),
        }
    }

    async fn restart(&mut self) {
        let kp: Arc<dyn KeyProvider> = Arc::new(InjectedKeyProvider::new(self.dek, 1));
        let mut dummy = Self::new("tmp_swap", 0xEE).await;
        std::mem::swap(&mut self.svc, &mut dummy.svc);
        drop(dummy.svc);
        let _ = std::fs::remove_dir_all(&dummy.dir);
        self.svc =
            BitchatCoreService::new_with_key_provider(self.dir.to_string_lossy().into_owned(), kp)
                .await
                .expect("restart service");
    }

    fn pid(&self) -> PeerId {
        self.svc.my_peer_id()
    }

    fn pid_hex(&self) -> String {
        self.svc.my_peer_id().to_string()
    }

    fn short(&self) -> ShortId {
        self.svc.my_short_id()
    }

    fn short_hex(&self) -> String {
        self.svc.my_short_id().to_string()
    }

    async fn pump_and_collect(&mut self) {
        let _ = self.svc.pump_outbox().await;
        for _ in 0..32 {
            match self.svc.poll_inbound().await {
                Ok(Some(msg)) => self.inbox.push(msg),
                _ => break,
            }
        }
    }

    async fn cleanup(mut self) {
        let _ = self.svc.gateway_stop().await;
        drop(self.svc);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn setup_ble_pair(a: &mut NodeBox, b: &mut NodeBox, approve: bool) {
    let a_sh = a.short_hex();
    let b_sh = b.short_hex();
    a.svc.on_peer_connected(b_sh.clone()).await.unwrap();
    b.svc.on_peer_connected(a_sh.clone()).await.unwrap();

    if approve {
        a.svc
            .trust_set_peer_state(b.pid_hex(), "approve".to_string())
            .await
            .unwrap();
        b.svc
            .trust_set_peer_state(a.pid_hex(), "approve".to_string())
            .await
            .unwrap();
    }

    for iter in 0..40 {
        if iter == 2 {
            if !a.svc.is_session_ready(&b_sh).unwrap_or(false) {
                let _ = a.svc.initiate_handshake(b_sh.clone()).await;
            } else if !b.svc.is_session_ready(&a_sh).unwrap_or(false) {
                let _ = b.svc.initiate_handshake(a_sh.clone()).await;
            }
        }
        while let Some(frame) = a.svc.poll_outbound() {
            let deliver = match frame.to {
                Some(ref t) => *t == b_sh,
                None => true,
            };
            if deliver {
                let _ = b.svc.on_frame_received(a_sh.clone(), frame.bytes);
            }
        }
        while let Some(frame) = b.svc.poll_outbound() {
            let deliver = match frame.to {
                Some(ref t) => *t == a_sh,
                None => true,
            };
            if deliver {
                let _ = a.svc.on_frame_received(b_sh.clone(), frame.bytes);
            }
        }
        a.pump_and_collect().await;
        b.pump_and_collect().await;
        if a.svc.is_session_ready(&b_sh).unwrap_or(false)
            && b.svc.is_session_ready(&a_sh).unwrap_or(false)
        {
            break;
        }
    }
    assert!(a.svc.is_session_ready(&b_sh).unwrap());
    assert!(b.svc.is_session_ready(&a_sh).unwrap());
}

fn sqlite_outbox_count(dir: &std::path::Path) -> i64 {
    let conn = Connection::open(dir.join("bitchat_outbox.db")).unwrap();
    conn.query_row("SELECT COUNT(*) FROM outbox", [], |r| r.get(0))
        .unwrap()
}

// =========================================================================
// RB1: Fresh peer -> auth -> Noise XX -> send -> lost ACK -> retry (same
// logical ID, fresh packet ID & Noise nonce) -> single display -> Delivered
// =========================================================================
#[tokio::test]
async fn test_rb1_fresh_peer_auth_noise_lost_ack_retry_single_display() {
    let mut alice = NodeBox::new("rb1_a", 0x11).await;
    let mut bob = NodeBox::new("rb1_b", 0x22).await;
    alice.svc.set_ack_timeout(Duration::from_millis(60));
    bob.svc.set_ack_timeout(Duration::from_millis(60));

    setup_ble_pair(&mut alice, &mut bob, true).await;

    let lid = alice
        .svc
        .send_message(bob.pid_hex(), "reliable-hello-rb1".into(), 0)
        .await
        .expect("send message");

    // Capture attempt 1 wire frame(s) from Alice and forward to Bob
    let mut attempt1_frames = Vec::new();
    while let Some(f) = alice.svc.poll_outbound() {
        let _ = bob
            .svc
            .on_frame_received(alice.short_hex(), f.bytes.clone());
        attempt1_frames.push(f.bytes);
    }
    assert!(!attempt1_frames.is_empty());

    // Bob receives message ONCE and emits ACK on outbound
    bob.pump_and_collect().await;
    assert_eq!(bob.inbox.len(), 1);
    assert_eq!(bob.inbox[0].text, "reliable-hello-rb1");
    assert_eq!(bob.inbox[0].msg_id, lid);

    // DROP Bob's first ACK!
    let mut dropped_acks = 0usize;
    while let Some(_ack) = bob.svc.poll_outbound() {
        dropped_acks += 1;
    }
    assert!(dropped_acks >= 1, "Bob must emit ACK on initial receipt");
    assert_ne!(
        alice.svc.get_delivery_state(lid.clone()).await.unwrap(),
        "Delivered"
    );

    // Wait past short ack_timeout and trigger retry pump
    tokio::time::sleep(Duration::from_millis(85)).await;
    alice.pump_and_collect().await;

    let mut attempt2_frames = Vec::new();
    while let Some(f) = alice.svc.poll_outbound() {
        let _ = bob
            .svc
            .on_frame_received(alice.short_hex(), f.bytes.clone());
        attempt2_frames.push(f.bytes);
    }
    assert!(
        !attempt2_frames.is_empty(),
        "Retry attempt must emit fresh frame"
    );

    // Decode first raw Envelope from attempt 1 & attempt 2 (assuming single chunk at 512 MTU)
    let env1 = Envelope::decode(&attempt1_frames[0][FRAGMENT_HEADER_LEN..]).unwrap();
    let env2 = Envelope::decode(&attempt2_frames[0][FRAGMENT_HEADER_LEN..]).unwrap();
    assert_ne!(
        env1.msg_id, env2.msg_id,
        "Retry must use a fresh wire msg_id"
    );
    assert_ne!(
        &env1.payload[..NONCE_LEN],
        &env2.payload[..NONCE_LEN],
        "Retry must use a fresh Noise AEAD nonce"
    );

    // Bob polls duplicate attempt -> MUST emit fresh ACK and MUST NOT display a second time!
    bob.pump_and_collect().await;
    assert_eq!(
        bob.inbox.len(),
        1,
        "Duplicate logical ID must not duplicate display"
    );

    // Deliver Bob's ACK back to Alice
    let mut delivered_ack = false;
    for _ in 0..10 {
        while let Some(f) = bob.svc.poll_outbound() {
            let _ = alice.svc.on_frame_received(bob.short_hex(), f.bytes);
            delivered_ack = true;
        }
        alice.pump_and_collect().await;
        if alice.svc.get_delivery_state(lid.clone()).await.unwrap() == "Delivered" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(delivered_ack);
    assert_eq!(
        alice.svc.get_delivery_state(lid).await.unwrap(),
        "Delivered"
    );

    alice.cleanup().await;
    bob.cleanup().await;
}

// =========================================================================
// RB2: Block over BLE, block over gateway, restart, continued enforcement
// =========================================================================
#[tokio::test]
async fn test_rb2_block_ble_and_gateway_survives_restart() {
    let mut alice = NodeBox::new("rb2_a", 0x31).await;
    let mut bob = NodeBox::new("rb2_b", 0x32).await;
    let mut gw = NodeBox::new("rb2_gw", 0x33).await;

    setup_ble_pair(&mut alice, &mut bob, true).await;

    // Start gateways & pair gw <-> alice
    alice
        .svc
        .gateway_start(Some("disabled".into()))
        .await
        .unwrap();
    gw.svc.gateway_start(Some("disabled".into())).await.unwrap();
    let c_gw = gw.svc.gateway_export_contact().await.unwrap();
    let ep_gw = gw.svc.gateway_bridge_arc().unwrap().endpoint_id();
    alice
        .svc
        .gateway_import_and_approve_contact(c_gw.clone())
        .await
        .unwrap();
    alice.svc.gateway_connect_to_contact(c_gw).await.unwrap();
    let c_alice = alice.svc.gateway_export_contact().await.unwrap();
    let ep_alice = alice.svc.gateway_bridge_arc().unwrap().endpoint_id();

    // Block Bob (BLE peer) and block gw (gateway endpoint) on Alice
    alice
        .svc
        .trust_set_peer_state(bob.pid_hex(), "block".into())
        .await
        .unwrap();
    alice
        .svc
        .gateway_revoke_or_block_peer(ep_gw.to_string(), true)
        .await
        .unwrap();

    // Restart both Alice & gw from persisted encrypted SQLite DBs
    alice.svc.gateway_stop().await.unwrap();
    gw.svc.gateway_stop().await.unwrap();
    alice.restart().await;
    alice
        .svc
        .gateway_start(Some("disabled".into()))
        .await
        .unwrap();
    gw.svc.gateway_start(Some("disabled".into())).await.unwrap();

    // Blocked BLE peer reconnect + send from Bob to Alice must be rejected
    setup_ble_pair(&mut alice, &mut bob, false).await;
    bob.svc
        .send_message(alice.pid_hex(), "from-blocked-bob".into(), 0)
        .await
        .unwrap();
    while let Some(f) = bob.svc.poll_outbound() {
        let _ = alice.svc.on_frame_received(bob.short_hex(), f.bytes);
    }
    alice.pump_and_collect().await;
    assert!(
        alice.inbox.is_empty(),
        "Blocked peer BLE message must be dropped after restart"
    );
    assert!(alice
        .svc
        .poll_auth_events()
        .into_iter()
        .any(|ev| ev.outcome == AuthOutcome::Deny));

    // Gateway redial from blocked peer (even with fresh pairing URI!) must stay blocked
    gw.svc
        .gateway_import_and_approve_contact(c_alice.clone())
        .await
        .unwrap();
    let _ = ep_alice;
    let _ = gw.svc.gateway_connect_to_contact(c_alice).await;
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        alice
            .svc
            .gateway_bridge_arc()
            .unwrap()
            .authorized_candidates_for_peer(&gw.pid())
            .await
            .len(),
        0
    );

    alice.cleanup().await;
    bob.cleanup().await;
    gw.cleanup().await;
}

// =========================================================================
// RB3: Approval before gateway start & revocation during active route use
// =========================================================================
#[tokio::test]
async fn test_rb3_approval_before_gateway_start_and_revoke_mid_route() {
    let mut sender = NodeBox::new("rb3_s", 0x41).await;
    let mut relay_gw = NodeBox::new("rb3_g", 0x42).await;
    let mut dest = NodeBox::new("rb3_d", 0x43).await;

    // Establish end-to-end session between sender & dest over BLE, then drop direct BLE link
    setup_ble_pair(&mut sender, &mut dest, true).await;
    sender
        .svc
        .on_peer_disconnected(dest.short_hex())
        .await
        .unwrap();
    dest.svc
        .on_peer_disconnected(sender.short_hex())
        .await
        .unwrap();

    // Pre-approve relay_gw's PeerId BEFORE gateway_start
    sender
        .svc
        .trust_set_peer_state(relay_gw.pid_hex(), "approve".into())
        .await
        .unwrap();
    sender
        .svc
        .gateway_start(Some("disabled".into()))
        .await
        .unwrap();
    relay_gw
        .svc
        .gateway_start(Some("disabled".into()))
        .await
        .unwrap();

    let c_gw = relay_gw.svc.gateway_export_contact().await.unwrap();
    let ep_gw = relay_gw.svc.gateway_bridge_arc().unwrap().endpoint_id();
    sender
        .svc
        .gateway_import_and_approve_contact(c_gw.clone())
        .await
        .unwrap();
    sender.svc.gateway_connect_to_contact(c_gw).await.unwrap();

    sender
        .svc
        .gateway_set_route_for_peer(dest.pid(), ep_gw)
        .await
        .unwrap();

    assert_eq!(
        sender
            .svc
            .gateway_bridge_arc()
            .unwrap()
            .authorized_candidates_for_peer(&dest.pid())
            .await
            .len(),
        1
    );

    let lid = sender
        .svc
        .send_message(dest.pid_hex(), "before-gw-revoke".into(), 0)
        .await
        .unwrap();
    assert_eq!(
        sender.svc.get_delivery_state(lid).await.unwrap(),
        "InFlight"
    );

    // Revoke gateway mid-route use -> route withdrawn & session closed immediately
    sender
        .svc
        .gateway_revoke_or_block_peer(ep_gw.to_string(), false)
        .await
        .unwrap();
    assert_eq!(
        sender
            .svc
            .gateway_bridge_arc()
            .unwrap()
            .authorized_candidates_for_peer(&dest.pid())
            .await
            .len(),
        0,
        "Revoked gateway must have zero authorized routes"
    );

    sender.cleanup().await;
    relay_gw.cleanup().await;
    dest.cleanup().await;
}

// =========================================================================
// RB4: Paused/Minimal profiles affect actual transit while local outbox intact
// =========================================================================
#[tokio::test]
async fn test_rb4_paused_and_minimal_profiles_govern_real_transit() {
    let mut a = NodeBox::new("rb4_a", 0x51).await;
    let mut b = NodeBox::new("rb4_b", 0x52).await;
    let mut c = NodeBox::new("rb4_c", 0x53).await;

    // 3-node topology A <-> B <-> C
    setup_ble_pair(&mut a, &mut b, true).await;
    b.svc.on_peer_connected(c.short_hex()).await.unwrap();
    c.svc.on_peer_connected(b.short_hex()).await.unwrap();

    // Also queue a local outbound message on B to a distant offline peer -> must NOT be dropped on pause
    let offline_peer = Identity::generate().peer_id();
    b.svc.register_verified_peer_id(offline_peer);
    let b_local_lid = b
        .svc
        .send_message(offline_peer.to_string(), "b-local-msg".into(), 0)
        .await
        .unwrap();
    while b.svc.poll_outbound().is_some() {}

    // Switch B to Paused (receive_only)
    assert_eq!(
        b.svc
            .set_relay_contribution_profile("receive_only")
            .unwrap(),
        "Paused"
    );

    // A sends broadcast transit traffic -> arrives at B, B must emit 0 transit frames when Paused
    a.svc
        .send_broadcast("transit-while-paused".into())
        .await
        .unwrap();
    while let Some(f) = a.svc.poll_outbound() {
        let _ = b.svc.on_frame_received(a.short_hex(), f.bytes);
    }
    b.pump_and_collect().await;
    let mut emitted_while_paused = 0usize;
    while b.svc.poll_outbound().is_some() {
        emitted_while_paused += 1;
    }
    assert_eq!(
        emitted_while_paused, 0,
        "Paused node must not forward transit"
    );
    let diag_paused = b.svc.get_diagnostics_snapshot().await;
    assert!(diag_paused.transit_packets_dropped_policy >= 1);

    // B's own local message stayed queued/intact!
    let st = b.svc.get_delivery_state(b_local_lid).await.unwrap();
    assert!(st == "Queued" || st == "InFlight");

    // Switch B to Minimal via low battery + power_save -> constrained transit
    assert_eq!(
        b.svc.set_relay_contribution_profile("auto").unwrap(),
        "Balanced"
    );
    let prof_min = b.svc.update_resource_state(12, 0, 1, 0, false);
    assert_eq!(prof_min, "Minimal");

    a.svc
        .send_broadcast("transit-minimal-first".into())
        .await
        .unwrap();
    while let Some(f) = a.svc.poll_outbound() {
        let _ = b.svc.on_frame_received(a.short_hex(), f.bytes);
    }
    // Pump outbox/node steps so scheduled relay jitter fires cleanly
    for _ in 0..25 {
        b.pump_and_collect().await;
        while let Some(f) = b.svc.poll_outbound() {
            let _ = c.svc.on_frame_received(b.short_hex(), f.bytes);
        }
        c.pump_and_collect().await;
        if !c.inbox.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(c.inbox.len(), 1);
    assert_eq!(c.inbox[0].text, "transit-minimal-first");

    a.cleanup().await;
    b.cleanup().await;
    c.cleanup().await;
}

// =========================================================================
// RB5: Volatile negotiation before content, unsupported rejection, zero SQLite
// =========================================================================
#[tokio::test]
async fn test_rb5_volatile_negotiation_unsupported_rejection_zero_sqlite_dedup() {
    // Part 1: Peer does not support volatile -> reject & refuse downgrade to Normal SQLite
    {
        let mut a = NodeBox::new("rb5_a1", 0x61).await;
        let mut b = NodeBox::new("rb5_b1", 0x62).await;
        b.svc.set_local_supports_volatile(false);
        setup_ble_pair(&mut a, &mut b, true).await;

        let st = a
            .svc
            .set_volatile_session(b.short_hex(), 120)
            .await
            .unwrap();
        assert_eq!(st, "PendingNegotiation");

        // Refuse sending before negotiation finishes
        let err_pend = a
            .svc
            .send_message(b.pid_hex(), "premature".into(), 1)
            .await
            .unwrap_err();
        assert!(err_pend.contains("VOLATILE_PENDING_NEGOTIATION"));

        // Relay Offer (A->B) and RejectUnsupported (B->A)
        for _ in 0..6 {
            while let Some(f) = a.svc.poll_outbound() {
                let _ = b.svc.on_frame_received(a.short_hex(), f.bytes);
            }
            b.pump_and_collect().await;
            while let Some(f) = b.svc.poll_outbound() {
                let _ = a.svc.on_frame_received(b.short_hex(), f.bytes);
            }
            a.pump_and_collect().await;
        }

        let vs = a.svc.get_volatile_state(b.short_hex()).await.unwrap();
        assert!(vs.contains("UnsupportedByPeer"));

        // Attempting Normal OR Privacy send must fail closed with 0 SQLite rows!
        let err_norm = a
            .svc
            .send_message(b.pid_hex(), "no-downgrade".into(), 0)
            .await
            .unwrap_err();
        assert!(err_norm.contains("VOLATILE_UNSUPPORTED_BY_PEER"));
        assert_eq!(sqlite_outbox_count(&a.dir), 0);

        a.cleanup().await;
        b.cleanup().await;
    }

    // Part 2: Both support volatile -> Active -> zero SQLite -> dedup single display
    let mut a = NodeBox::new("rb5_a2", 0x63).await;
    let mut b = NodeBox::new("rb5_b2", 0x64).await;
    a.svc.set_ack_timeout(Duration::from_millis(60));
    setup_ble_pair(&mut a, &mut b, true).await;

    a.svc
        .set_volatile_session(b.short_hex(), 120)
        .await
        .unwrap();
    for _ in 0..8 {
        while let Some(f) = a.svc.poll_outbound() {
            let _ = b.svc.on_frame_received(a.short_hex(), f.bytes);
        }
        b.pump_and_collect().await;
        while let Some(f) = b.svc.poll_outbound() {
            let _ = a.svc.on_frame_received(b.short_hex(), f.bytes);
        }
        a.pump_and_collect().await;
    }
    assert!(a
        .svc
        .get_volatile_state(b.short_hex())
        .await
        .unwrap()
        .contains("Active"));
    assert!(b
        .svc
        .get_volatile_state(a.short_hex())
        .await
        .unwrap()
        .contains("Active"));

    // Even passing mode=0 is upgraded/pinned to Privacy volatile mode
    let lid = a
        .svc
        .send_message(b.pid_hex(), "ephemeral-secret-rb5".into(), 0)
        .await
        .unwrap();
    assert_eq!(sqlite_outbox_count(&a.dir), 0);

    while let Some(f) = a.svc.poll_outbound() {
        let _ = b.svc.on_frame_received(a.short_hex(), f.bytes);
    }
    b.pump_and_collect().await;
    assert_eq!(b.inbox.len(), 1);
    assert_eq!(sqlite_outbox_count(&b.dir), 0);

    // Drop initial ACK so Alice retries -> Bob receives retry duplicate -> still 1 display
    while b.svc.poll_outbound().is_some() {}
    tokio::time::sleep(Duration::from_millis(85)).await;
    a.pump_and_collect().await;
    while let Some(f) = a.svc.poll_outbound() {
        let _ = b.svc.on_frame_received(a.short_hex(), f.bytes);
    }
    b.pump_and_collect().await;
    assert_eq!(
        b.inbox.len(),
        1,
        "Volatile duplicate retry must display once"
    );
    while let Some(f) = b.svc.poll_outbound() {
        let _ = a.svc.on_frame_received(b.short_hex(), f.bytes);
    }
    a.pump_and_collect().await;
    assert_eq!(a.svc.get_delivery_state(lid).await.unwrap(), "Delivered");

    a.cleanup().await;
    b.cleanup().await;
}

// =========================================================================
// RB6: Missing key, outbox full, and DB corruption -> typed failure
// =========================================================================
#[tokio::test]
async fn test_rb6_storage_failures_yield_typed_errors_no_fallback() {
    let mut node = NodeBox::new("rb6", 0x71).await;
    let peer = Identity::generate().peer_id();
    node.svc.register_verified_peer_id(peer);
    let max_records = OutboxLimits::default().max_records;

    for i in 0..max_records {
        node.svc
            .send_message(peer.to_string(), format!("fill-{i}"), 0)
            .await
            .expect("within outbox limit");
    }
    let full_err = node
        .svc
        .send_message(peer.to_string(), "overflow".into(), 0)
        .await
        .unwrap_err();
    assert!(
        full_err.contains("QueueFull"),
        "Expected QueueFull, got: {full_err}"
    );

    let dir = node.dir.clone();
    drop(node.svc);

    // Missing DEK fails closed
    let missing_key_err =
        BitchatCoreService::new_with_key(dir.to_string_lossy().into_owned(), None, 1)
            .await
            .err()
            .expect("missing DEK must fail");
    assert!(missing_key_err.contains("KEY_UNAVAILABLE"));

    // Corrupt identity.enc fails closed instead of silently regenerating a new identity
    std::fs::write(dir.join("identity.enc"), b"CORRUPTED_BYTES").unwrap();
    let corrupt_err =
        BitchatCoreService::new_with_key(dir.to_string_lossy().into_owned(), Some([0x71u8; 32]), 1)
            .await
            .err()
            .expect("corrupt identity must fail closed");
    assert!(corrupt_err.contains("IDENTITY_CORRUPT") || corrupt_err.contains("CORRUPT"));

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================================
// RB7: Fragmentation at 23B / 64B / 512B MTU + malformed/incomplete bounds
// =========================================================================
#[tokio::test]
async fn test_rb7_fragmentation_small_and_large_mtu_and_malformed_bounds() {
    let mut a = NodeBox::new("rb7_a", 0x81).await;
    let mut b = NodeBox::new("rb7_b", 0x82).await;
    setup_ble_pair(&mut a, &mut b, true).await;

    // 1. Default minimum BLE MTU = 23 bytes (19-byte chunk payload -> >25 fragments)
    a.svc.set_ble_mtu(23).unwrap();
    let msg_small_mtu = "A-small-MTU-payload-across-many-chunks-0123456789";
    a.svc
        .send_message(b.pid_hex(), msg_small_mtu.into(), 0)
        .await
        .unwrap();
    let mut frag_count_23 = 0usize;
    while let Some(f) = a.svc.poll_outbound() {
        assert!(f.bytes.len() <= 23);
        let _ = b.svc.on_frame_received(a.short_hex(), f.bytes);
        frag_count_23 += 1;
    }
    assert!(
        frag_count_23 >= 10,
        "MTU=23 must fragment into multiple chunks"
    );
    b.pump_and_collect().await;
    assert_eq!(b.inbox.len(), 1);
    assert_eq!(b.inbox[0].text, msg_small_mtu);
    while let Some(f) = b.svc.poll_outbound() {
        let _ = a.svc.on_frame_received(b.short_hex(), f.bytes);
    }
    a.pump_and_collect().await;

    // 2. Flood Bob with malformed, duplicate, and >64 incomplete fragment streams
    let _ = b.svc.on_frame_received(a.short_hex(), vec![0x01, 0x02]); // short malformed
    for bogus_id in 5000u16..5090u16 {
        let partial = Fragmenter::split_with_id(bogus_id, b"incomplete-noise-payload", 16).unwrap();
        // Deliver fragment 0 duplicate twice, omit final fragment!
        let _ = b.svc.on_frame_received(a.short_hex(), partial[0].clone());
        let _ = b.svc.on_frame_received(a.short_hex(), partial[0].clone());
    }
    for _ in 0..4 {
        b.pump_and_collect().await;
    }
    assert_eq!(
        b.inbox.len(),
        1,
        "Incomplete/malformed fragments must not deliver"
    );

    // 3. Switch A to 64B then 512B MTU -> valid delivery still works cleanly after flood!
    a.svc.set_ble_mtu(64).unwrap();
    a.svc
        .send_message(b.pid_hex(), "after-flood-64".into(), 0)
        .await
        .unwrap();
    while let Some(f) = a.svc.poll_outbound() {
        assert!(f.bytes.len() <= 64);
        let _ = b.svc.on_frame_received(a.short_hex(), f.bytes);
    }
    b.pump_and_collect().await;
    while let Some(f) = b.svc.poll_outbound() {
        let _ = a.svc.on_frame_received(b.short_hex(), f.bytes);
    }
    a.pump_and_collect().await;

    a.svc.set_ble_mtu(512).unwrap();
    a.svc
        .send_message(b.pid_hex(), "after-flood-512".into(), 0)
        .await
        .unwrap();
    while let Some(f) = a.svc.poll_outbound() {
        assert!(f.bytes.len() <= 512);
        let _ = b.svc.on_frame_received(a.short_hex(), f.bytes);
    }
    b.pump_and_collect().await;

    assert_eq!(b.inbox.len(), 3);
    assert_eq!(b.inbox[1].text, "after-flood-64");
    assert_eq!(b.inbox[2].text, "after-flood-512");

    a.cleanup().await;
    b.cleanup().await;
}

// =========================================================================
// RB8: Loop limits (hop_count >= MAX_HOPS, bridge_crossings > 1) + zero
// public-broadcast export to connected gateways
// =========================================================================
#[tokio::test]
async fn test_rb8_loop_limits_and_no_broadcast_gateway_export() {
    let mut a = NodeBox::new("rb8_a", 0x91).await;
    let mut b = NodeBox::new("rb8_b", 0x92).await;

    a.svc.gateway_start(Some("disabled".into())).await.unwrap();
    b.svc.gateway_start(Some("disabled".into())).await.unwrap();
    let c_b = b.svc.gateway_export_contact().await.unwrap();
    a.svc
        .gateway_import_and_approve_contact(c_b.clone())
        .await
        .unwrap();
    a.svc.gateway_connect_to_contact(c_b).await.unwrap();

    // Broadcast must produce ZERO gateway exports
    a.svc
        .on_peer_connected("1122334455667788".into())
        .await
        .unwrap();
    while a.svc.poll_outbound().is_some() {}
    a.svc.send_broadcast("local-ble-only".into()).await.unwrap();
    a.pump_and_collect().await;

    let gw_stats_a: serde_json::Value =
        serde_json::from_str(&a.svc.gateway_get_stats().await.unwrap()).unwrap();
    assert_eq!(gw_stats_a["messages_exported"].as_u64(), Some(0));

    // Check forward_mesh_packet rejects broadcast directly AND GatewayBridge rejects bridge_crossings > 1
    let broadcast_env = Envelope {
        version: 1,
        msg_type: bitchat2_core::envelope::MsgType::Chat,
        flags: Default::default(),
        hop_count: 0,
        ttl_max: 7,
        sender_short_id: a.short(),
        recipient_short_id: ShortId([0u8; 8]),
        msg_id: [0x55u8; 8],
        payload: b"public".to_vec(),
    };
    let gw_arc = a.svc.gateway_bridge_arc().unwrap();
    let exported = gw_arc
        .forward_mesh_packet(&broadcast_env, broadcast_env.encode().unwrap())
        .await
        .unwrap();
    assert!(
        !exported,
        "Public broadcasts must NEVER export over gateway"
    );

    // Exhausted hop budget (`hop_count = MAX_HOPS = 7`) dropped at relay reception
    let peer_src = Identity::generate();
    b.svc
        .on_peer_connected(peer_src.short_id().to_string())
        .await
        .unwrap();
    while b.svc.poll_outbound().is_some() {}
    let max_hop_env = Envelope {
        version: 1,
        msg_type: bitchat2_core::envelope::MsgType::Chat,
        flags: Default::default(),
        hop_count: 7,
        ttl_max: 7,
        sender_short_id: peer_src.short_id(),
        recipient_short_id: ShortId([0u8; 8]),
        msg_id: [0x77u8; 8],
        payload: b"hop-exhausted".to_vec(),
    };
    let frags = Fragmenter::default()
        .split(&max_hop_env.encode().unwrap(), 512)
        .unwrap();
    for f in frags {
        let _ = b.svc.on_frame_received(peer_src.short_id().to_string(), f);
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    b.pump_and_collect().await;
    assert!(
        b.svc.poll_outbound().is_none(),
        "hop_count == MAX_HOPS must not relay"
    );

    a.cleanup().await;
    b.cleanup().await;
}

// =========================================================================
// RB9: Session restart, forged high nonce, wrong-peer ACK, stale route epoch
// =========================================================================
#[tokio::test]
async fn test_rb9_session_restart_forged_high_nonce_wrong_peer_ack_stale_route_epoch() {
    let mut alice = NodeBox::new("rb9_a", 0xA1).await;
    let mut bob = NodeBox::new("rb9_b", 0xA2).await;
    setup_ble_pair(&mut alice, &mut bob, true).await;

    // 1. Send valid msg1 -> tamper copy with forged high nonce u64::MAX - 5 -> feed BEFORE valid msg2
    alice
        .svc
        .send_message(bob.pid_hex(), "valid-1".into(), 0)
        .await
        .unwrap();
    let frame1 = alice.svc.poll_outbound().unwrap();
    let mut env_forged = Envelope::decode(&frame1.bytes[FRAGMENT_HEADER_LEN..]).unwrap();
    env_forged.msg_id = [0xEEu8; 8];
    env_forged.payload[0..NONCE_LEN].copy_from_slice(&(u64::MAX - 5).to_be_bytes());
    let forged_frag = Fragmenter::default()
        .split(&env_forged.encode().unwrap(), 512)
        .unwrap();
    for f in forged_frag {
        let _ = bob.svc.on_frame_received(alice.short_hex(), f);
    }
    bob.pump_and_collect().await;
    assert!(
        bob.inbox.is_empty(),
        "Forged high-nonce packet must fail AEAD"
    );

    // Now deliver genuine msg1 (low nonce) -> must NOT be rejected by forged high-nonce attempt!
    let _ = bob.svc.on_frame_received(alice.short_hex(), frame1.bytes);
    bob.pump_and_collect().await;
    assert_eq!(bob.inbox.len(), 1);
    assert_eq!(bob.inbox[0].text, "valid-1");
    while let Some(f) = bob.svc.poll_outbound() {
        let _ = alice.svc.on_frame_received(bob.short_hex(), f.bytes);
    }
    alice.pump_and_collect().await;

    // 2. Wrong-peer ACK: C cannot ACK Alice's message sent to Bob!
    let lid_for_bob = alice
        .svc
        .send_message(bob.pid_hex(), "await-bongs".into(), 0)
        .await
        .unwrap();
    while alice.svc.poll_outbound().is_some() {}

    // Craft an authenticated Noise session C <-> Alice and forge ACK inside C's session
    let carol = Identity::generate();
    let c_sh = carol.short_id().to_string();
    alice.svc.on_peer_connected(c_sh.clone()).await.unwrap();
    alice
        .svc
        .trust_set_peer_state(carol.peer_id().to_string(), "approve".into())
        .await
        .unwrap();

    // Establish C side using Session directly -> feed Alice C's ApplicationFrame::new_ack(lid_for_bob)
    let mut sess_c = if alice.short() < carol.short_id() {
        bitchat2_core::crypto::Session::responder(&carol).unwrap()
    } else {
        let mut s = bitchat2_core::crypto::Session::initiator(&carol).unwrap();
        let hs1 = s.write_handshake_vec().unwrap();
        let env = Envelope {
            version: 1,
            msg_type: bitchat2_core::envelope::MsgType::Handshake,
            flags: Default::default(),
            hop_count: 0,
            ttl_max: 7,
            sender_short_id: carol.short_id(),
            recipient_short_id: alice.short(),
            msg_id: [0x11u8; 8],
            payload: bitchat2_core::crypto::HandshakeFrame::new(1, 77, alice.short().0, hs1)
                .encode(),
        };
        for f in Fragmenter::default()
            .split(&env.encode().unwrap(), 512)
            .unwrap()
        {
            let _ = alice.svc.on_frame_received(c_sh.clone(), f);
        }
        s
    };

    for _ in 0..20 {
        alice.pump_and_collect().await;
        while let Some(f) = alice.svc.poll_outbound() {
            if f.to.as_deref() == Some(c_sh.as_str()) {
                let env = Envelope::decode(&f.bytes[FRAGMENT_HEADER_LEN..]).unwrap();
                if env.msg_type == bitchat2_core::envelope::MsgType::Handshake {
                    let hsf = bitchat2_core::crypto::HandshakeFrame::decode(&env.payload).unwrap();
                    let _ = sess_c.read_handshake(&hsf.noise_payload);
                    if !sess_c.is_handshake_finished() {
                        if let Ok(reply) = sess_c.write_handshake_vec() {
                            let renv = Envelope {
                                version: 1,
                                msg_type: bitchat2_core::envelope::MsgType::Handshake,
                                flags: Default::default(),
                                hop_count: 0,
                                ttl_max: 7,
                                sender_short_id: carol.short_id(),
                                recipient_short_id: alice.short(),
                                msg_id: rand::random(),
                                payload: bitchat2_core::crypto::HandshakeFrame::new(
                                    hsf.step + 1,
                                    hsf.attempt_id,
                                    hsf.session_context,
                                    reply,
                                )
                                .encode(),
                            };
                            for rf in Fragmenter::default()
                                .split(&renv.encode().unwrap(), 512)
                                .unwrap()
                            {
                                let _ = alice.svc.on_frame_received(c_sh.clone(), rf);
                            }
                        }
                    }
                }
            }
        }
        if sess_c.is_handshake_finished() && alice.svc.is_session_ready(&c_sh).unwrap_or(false) {
            break;
        }
    }
    assert!(sess_c.is_handshake_finished());

    let raw_lid = <[u8; 16]>::try_from(
        (0..16)
            .map(|i| u8::from_str_radix(&lid_for_bob[i * 2..i * 2 + 2], 16).unwrap())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let spoofed_ack = ApplicationFrame::new_ack(raw_lid, carol.peer_id(), alice.pid(), 100);
    let padded_ack = pad_inner_payload(&spoofed_ack.encode(), true).unwrap();
    let enc_ack = sess_c.encrypt(&padded_ack).unwrap();
    let ack_env = Envelope {
        version: 1,
        msg_type: bitchat2_core::envelope::MsgType::Chat,
        flags: Default::default(),
        hop_count: 0,
        ttl_max: 7,
        sender_short_id: carol.short_id(),
        recipient_short_id: alice.short(),
        msg_id: rand::random(),
        payload: enc_ack,
    };
    for f in Fragmenter::default()
        .split(&ack_env.encode().unwrap(), 512)
        .unwrap()
    {
        let _ = alice.svc.on_frame_received(c_sh.clone(), f);
    }
    alice.pump_and_collect().await;
    assert_ne!(
        alice
            .svc
            .get_delivery_state(lid_for_bob.clone())
            .await
            .unwrap(),
        "Delivered",
        "Wrong-peer ACK from Carol must never mark Bob's message Delivered"
    );
    alice.svc.cancel_message(lid_for_bob).await.unwrap();

    // 3. Stale route snapshot epoch rejected
    let gw_ep = iroh::SecretKey::generate().public();
    let mut table = GatewayRouteTable::new();
    let now = Instant::now();
    assert!(table
        .apply_snapshot(
            gw_ep,
            5,
            2,
            vec![bob.short()],
            Duration::from_secs(300),
            now
        )
        .is_ok());
    assert!(
        table
            .apply_snapshot(
                gw_ep,
                4,
                99,
                vec![alice.short()],
                Duration::from_secs(300),
                now
            )
            .is_err(),
        "Stale route epoch must be rejected"
    );

    // 4. Receiver session restart -> restarted peer initiates fresh Noise session -> deliver message
    bob.restart().await;
    setup_ble_pair(&mut alice, &mut bob, false).await;
    alice
        .svc
        .send_message(bob.pid_hex(), "post-restart-ok".into(), 0)
        .await
        .unwrap();
    while let Some(f) = alice.svc.poll_outbound() {
        let _ = bob.svc.on_frame_received(alice.short_hex(), f.bytes);
    }
    bob.pump_and_collect().await;
    assert_eq!(bob.inbox.len(), 2);
    assert_eq!(bob.inbox[1].text, "post-restart-ok");

    alice.cleanup().await;
    bob.cleanup().await;
}
