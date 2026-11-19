use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bitchat2_core::api::BitchatCoreService;
use bitchat2_core::gateway::routes::GatewayRouteTable;
use bitchat2_core::gateway::wire::{
    GatewayMessage, RouteRecipient, RouteSnapshot, WireError, MAX_LEASE_SECONDS,
    MAX_SNAPSHOT_RECIPIENTS, MAX_WIRE_FRAME_SIZE, WIRE_VERSION,
};
use bitchat2_core::identity::Identity;
use bitchat2_core::mesh::cover::{CoverFrame, CoverFrameKind, COVER_MAGIC};
use bitchat2_core::mesh::fragment::{Reassembler, DEFAULT_MAX_PARTIAL_FRAMES};
use bitchat2_core::storage::{InjectedKeyProvider, KeyProvider};
use bitchat2_core::sync::{
    conversation_id, SqliteSyncStore, SyncRecord, SyncRecordKind, SyncSessionManager, SYNC_MAGIC,
};
use bitchat2_core::trust::{
    AuthOutcome, AuthReason, AuthorizationContext, Capability, SignedVoucher,
    SignedVoucherRevocation, TrustPolicy, TrustQuotaConfig, TrustStore,
};

fn fresh_temp_dir(label: &str) -> std::path::PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "bitchat_s14_{label}_{ts}_{}",
        rand::random::<u32>()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn new_node(label: &str, seed_byte: u8) -> (BitchatCoreService, std::path::PathBuf) {
    let dir = fresh_temp_dir(label);
    let dek = [seed_byte ^ 0xA5; 32];
    let mut svc = BitchatCoreService::new_with_key(dir.to_str().unwrap().to_string(), Some(dek), 1)
        .await
        .unwrap();
    svc.set_ack_timeout(Duration::from_millis(80));
    (svc, dir)
}

async fn pump_pair(a: &mut BitchatCoreService, b: &mut BitchatCoreService) {
    let a_sh = a.my_short_id().to_string();
    let b_sh = b.my_short_id().to_string();
    for _ in 0..35 {
        let mut moved = false;
        while let Some(f) = a.poll_outbound() {
            let _ = b.on_frame_received(a_sh.clone(), f.bytes);
            moved = true;
        }
        let _ = b.pump_outbox().await;
        while let Ok(Some(_)) = b.poll_inbound().await {
            moved = true;
        }
        while let Some(f) = b.poll_outbound() {
            let _ = a.on_frame_received(b_sh.clone(), f.bytes);
            moved = true;
        }
        let _ = a.pump_outbox().await;
        while let Ok(Some(_)) = a.poll_inbound().await {
            moved = true;
        }
        if !moved {
            break;
        }
    }
}

#[tokio::test]
async fn test_s14_connection_and_identity_churn_bounded() {
    let (mut victim, dir_v) = new_node("churn_v", 0x11).await;
    let (mut legit, dir_l) = new_node("churn_l", 0x12).await;

    let v_sh = victim.my_short_id().to_string();
    let l_sh = legit.my_short_id().to_string();
    let v_pid = victim.my_peer_id().to_string();
    let l_pid = legit.my_peer_id().to_string();

    victim
        .trust_set_peer_state(l_pid.clone(), "approve".into())
        .await
        .unwrap();
    legit
        .trust_set_peer_state(v_pid.clone(), "approve".into())
        .await
        .unwrap();

    // Churn 150 transient unverified BLE connections & RSSI samples
    for i in 1..=150u64 {
        let churn_sid = format!("{i:016x}");
        victim.on_peer_connected(churn_sid.clone()).await.unwrap();
        let _ = victim.record_ble_rssi(churn_sid.clone(), -50).await;
        // Unverified link must NEVER report proximity category
        assert_eq!(
            victim.get_peer_proximity(churn_sid.clone()).await.unwrap(),
            "Unknown"
        );
        victim.on_peer_disconnected(churn_sid).await.unwrap();
    }

    // Legitimate peer connects and exchanges encrypted message cleanly after churn
    victim.on_peer_connected(l_sh.clone()).await.unwrap();
    legit.on_peer_connected(v_sh.clone()).await.unwrap();
    pump_pair(&mut victim, &mut legit).await;

    assert!(victim.is_session_ready(&l_sh).unwrap());
    legit
        .send_message(v_pid, "survives-churn".into(), 0)
        .await
        .unwrap();
    let mut received_texts = Vec::new();
    for _ in 0..35 {
        let mut moved = false;
        while let Some(f) = legit.poll_outbound() {
            let _ = victim.on_frame_received(l_sh.clone(), f.bytes);
            moved = true;
        }
        let _ = victim.pump_outbox().await;
        while let Ok(Some(msg)) = victim.poll_inbound().await {
            received_texts.push(msg.text);
            moved = true;
        }
        while let Some(f) = victim.poll_outbound() {
            let _ = legit.on_frame_received(v_sh.clone(), f.bytes);
            moved = true;
        }
        let _ = legit.pump_outbox().await;
        while let Ok(Some(_)) = legit.poll_inbound().await {
            moved = true;
        }
        if !moved {
            break;
        }
    }

    assert_eq!(received_texts, vec!["survives-churn".to_string()]);

    drop(victim);
    drop(legit);
    let _ = std::fs::remove_dir_all(&dir_v);
    let _ = std::fs::remove_dir_all(&dir_l);
}

#[tokio::test]
async fn test_s14_slow_frames_huge_lengths_and_fragment_flood_starvation() {
    // 1. Gateway wire parser rejects zero-length, >64KiB length prefix, and times out slow streams
    assert_eq!(GatewayMessage::decode(&[]), Err(WireError::ZeroLength));
    let huge_buf = vec![0u8; MAX_WIRE_FRAME_SIZE + 1];
    assert_eq!(
        GatewayMessage::decode(&huge_buf),
        Err(WireError::FrameTooLarge(MAX_WIRE_FRAME_SIZE + 1))
    );

    // Slow-loris stream sending 4-byte length prefix (100B) but only 3 bytes of body times out
    let (mut client, mut server) = tokio::io::duplex(1024);
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let _ = client.write_all(&100u32.to_be_bytes()).await;
        let _ = client.write_all(&[1, 2, 3]).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
    });
    let slow_res = GatewayMessage::read_framed(&mut server, Duration::from_millis(40)).await;
    assert_eq!(slow_res, Err(WireError::Timeout));

    // 2. BLE Reassembler survives 250 adversarial partial/malformed fragment floods
    let mut reassembler = Reassembler::default();
    let attacker = Some([0x99; 8]);
    let now = Instant::now();
    for i in 0..250u16 {
        let mut fake_frag = Vec::with_capacity(20);
        fake_frag.extend_from_slice(&i.to_be_bytes());
        fake_frag.push(0); // idx 0
        fake_frag.push(10); // total 10 (never completed)
        fake_frag.extend_from_slice(b"partial-flood");
        let _ = reassembler.process_from(attacker, &fake_frag, now);
    }
    assert!(reassembler.active_frames_count() <= DEFAULT_MAX_PARTIAL_FRAMES);

    // Malformed fragment headers (total = 0, idx >= total) fail closed
    let bad_zero = [0, 1, 0, 0, b'a'];
    assert!(reassembler.process_from(attacker, &bad_zero, now).is_err());

    let bad_idx = [0, 2, 5, 5, b'a'];
    assert!(reassembler.process_from(attacker, &bad_idx, now).is_err());

    // 3. End-to-end 20-byte MTU delivery succeeds immediately after fragment flood on live node
    let (mut a, dir_a) = new_node("frag_a", 0x21).await;
    let (mut b, dir_b) = new_node("frag_b", 0x22).await;
    let a_sh = a.my_short_id().to_string();
    let b_sh = b.my_short_id().to_string();
    a.on_peer_connected(b_sh.clone()).await.unwrap();
    b.on_peer_connected(a_sh.clone()).await.unwrap();
    a.trust_set_peer_state(b.my_peer_id().to_string(), "approve".into())
        .await
        .unwrap();
    b.trust_set_peer_state(a.my_peer_id().to_string(), "approve".into())
        .await
        .unwrap();
    pump_pair(&mut a, &mut b).await;

    let _ = a.set_ble_mtu(20);
    let _ = b.set_ble_mtu(20);

    // Inject 160 orphan partial fragments into b before legitimate fragmented message
    for i in 0..160u16 {
        let mut f = Vec::from(b"BCFR" as &[u8]);
        f.extend_from_slice(&i.to_be_bytes());
        f.push(0);
        f.push(8);
        f.extend_from_slice(&[0xAA; 12]);
        let _ = b.on_frame_received(a_sh.clone(), f);
    }

    a.send_message(
        b.my_peer_id().to_string(),
        "legitimate-message-after-fragment-flood-survives".into(),
        0,
    )
    .await
    .unwrap();
    pump_pair(&mut a, &mut b).await;
    assert!(b.get_diagnostics_snapshot().await.deliveries_count >= 1);

    drop(a);
    drop(b);
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

#[test]
fn test_s14_pending_request_quota_starvation_and_per_sender_isolation() {
    let local_id = Identity::from_bytes(&[0x40; 32]).peer_id();
    let mut ts = TrustStore::new(local_id, TrustQuotaConfig::default());
    let now_inst = Instant::now();
    let now_unix = 1_700_000_000u64;
    let ctx = AuthorizationContext {
        now_monotonic: now_inst,
        now_unix: Some(now_unix),
        gateway_opted_in: false,
        allow_vouched_gateway_forwarding: false,
    };

    // Fill all 32 pending request slots with 32 distinct unapproved sybils
    for s in 1..=32u8 {
        let p = Identity::from_bytes(&[s; 32]).peer_id();
        let dec = TrustPolicy::authorize(&mut ts, &p, Capability::ReceiveDirectMessage, &ctx);
        assert_eq!(dec.outcome, AuthOutcome::Pending);
        ts.record_identity_verified(p).unwrap();
    }

    // 33rd unapproved sender is rejected with MessageRequestQuotaExceeded
    let sybil_33 = Identity::from_bytes(&[33; 32]).peer_id();
    let dec_33 = TrustPolicy::authorize(&mut ts, &sybil_33, Capability::ReceiveDirectMessage, &ctx);
    assert_eq!(dec_33.outcome, AuthOutcome::Deny);
    assert_eq!(dec_33.reason, AuthReason::MessageRequestQuotaExceeded);
    assert!(ts.record_identity_verified(sybil_33).is_err());

    // Approved peer is NEVER starved even when global pending pool is full at 32/32
    let approved_peer = Identity::from_bytes(&[0x99; 32]).peer_id();
    let caps = HashSet::from([Capability::ReceiveDirectMessage]);
    ts.approve_peer(approved_peer, caps, now_unix).unwrap();
    let dec_approved = TrustPolicy::authorize(
        &mut ts,
        &approved_peer,
        Capability::ReceiveDirectMessage,
        &ctx,
    );
    assert_eq!(dec_approved.outcome, AuthOutcome::Allow);
}

#[test]
fn test_s14_malicious_signed_routes_epoch_replay_and_wrong_signer_rejected() {
    let gw_id = Identity::from_bytes(&[0x51; 32]);
    let wrong_id = Identity::from_bytes(&[0x52; 32]);
    let target_peer = Identity::from_bytes(&[0x53; 32]);

    let mut snap = RouteSnapshot {
        protocol_version: WIRE_VERSION,
        session_epoch: 10,
        sequence_number: 5,
        lease_seconds: 300,
        recipients: vec![RouteRecipient {
            short_id_hex: target_peer.short_id().to_string(),
            public_key_hex: Some(target_peer.peer_id().to_string()),
        }],
        signature: Vec::new(),
    };
    snap.sign(&gw_id);
    assert!(snap.verify_author(&gw_id.peer_id()).is_ok());
    assert!(snap.validate_bounds().is_ok());

    // 1. Wrong signer / impersonated gateway rejected
    assert!(snap.verify_author(&wrong_id.peer_id()).is_err());

    // 2. Tampered sequence/epoch/recipient invalidates signature
    let mut tampered = snap.clone();
    tampered.sequence_number = 6;
    assert!(tampered.verify_author(&gw_id.peer_id()).is_err());

    // 3. Oversized recipient list or excessive lease rejected by validate_bounds
    let mut huge_snap = snap;
    huge_snap.lease_seconds = MAX_LEASE_SECONDS + 1;
    assert!(huge_snap.validate_bounds().is_err());
    huge_snap.lease_seconds = 300;
    huge_snap.recipients = vec![
        RouteRecipient {
            short_id_hex: target_peer.short_id().to_string(),
            public_key_hex: None,
        };
        MAX_SNAPSHOT_RECIPIENTS + 1
    ];
    assert!(huge_snap.validate_bounds().is_err());

    // 4. Stale epoch, duplicate sequence, u64::MAX sequence, and mismatched PeerId->ShortId binding rejected
    let mut table = GatewayRouteTable::new();
    let secret = iroh::SecretKey::from_bytes(&[0x51; 32]);
    let gw_ep = secret.public();
    let now = Instant::now();

    assert!(table
        .apply_snapshot_with_peers(
            gw_ep,
            10,
            5,
            vec![(target_peer.short_id(), Some(target_peer.peer_id()))],
            Duration::from_secs(300),
            now,
        )
        .is_ok());

    // Duplicate sequence (10, 5) rejected
    assert!(table
        .apply_snapshot_with_peers(gw_ep, 10, 5, vec![], Duration::from_secs(300), now)
        .is_err());
    // Older sequence (10, 4) rejected
    assert!(table
        .apply_snapshot_with_peers(gw_ep, 10, 4, vec![], Duration::from_secs(300), now)
        .is_err());
    // Stale epoch (9, 99) rejected
    assert!(table
        .apply_snapshot_with_peers(gw_ep, 9, 99, vec![], Duration::from_secs(300), now)
        .is_err());
    // Sequence overflow u64::MAX rejected
    assert!(table
        .apply_snapshot_with_peers(gw_ep, 10, u64::MAX, vec![], Duration::from_secs(300), now)
        .is_err());
    // Spoofed PeerId not matching announced ShortId is stripped from PeerId route bindings
    assert!(table
        .apply_snapshot_with_peers(
            gw_ep,
            10,
            6,
            vec![(target_peer.short_id(), Some(wrong_id.peer_id()))],
            Duration::from_secs(300),
            now,
        )
        .is_ok());
    assert_eq!(
        table.peer_id_for_short_at(&target_peer.short_id(), now),
        None
    );
    assert!(table
        .candidates_for_peer_at(&wrong_id.peer_id(), now, |_| true)
        .is_empty());
}

#[test]
fn test_s14_credential_replay_voucher_revocation_and_non_transitivity() {
    let introducer = Identity::from_bytes(&[0x61; 32]);
    let subject = Identity::from_bytes(&[0x62; 32]);
    let audience = Identity::from_bytes(&[0x63; 32]);
    let bystander = Identity::from_bytes(&[0x64; 32]);
    let fourth_party = Identity::from_bytes(&[0x65; 32]);

    let now_inst = Instant::now();
    let now_unix = 1_700_000_000u64;
    let intro_caps = HashSet::from([
        Capability::ReceiveDirectMessage,
        Capability::ActAsIntroducer,
        Capability::IssueIntroduction,
    ]);

    let voucher = SignedVoucher::issue(
        &introducer,
        subject.peer_id(),
        audience.peer_id(),
        Capability::ReceiveDirectMessage,
        now_unix,
        now_unix + 3600,
        1,
    )
    .unwrap();

    // 1. Replay to wrong audience (bystander) fails closed
    let mut ts_bystander = TrustStore::new(bystander.peer_id(), TrustQuotaConfig::default());
    ts_bystander
        .approve_peer(introducer.peer_id(), intro_caps.clone(), now_unix)
        .unwrap();
    assert!(ts_bystander
        .import_voucher(&voucher, now_inst, Some(now_unix + 10))
        .is_err());

    // 2. Replay after expiry fails closed
    let mut ts_audience = TrustStore::new(audience.peer_id(), TrustQuotaConfig::default());
    ts_audience
        .approve_peer(introducer.peer_id(), intro_caps.clone(), now_unix)
        .unwrap();
    assert!(ts_audience
        .import_voucher(&voucher, now_inst, Some(now_unix + 4000))
        .is_err());

    // 3. Valid import succeeds, duplicate replay is rejected, and revocation tombstones voucher
    ts_audience
        .import_voucher(&voucher, now_inst, Some(now_unix + 10))
        .unwrap();
    assert!(ts_audience
        .import_voucher(&voucher, now_inst, Some(now_unix + 12))
        .is_err());

    let rev = SignedVoucherRevocation::issue(
        &introducer,
        subject.peer_id(),
        audience.peer_id(),
        voucher.credential_id,
        1,
        now_unix + 20,
    )
    .unwrap();
    ts_audience.import_revocation(&rev, now_inst).unwrap();
    assert!(ts_audience
        .import_voucher(&voucher, now_inst, Some(now_unix + 30))
        .is_err());

    // 4. Non-transitivity: Vouched subject cannot act as introducer for fourth_party on audience
    let mut ts_non_trans = TrustStore::new(audience.peer_id(), TrustQuotaConfig::default());
    ts_non_trans
        .approve_peer(introducer.peer_id(), intro_caps, now_unix)
        .unwrap();
    let v1 = SignedVoucher::issue(
        &introducer,
        subject.peer_id(),
        audience.peer_id(),
        Capability::ReceiveDirectMessage,
        now_unix,
        now_unix + 3600,
        1,
    )
    .unwrap();
    ts_non_trans
        .import_voucher(&v1, now_inst, Some(now_unix + 5))
        .unwrap();

    let transitive_v2 = SignedVoucher::issue(
        &subject,
        fourth_party.peer_id(),
        audience.peer_id(),
        Capability::ReceiveDirectMessage,
        now_unix,
        now_unix + 3600,
        1,
    )
    .unwrap();
    assert!(
        ts_non_trans
            .import_voucher(&transitive_v2, now_inst, Some(now_unix + 10))
            .is_err(),
        "1-hop Vouched peer must NEVER be allowed to introduce a 2nd-hop peer"
    );
}

#[test]
fn test_s14_corrupt_cover_and_sync_frames_rejected_cleanly() {
    // 1. Corrupt CoverFrame payloads
    assert!(CoverFrame::decode(b"BC2C").is_err()); // truncated
    let mut bad_ver = Vec::from(COVER_MAGIC);
    bad_ver.extend_from_slice(&[99, CoverFrameKind::Offer as u8, 0, 0]);
    assert!(CoverFrame::decode(&bad_ver).is_err());

    // Control frame with non-zero payload rejected
    let mut bad_ctrl = Vec::from(COVER_MAGIC);
    bad_ctrl.extend_from_slice(&[1, CoverFrameKind::Offer as u8, 0, 4, 1, 2, 3, 4]);
    assert!(CoverFrame::decode(&bad_ctrl).is_err());

    // Dummy frame with non-bucket length (55B not in [32, 96, 224]) rejected
    let mut bad_dummy = Vec::from(COVER_MAGIC);
    bad_dummy.extend_from_slice(&[1, CoverFrameKind::Dummy as u8, 0, 55]);
    bad_dummy.extend_from_slice(&[0u8; 55]);
    assert!(CoverFrame::decode(&bad_dummy).is_err());

    // 2. Corrupt BC2S sync chunks & forged SyncRecord inside SyncSessionManager
    let dir = fresh_temp_dir("s14_sync");
    let id_a = Identity::from_bytes(&[0x71; 32]);
    let id_b = Identity::from_bytes(&[0x72; 32]);
    let kp: Arc<dyn KeyProvider> = Arc::new(InjectedKeyProvider::new([0x88; 32], 1));
    let st_a = SqliteSyncStore::open(&dir.join("a.db"), id_a.peer_id(), kp).unwrap();

    let mut mgr_a = SyncSessionManager::new(true);
    let now_inst = Instant::now();
    let now_unix = 100_000u64;
    let _ = mgr_a
        .initiate_session(
            id_a.peer_id(),
            id_b.peer_id(),
            &st_a,
            true,
            now_inst,
            now_unix,
        )
        .unwrap();

    // Send malformed chunk (total = 0) -> session aborts cleanly without panic
    let mut bad_chunk = Vec::from(SYNC_MAGIC);
    bad_chunk.extend_from_slice(&[1, 0, 1, 0, 0, 0xAA]);
    let out = mgr_a.handle_inbound_chunk(
        id_a.peer_id(),
        id_b.peer_id(),
        &bad_chunk,
        Some(&st_a),
        now_inst,
        now_unix,
    );
    assert!(!out.outbound_chunks.is_empty());
    assert_eq!(mgr_a.active_session_count(), 0);

    // Forged SyncRecord signature rejected by verify_for_conversation
    let mut forged = SyncRecord::create_signed(
        &id_b,
        id_a.peer_id(),
        [0x11; 16],
        now_unix - 10,
        now_unix + 3600,
        SyncRecordKind::Message,
        b"authentic".to_vec(),
    )
    .unwrap();
    forged.body = b"tampered-after-signing".to_vec();
    assert!(forged
        .verify_for_conversation(&id_a.peer_id(), &id_b.peer_id(), 0, now_unix)
        .is_err());
    let _ = conversation_id(&id_a.peer_id(), &id_b.peer_id());

    drop(st_a);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_s14_secret_redaction_and_zero_plaintext_in_storage_files() {
    let dir = fresh_temp_dir("s14_secrets");
    let raw_secret = [0x6B; 32];
    let id = Identity::from_bytes(&raw_secret);
    let dbg_str = format!("{id:?}");
    assert!(
        dbg_str.contains("REDACTED"),
        "Identity Debug must redact secret key: {dbg_str}"
    );
    assert!(
        !dbg_str.contains("6b6b6b"),
        "Identity Debug must never leak hex secret bytes"
    );

    // Pre-write legacy identity.key so ProtectedIdentityStore migrates it into BCID v1 identity.enc and shreds identity.key
    std::fs::write(dir.join("identity.key"), raw_secret).unwrap();

    let kp: Arc<dyn KeyProvider> = Arc::new(InjectedKeyProvider::new([0x9C; 32], 1));
    let mut svc_a =
        BitchatCoreService::new_with_key_provider(dir.to_str().unwrap().to_string(), kp)
            .await
            .unwrap();

    let (mut svc_b, dir_b) = new_node("s14_sec_b", 0x7E).await;
    let a_sh = svc_a.my_short_id().to_string();
    let b_sh = svc_b.my_short_id().to_string();
    let a_pid = svc_a.my_peer_id().to_string();
    let b_pid = svc_b.my_peer_id().to_string();

    svc_a.on_peer_connected(b_sh.clone()).await.unwrap();
    svc_b.on_peer_connected(a_sh.clone()).await.unwrap();
    svc_a
        .trust_set_peer_state(b_pid.clone(), "approve".into())
        .await
        .unwrap();
    svc_b
        .trust_set_peer_state(a_pid, "approve".into())
        .await
        .unwrap();
    pump_pair(&mut svc_a, &mut svc_b).await;

    svc_a.set_sync_enabled(true).unwrap();

    let normal_secret = "TOP_SECRET_NORMAL_PAYLOAD_99112233";
    svc_a
        .send_message(b_pid.clone(), normal_secret.into(), 0)
        .await
        .unwrap();

    // Negotiate volatile session and send volatile message
    svc_a.set_volatile_session(b_sh, 300).await.unwrap();
    pump_pair(&mut svc_a, &mut svc_b).await;
    let vol_secret = "TOP_SECRET_VOLATILE_PAYLOAD_88224466";
    svc_a
        .send_message(b_pid, vol_secret.into(), 0)
        .await
        .unwrap();

    drop(svc_a);
    drop(svc_b);

    // Legacy plaintext identity.key must be shredded after migration; only encrypted identity.enc exists
    assert!(!dir.join("identity.key").exists());
    assert!(dir.join("identity.enc").exists());

    // Scan every file in storage directory for raw Ed25519 secret bytes or message plaintexts
    for entry in std::fs::read_dir(&dir).unwrap() {
        let entry = entry.unwrap();
        if entry.path().is_file() {
            let bytes = std::fs::read(entry.path()).unwrap();
            assert!(
                !bytes.windows(raw_secret.len()).any(|w| w == raw_secret),
                "File {:?} leaked raw Ed25519 secret key bytes!",
                entry.file_name()
            );
            assert!(
                !bytes
                    .windows(normal_secret.len())
                    .any(|w| w == normal_secret.as_bytes()),
                "File {:?} leaked unencrypted normal message plaintext!",
                entry.file_name()
            );
            assert!(
                !bytes
                    .windows(vol_secret.len())
                    .any(|w| w == vol_secret.as_bytes()),
                "File {:?} leaked volatile message plaintext!",
                entry.file_name()
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir_b);
}
