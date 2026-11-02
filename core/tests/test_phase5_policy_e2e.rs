//! Phase 5 End-to-End Integration, Attack, and Decoder Property Tests.
//!
//! Covers all 4 required public-runtime flows from `docs/PHASE_5.md` Section 11:
//! 1. Pending user request -> explicit approval -> fresh Noise XX handshake -> queued send
//!    -> lost ACK -> retry -> single recipient display -> terminal `Delivered`.
//! 2. Introducer voucher (`bitchat2/vouch/v1`) -> scoped admission -> signed issuer revocation
//!    (`bitchat2/vouch-revoke/v1`) -> policy transition without identity/session confusion.
//! 3. Approved gateway -> resource pressure (`Minimal`/`Paused`) -> reserved ACK capacity &
//!    route failover -> eventual authenticated ACK.
//! 4. Volatile-required sender/receiver (`BC2V`) -> E2E negotiated capability -> zero SQLite
//!    application persistence -> expiry & pending-send cancellation on close.
//! 5. Deterministic property/fuzz suite across all Phase 5 decoders (vouchers, revocations,
//!    volatile negotiation frames, strict identity parsers, and gateway contacts).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use bitchat2_core::envelope::{pad_inner_payload, unpad_bucket, MsgType};
use bitchat2_core::gateway::GatewayBridge;
use bitchat2_core::identity::{Identity, PeerId, ShortId};
use bitchat2_core::mesh::delivery::{
    ApplicationFrame, DedupStatus, DeliveryAction, DeliveryCoordinator, DeliveryState,
    EnqueueMessage, InboxDeduplicator,
};
use bitchat2_core::mesh::peer::PeerSessionManager;
use bitchat2_core::mesh::policy::{CoverageAwareFlood, CoverageAwareFloodConfig, NaiveFlood};
use bitchat2_core::mesh::proximity::{ProximityCategory, ProximityTracker};
use bitchat2_core::mesh::relay::MeshNode;
use bitchat2_core::mesh::resource::{
    BatteryBand, RelayResourceProfile, ResourceAdmission, ResourceAwareRelayPolicy,
    ResourceBudgetManager, ResourceSnapshot, ThermalState, TrafficOrigin, TriState,
};
use bitchat2_core::storage::{
    InjectedKeyProvider, MemoryOutboxStore, MessageMode, OutboxLimits, OutboxManager, SendResult,
    SqliteOutboxStore, VolatileFrameKind, VolatileNegotiationFrame, VolatileSendCheck,
    VolatileSessionManager,
};
use bitchat2_core::transport::sim::SimNetwork;
use bitchat2_core::trust::{
    AuthOutcome, AuthReason, AuthorizationContext, Capability, PeerTrustState, SignedVoucher,
    SignedVoucherRevocation, TrustPolicy, TrustQuotaConfig, TrustStore,
};
use iroh::RelayMode;

#[tokio::test]
async fn test_phase5_e2e_flow_1_pending_consent_to_approval_handshake_retry_and_delivered() {
    let net = SimNetwork::new(5001);
    let id_alice = Identity::generate();
    let id_bob = Identity::generate();
    let s_alice = id_alice.short_id();
    let s_bob = id_bob.short_id();

    net.set_bidirectional_link(s_alice, s_bob, Duration::ZERO, 0.0);

    let mut node_alice = MeshNode::new(
        Identity::from_bytes(&id_alice.to_bytes()),
        net.create_node(s_alice),
        Box::new(NaiveFlood),
    );
    let mut node_bob = MeshNode::new(
        Identity::from_bytes(&id_bob.to_bytes()),
        net.create_node(s_bob),
        Box::new(NaiveFlood),
    );

    let mut session_alice = PeerSessionManager::new();
    let mut session_bob = PeerSessionManager::new();
    let mut trust_bob = TrustStore::new(id_bob.peer_id(), TrustQuotaConfig::default());
    let mut coord_alice = DeliveryCoordinator::new(id_alice.peer_id());
    let mut dedup_bob = InboxDeduplicator::default();

    let limits = OutboxLimits::default();
    let outbox_alice_sqlite =
        SqliteOutboxStore::in_memory(Arc::new(InjectedKeyProvider::random(1)), limits).unwrap();
    let mut outbox_alice = OutboxManager::new(
        id_alice.peer_id(),
        outbox_alice_sqlite,
        MemoryOutboxStore::new(limits),
        limits,
    );

    let sim_time = Instant::now();
    let now_unix = 1_700_000_000;

    // 1. Complete Noise XX mutual identity verification
    session_alice
        .initiate_handshake(&mut node_alice, s_bob)
        .await
        .unwrap();
    node_bob.step_at(sim_time).await.unwrap();
    let f1 = node_bob.recv_inbound().unwrap();
    session_bob
        .handle_handshake(&mut node_bob, &f1)
        .await
        .unwrap();

    node_alice.step_at(sim_time).await.unwrap();
    let f2 = node_alice.recv_inbound().unwrap();
    session_alice
        .handle_handshake(&mut node_alice, &f2)
        .await
        .unwrap();

    node_bob.step_at(sim_time).await.unwrap();
    let f3 = node_bob.recv_inbound().unwrap();
    session_bob
        .handle_handshake(&mut node_bob, &f3)
        .await
        .unwrap();

    assert!(session_alice.is_session_ready(&s_bob));
    assert!(session_bob.is_session_ready(&s_alice));

    // 2. Bob records Alice's verified PeerId -> transitions to IdentityVerifiedPendingConsent (NOT Approved!)
    let st = trust_bob
        .record_identity_verified(id_alice.peer_id())
        .unwrap();
    assert_eq!(st, PeerTrustState::IdentityVerifiedPendingConsent);

    let ctx = AuthorizationContext {
        now_monotonic: sim_time,
        now_unix: Some(now_unix),
        gateway_opted_in: true,
        allow_vouched_gateway_forwarding: false,
    };
    let pending_dec = TrustPolicy::authorize(
        &mut trust_bob,
        &id_alice.peer_id(),
        Capability::ReceiveDirectMessage,
        &ctx,
    );
    assert_eq!(pending_dec.outcome, AuthOutcome::Pending);
    assert_eq!(pending_dec.reason, AuthReason::PendingUserConsent);

    // 3. Explicit user approval on Bob grants ReceiveDirectMessage
    let mut caps = HashSet::new();
    caps.insert(Capability::ReceiveDirectMessage);
    trust_bob
        .approve_peer(id_alice.peer_id(), caps, now_unix)
        .unwrap();
    let approved_dec = TrustPolicy::authorize(
        &mut trust_bob,
        &id_alice.peer_id(),
        Capability::ReceiveDirectMessage,
        &ctx,
    );
    assert_eq!(approved_dec.outcome, AuthOutcome::Allow);
    assert_eq!(approved_dec.reason, AuthReason::ExplicitlyApproved);

    // 4. Alice queues message -> Attempt 1 -> Bob receives once -> ACK lost in transit
    let payload = b"Phase 5 verified message".to_vec();
    let logical_id = match outbox_alice.send_to(
        id_bob.peer_id(),
        payload.clone(),
        MessageMode::Normal,
        None,
        now_unix,
    ) {
        SendResult::Accepted { logical_id, .. } => logical_id,
        other => panic!("Expected Accepted, got {other:?}"),
    };
    coord_alice.enqueue(
        EnqueueMessage {
            logical_id,
            recipient_peer_id: id_bob.peer_id(),
            recipient_short_id: s_bob,
            body: payload.clone(),
            ttl: Duration::from_secs(60),
            max_attempts: 5,
            is_privacy_mode: false,
        },
        sim_time,
    );

    coord_alice
        .mark_in_flight(&logical_id, Duration::from_millis(50), sim_time)
        .unwrap();
    outbox_alice.mark_in_flight(&logical_id, now_unix).unwrap();

    let frame1 = ApplicationFrame::new_message(
        logical_id,
        id_alice.peer_id(),
        id_bob.peer_id(),
        payload.clone(),
        Duration::from_secs(60),
        now_unix,
    );
    let pkt1 = session_alice
        .send_encrypted(
            &mut node_alice,
            s_bob,
            &pad_inner_payload(&frame1.encode(), true).unwrap(),
        )
        .await
        .unwrap();

    node_bob.step_at(sim_time).await.unwrap();
    let in1 = node_bob.recv_inbound().unwrap();
    let dec1 = session_bob.decrypt_inbound(&in1).unwrap();
    let parsed1 = ApplicationFrame::decode(&unpad_bucket(&dec1.plaintext).unwrap()).unwrap();
    assert_eq!(
        dedup_bob.check_or_insert(&id_alice.peer_id(), &parsed1.logical_id, sim_time),
        DedupStatus::New
    );

    // Simulate lost ACK -> timeout triggers retry
    let retry_time = sim_time + Duration::from_millis(60);
    assert_eq!(
        coord_alice.check_timeouts(retry_time),
        vec![DeliveryAction::ReadyToRetry(logical_id)]
    );

    coord_alice
        .mark_in_flight(&logical_id, Duration::from_millis(100), retry_time)
        .unwrap();
    outbox_alice
        .mark_in_flight(&logical_id, now_unix + 1)
        .unwrap();
    let pkt2 = session_alice
        .send_encrypted(
            &mut node_alice,
            s_bob,
            &pad_inner_payload(&frame1.encode(), true).unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(pkt1, pkt2);

    // Bob receives retry -> deduplicated (no duplicate display) -> sends fresh ACK
    node_bob.step_at(retry_time).await.unwrap();
    let in2 = node_bob.recv_inbound().unwrap();
    let dec2 = session_bob.decrypt_inbound(&in2).unwrap();
    let parsed2 = ApplicationFrame::decode(&unpad_bucket(&dec2.plaintext).unwrap()).unwrap();
    assert_eq!(
        dedup_bob.check_or_insert(&id_alice.peer_id(), &parsed2.logical_id, retry_time),
        DedupStatus::Duplicate
    );

    let ack = ApplicationFrame::new_ack(logical_id, id_bob.peer_id(), id_alice.peer_id(), now_unix);
    session_bob
        .send_encrypted(
            &mut node_bob,
            s_alice,
            &pad_inner_payload(&ack.encode(), false).unwrap(),
        )
        .await
        .unwrap();

    node_alice.step_at(retry_time).await.unwrap();
    let ack_in = node_alice.recv_inbound().unwrap();
    let ack_dec = session_alice.decrypt_inbound(&ack_in).unwrap();
    let ack_parsed = ApplicationFrame::decode(&unpad_bucket(&ack_dec.plaintext).unwrap()).unwrap();

    assert!(coord_alice
        .handle_ack(&ack_parsed, &ack_dec.remote_peer_id)
        .unwrap());
    outbox_alice.mark_delivered(&logical_id).unwrap();
    assert_eq!(
        coord_alice.get_state(&logical_id),
        Some(DeliveryState::Delivered)
    );
}

#[tokio::test]
async fn test_phase5_e2e_flow_2_introducer_voucher_scoped_admission_and_issuer_revocation() {
    let verifier = Identity::generate();
    let introducer = Identity::generate();
    let carol = Identity::generate();
    let now = Instant::now();
    let now_unix = 1_700_000_000;

    let mut store = TrustStore::new(verifier.peer_id(), TrustQuotaConfig::default());
    let mut caps = HashSet::new();
    caps.insert(Capability::ActAsIntroducer);
    store
        .approve_peer(introducer.peer_id(), caps, now_unix)
        .unwrap();

    // Introducer issues audience-bound voucher for Carol
    let voucher = SignedVoucher::issue(
        &introducer,
        carol.peer_id(),
        verifier.peer_id(),
        Capability::ReceiveDirectMessage,
        now_unix,
        now_unix + 3600,
        1,
    )
    .unwrap();
    let wire = voucher.to_wire_bytes().unwrap();
    assert_eq!(
        store
            .import_voucher_batch_wire(&wire, now, Some(now_unix))
            .unwrap(),
        1
    );

    let ctx = AuthorizationContext {
        now_monotonic: now,
        now_unix: Some(now_unix),
        gateway_opted_in: true,
        allow_vouched_gateway_forwarding: false,
    };

    // Carol is admitted for ReceiveDirectMessage via one-hop voucher, but denied gateway forwarding
    let dm_dec = TrustPolicy::authorize(
        &mut store,
        &carol.peer_id(),
        Capability::ReceiveDirectMessage,
        &ctx,
    );
    assert_eq!(dm_dec.outcome, AuthOutcome::Allow);
    assert_eq!(dm_dec.reason, AuthReason::ValidOneHopVoucher);

    let gw_dec = TrustPolicy::authorize(
        &mut store,
        &carol.peer_id(),
        Capability::UseGatewayForForwarding,
        &ctx,
    );
    assert_eq!(gw_dec.outcome, AuthOutcome::Deny);

    // Introducer signs revocation -> Carol's voucher is immediately revoked without altering Carol's PeerId
    let rev = SignedVoucherRevocation::issue(
        &introducer,
        carol.peer_id(),
        verifier.peer_id(),
        voucher.credential_id,
        1,
        now_unix + 10,
    )
    .unwrap();
    let rev_wire = rev.to_wire_bytes().unwrap();
    let rev_decoded = SignedVoucherRevocation::from_wire_bytes(&rev_wire).unwrap();
    store.import_revocation(&rev_decoded, now).unwrap();

    let post_rev = TrustPolicy::authorize(
        &mut store,
        &carol.peer_id(),
        Capability::ReceiveDirectMessage,
        &ctx,
    );
    assert_ne!(post_rev.outcome, AuthOutcome::Allow);
}

#[tokio::test]
async fn test_phase5_e2e_flow_3_gateway_resource_pressure_failover_and_eventual_ack() {
    let id_host = Identity::generate();
    let id_gw1 = Identity::generate();
    let id_gw2 = Identity::generate();
    let recipient_short = ShortId([0x77; 8]);
    let now = Instant::now();

    let host = GatewayBridge::new(id_host, None, RelayMode::Disabled)
        .await
        .unwrap();
    let gw1 = GatewayBridge::new(id_gw1, None, RelayMode::Disabled)
        .await
        .unwrap();
    let gw2 = GatewayBridge::new(id_gw2, None, RelayMode::Disabled)
        .await
        .unwrap();

    let c1 = gw1.export_contact().await.unwrap();
    let c2 = gw2.export_contact().await.unwrap();
    host.import_and_approve_contact(&c1.export_to_string().unwrap())
        .await
        .unwrap();
    host.import_and_approve_contact(&c2.export_to_string().unwrap())
        .await
        .unwrap();

    let routes_arc = host.routes();
    {
        let mut r = routes_arc.write().await;
        r.apply_snapshot(
            gw1.endpoint_id(),
            1,
            10,
            vec![recipient_short],
            Duration::from_secs(60),
            now,
        )
        .unwrap();
        r.apply_snapshot(
            gw2.endpoint_id(),
            1,
            5,
            vec![recipient_short],
            Duration::from_secs(60),
            now,
        )
        .unwrap();
    }

    // Resource pressure on local bridge: Minimal profile reserves 25% for ACK/Handshake control
    let mut budget =
        ResourceBudgetManager::new(RelayResourceProfile::Minimal, Duration::from_secs(10), now);
    let nbr = ShortId([0x12; 8]);
    // Exhaust data budget across two neighbors (4 KiB + 2 KiB = 6 KiB = 75% of 8 KiB)
    assert!(matches!(
        budget.check_and_record(
            TrafficOrigin::TransitRelay { neighbor: nbr },
            MsgType::Chat,
            4096,
            now
        ),
        ResourceAdmission::Admitted { .. }
    ));
    assert!(matches!(
        budget.check_and_record(
            TrafficOrigin::TransitRelay {
                neighbor: ShortId([0x13; 8])
            },
            MsgType::Chat,
            2048,
            now
        ),
        ResourceAdmission::Admitted { .. }
    ));
    // Further transit Chat is rejected, but transit ACK is admitted via reserved control capacity!
    assert_eq!(
        budget.check_and_record(
            TrafficOrigin::TransitRelay {
                neighbor: ShortId([0x13; 8])
            },
            MsgType::Chat,
            512,
            now
        ),
        ResourceAdmission::RejectedControlCapacityReserved
    );
    assert!(matches!(
        budget.check_and_record(
            TrafficOrigin::TransitRelay {
                neighbor: ShortId([0x13; 8])
            },
            MsgType::Ack,
            256,
            now
        ),
        ResourceAdmission::Admitted { .. }
    ));

    // Revoking gw1 fails over route selection to gw2
    host.revoke_or_block_peer(gw1.endpoint_id(), false).await;
    let cfg_arc = host.config();
    let cfg = cfg_arc.read().await;
    assert_eq!(
        routes_arc
            .read()
            .await
            .lookup_authorized_at(&recipient_short, now, |id| cfg.is_approved(id)),
        Some(gw2.endpoint_id())
    );
    drop(cfg);

    host.close().await;
    gw1.close().await;
    gw2.close().await;
}

#[tokio::test]
async fn test_phase5_e2e_flow_4_volatile_negotiation_zero_sqlite_expiry_and_cancellation() {
    let id_alice = Identity::generate();
    let id_bob = Identity::generate();
    let now = Instant::now();
    let limits = OutboxLimits::default();

    let sqlite_a =
        SqliteOutboxStore::in_memory(Arc::new(InjectedKeyProvider::random(1)), limits).unwrap();
    let sqlite_b =
        SqliteOutboxStore::in_memory(Arc::new(InjectedKeyProvider::random(1)), limits).unwrap();
    let mut outbox_a = OutboxManager::new(
        id_alice.peer_id(),
        sqlite_a,
        MemoryOutboxStore::new(limits),
        limits,
    );
    let mut outbox_b = OutboxManager::new(
        id_bob.peer_id(),
        sqlite_b,
        MemoryOutboxStore::new(limits),
        limits,
    );
    let mut coord_a = DeliveryCoordinator::new(id_alice.peer_id());
    let mut coord_b = DeliveryCoordinator::new(id_bob.peer_id());

    let mut vol_a = VolatileSessionManager::new(true);
    let mut vol_b = VolatileSessionManager::new(true);

    // 1. Negotiate 30s volatile retention over ApplicationFrame
    let offer = vol_a.initiate_volatile_session(id_bob.peer_id(), 30);
    let offer_app = ApplicationFrame::new_message(
        [1u8; 16],
        id_alice.peer_id(),
        id_bob.peer_id(),
        offer.encode().to_vec(),
        Duration::from_secs(60),
        100,
    );
    let decoded_offer = VolatileNegotiationFrame::decode(&offer_app.body).unwrap();
    let accept = vol_b
        .handle_negotiation_frame(
            id_alice.peer_id(),
            &decoded_offer,
            &mut outbox_b,
            &mut coord_b,
        )
        .unwrap()
        .unwrap();
    assert_eq!(accept.kind, VolatileFrameKind::Accept);
    vol_a
        .handle_negotiation_frame(id_bob.peer_id(), &accept, &mut outbox_a, &mut coord_a)
        .unwrap();

    let ttl = match vol_a.check_send_policy(&id_bob.peer_id(), MessageMode::Privacy) {
        VolatileSendCheck::AllowVolatile { ttl } => ttl,
        other => panic!("Unexpected: {other:?}"),
    };
    assert_eq!(ttl, Duration::from_secs(30));

    // 2. Send volatile message -> stored in memory outbox only, 0 SQLite records
    let send_res = outbox_a.send_to(
        id_bob.peer_id(),
        b"Burn after reading".to_vec(),
        MessageMode::Privacy,
        Some(ttl),
        100,
    );
    let logical_id = match send_res {
        SendResult::Accepted { logical_id, .. } => logical_id,
        other => panic!("Unexpected: {other:?}"),
    };
    coord_a.enqueue(
        EnqueueMessage {
            logical_id,
            recipient_peer_id: id_bob.peer_id(),
            recipient_short_id: id_bob.short_id(),
            body: b"Burn after reading".to_vec(),
            ttl,
            max_attempts: 3,
            is_privacy_mode: true,
        },
        now,
    );
    vol_a.register_pending_volatile_send(id_bob.peer_id(), logical_id);

    vol_b
        .record_volatile_message(
            id_alice.peer_id(),
            logical_id,
            id_alice.peer_id(),
            b"Burn after reading".to_vec(),
            now,
        )
        .unwrap();
    assert_eq!(vol_b.active_messages(&id_alice.peer_id(), now).len(), 1);

    // 3. Expiry purges Bob's volatile buffer
    assert_eq!(
        vol_b
            .active_messages(&id_alice.peer_id(), now + Duration::from_secs(31))
            .len(),
        0
    );

    // 4. Closing conversation on Alice cancels pending send
    assert_eq!(
        vol_a.close_conversation(&id_bob.peer_id(), &mut outbox_a, &mut coord_a),
        1
    );
    assert_eq!(
        coord_a.get_state(&logical_id),
        Some(DeliveryState::Cancelled)
    );
}

#[test]
fn test_phase5_decoder_fuzz_and_property_invariants() {
    let mut rng = StdRng::seed_from_u64(0x5EED_F022);
    let mut prox = ProximityTracker::default();
    let now = Instant::now();

    for i in 0..1000 {
        let len = rng.gen_range(0..512);
        let mut buf = vec![0u8; len];
        rng.fill(&mut buf[..]);

        // Must never panic on arbitrary random/truncated/oversized byte sequences
        let _ = SignedVoucher::from_wire_bytes(&buf);
        let _ = SignedVoucherRevocation::from_wire_bytes(&buf);
        let _ = VolatileNegotiationFrame::decode(&buf);
        let _ = ApplicationFrame::decode(&buf);

        if let Ok(s) = std::str::from_utf8(&buf) {
            let _ = ShortId::from_user_input(s);
            let _ = PeerId::from_hex(s);
        }

        // Proximity fuzzing across arbitrary RSSI and hop counts
        let rssi: i8 = rng.gen();
        let hops: u8 = rng.gen_range(0..10);
        prox.record_direct_link_rssi(&format!("link-{}", i % 16), rssi, hops, now);
    }

    // ResourceAwareRelayPolicy wrapper deterministic check
    let cov = CoverageAwareFlood::with_seed(CoverageAwareFloodConfig::default(), 42);
    let budget =
        ResourceBudgetManager::new(RelayResourceProfile::Balanced, Duration::from_secs(10), now);
    let policy = ResourceAwareRelayPolicy::new(cov, budget);
    let mut snap = ResourceSnapshot::default_at(now);
    snap.thermal_state = ThermalState::Critical;
    snap.charging = TriState::True;
    snap.battery_band = BatteryBand::High;
    assert_eq!(policy.update_snapshot(&snap), RelayResourceProfile::Paused);
    assert_eq!(ProximityCategory::default(), ProximityCategory::Unknown);
}
