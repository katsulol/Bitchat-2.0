//! Phase 4 End-to-End Delivery Protocol Integration Test.
//!
//! Verifies:
//! 1. Fresh handshake bootstrap
//! 2. Message admission, queueing, and routing
//! 3. Recipient authenticated delivery
//! 4. Lost ACK handling and retry cycle
//! 5. Logical ID invariance across attempts
//! 6. Fresh ephemeral packet IDs and Noise transport nonces on retry
//! 7. Recipient exactly-once deduplication
//! 8. Final outbox completion and terminal state immutability

use bitchat2_core::envelope::{pad_inner_payload, unpad_bucket, MsgType};
use bitchat2_core::identity::Identity;
use bitchat2_core::mesh::delivery::{
    ApplicationFrame, ApplicationFrameKind, DedupStatus, DeliveryAction, DeliveryCoordinator,
    DeliveryState, EnqueueMessage, InboxDeduplicator,
};
use bitchat2_core::mesh::peer::PeerSessionManager;
use bitchat2_core::mesh::policy::NaiveFlood;
use bitchat2_core::mesh::relay::MeshNode;
use bitchat2_core::mesh::router::{
    IrohPathObservation, RouteCandidate, RouteContext, RouteReason, Router,
};
use bitchat2_core::storage::{
    InjectedKeyProvider, MemoryOutboxStore, MessageMode, OutboxLimits, OutboxManager, SendResult,
    SqliteOutboxStore,
};
use bitchat2_core::transport::sim::SimNetwork;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[tokio::test]
async fn test_phase4_delivery_e2e_lifecycle() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let net = SimNetwork::new(888);
        let id_a = Identity::generate();
        let id_b = Identity::generate();

        let s_a = id_a.short_id();
        let s_b = id_b.short_id();

        // Bidirectional radio link
        net.set_bidirectional_link(s_a, s_b, Duration::ZERO, 0.0);

        let t_a = net.create_node(s_a);
        let t_b = net.create_node(s_b);

        let mut node_a = MeshNode::new(
            Identity::from_bytes(&id_a.to_bytes()),
            t_a,
            Box::new(NaiveFlood),
        );
        let mut node_b = MeshNode::new(
            Identity::from_bytes(&id_b.to_bytes()),
            t_b,
            Box::new(NaiveFlood),
        );

        let mut session_mgr_a = PeerSessionManager::new();
        let mut session_mgr_b = PeerSessionManager::new();

        let mut coord_a = DeliveryCoordinator::new(id_a.peer_id());
        let mut dedup_b = InboxDeduplicator::new(1000, Duration::from_secs(3600));

        let limits = OutboxLimits::default();
        let key_provider = Arc::new(InjectedKeyProvider::random(1));
        let sqlite = SqliteOutboxStore::in_memory(key_provider, limits).unwrap();
        let memory = MemoryOutboxStore::new(limits);
        let mut outbox_a = OutboxManager::new(id_a.peer_id(), sqlite, memory, limits);

        let sim_time = Instant::now();

        // ---------------------------------------------------------------------
        // Step 1: Outbox Send Admission & Router Handshake Check
        // ---------------------------------------------------------------------
        let raw_payload = b"Top secret Phase 4 application payload".to_vec();
        let send_res = outbox_a.send_to(
            id_b.peer_id(),
            raw_payload.clone(),
            MessageMode::Normal,
            None,
            100,
        );

        let logical_id = match send_res {
            SendResult::Accepted { logical_id, state } => {
                assert_eq!(state, DeliveryState::Queued);
                logical_id
            }
            SendResult::Rejected(e) => panic!("Send rejected: {e:?}"),
        };

        // Query Router: no active session yet -> must signal SessionHandshakeRequired
        let route_ctx = RouteContext {
            target_peer_id: id_b.peer_id(),
            target_short_id: s_b,
            mode: MessageMode::Normal,
            is_direct_ble_neighbor: true,
            has_authenticated_session: false, // Session not yet established
            gateway_route_available: None,
            gateway_opted_in: false,
            iroh_path: IrohPathObservation::Unknown,
            privacy_block_relay: false,
            iroh_endpoint_relay_disabled: false,
            attempt_count: 0,
            max_attempts: 10,
            ble_radio_ready: true,
        };

        let decision = Router::decide(&route_ctx);
        assert_eq!(decision.reason, RouteReason::SessionHandshakeRequired);
        assert!(decision.needs_handshake);
        assert_eq!(decision.candidate, RouteCandidate::DirectBle(s_b));

        // ---------------------------------------------------------------------
        // Step 2: Establish Authenticated Noise Session
        // ---------------------------------------------------------------------
        session_mgr_a
            .initiate_handshake(&mut node_a, s_b)
            .await
            .expect("Initiate handshake");

        // Flight 1: A -> B
        node_b.step_at(sim_time).await.unwrap();
        let f1 = node_b.recv_inbound().expect("Node B receives flight 1");
        assert_eq!(f1.msg_type, MsgType::Handshake);
        session_mgr_b
            .handle_handshake(&mut node_b, &f1)
            .await
            .expect("B handles flight 1 and sends flight 2");

        // Flight 2: B -> A
        node_a.step_at(sim_time).await.unwrap();
        let f2 = node_a.recv_inbound().expect("Node A receives flight 2");
        assert_eq!(f2.msg_type, MsgType::Handshake);
        session_mgr_a
            .handle_handshake(&mut node_a, &f2)
            .await
            .expect("A handles flight 2 and sends flight 3");

        // Flight 3: A -> B
        node_b.step_at(sim_time).await.unwrap();
        let f3 = node_b.recv_inbound().expect("Node B receives flight 3");
        assert_eq!(f3.msg_type, MsgType::Handshake);
        session_mgr_b
            .handle_handshake(&mut node_b, &f3)
            .await
            .expect("B handles flight 3");

        // Confirm both sessions are authenticated and established
        assert!(session_mgr_a.is_session_ready(&s_b));
        assert!(session_mgr_b.is_session_ready(&s_a));

        // ---------------------------------------------------------------------
        // Step 3: Attempt 1 — Send Message Frame & Simulate Lost ACK
        // ---------------------------------------------------------------------
        // Router now confirms DirectBleNeighbor with session active
        let mut route_ctx_ready = route_ctx.clone();
        route_ctx_ready.has_authenticated_session = true;
        let ready_dec = Router::decide(&route_ctx_ready);
        assert_eq!(ready_dec.reason, RouteReason::DirectBleNeighbor);
        assert!(!ready_dec.needs_handshake);

        // Enqueue into DeliveryCoordinator
        coord_a.enqueue(
            EnqueueMessage {
                logical_id,
                recipient_peer_id: id_b.peer_id(),
                recipient_short_id: s_b,
                body: raw_payload.clone(),
                ttl: Duration::from_secs(60),
                max_attempts: 5,
                is_privacy_mode: false,
            },
            sim_time,
        );

        // Mark attempt 1 in flight
        coord_a
            .mark_in_flight(&logical_id, Duration::from_millis(50), sim_time)
            .unwrap();
        outbox_a.mark_in_flight(&logical_id, 100).unwrap();

        // Construct ApplicationFrame
        let app_frame_1 = ApplicationFrame::new_message(
            logical_id,
            id_a.peer_id(),
            id_b.peer_id(),
            raw_payload.clone(),
            Duration::from_secs(60),
            100,
        );
        let encoded_frame_1 = app_frame_1.encode();
        let padded_frame_1 = pad_inner_payload(&encoded_frame_1, true).unwrap();

        // Send attempt 1
        let packet_id_1 = session_mgr_a
            .send_encrypted(&mut node_a, s_b, &padded_frame_1)
            .await
            .unwrap();

        // Node B receives attempt 1
        node_b.step_at(sim_time).await.unwrap();
        let in_msg_1 = node_b.recv_inbound().expect("B receives attempt 1");
        assert_eq!(in_msg_1.msg_type, MsgType::Chat);

        let decrypted_1 = session_mgr_b
            .decrypt_inbound(&in_msg_1)
            .expect("B decrypts attempt 1");
        assert_eq!(decrypted_1.remote_peer_id, id_a.peer_id());

        let unpadded_1 = unpad_bucket(&decrypted_1.plaintext).unwrap();
        let parsed_frame_1 = ApplicationFrame::decode(&unpadded_1).unwrap();
        assert_eq!(parsed_frame_1.logical_id, logical_id);

        // Inbox deduplication: Attempt 1 is New
        let dedup_res_1 = dedup_b.check_or_insert(&id_a.peer_id(), &logical_id, sim_time);
        assert_eq!(dedup_res_1, DedupStatus::New);

        // Node B generates ACK
        let ack_frame_1 =
            ApplicationFrame::new_ack(logical_id, id_b.peer_id(), id_a.peer_id(), 100);
        let padded_ack_1 = pad_inner_payload(&ack_frame_1.encode(), false).unwrap();
        let _ack_pkt_1 = session_mgr_b
            .send_encrypted(&mut node_b, s_a, &padded_ack_1)
            .await
            .unwrap();

        // *** SIMULATE LOST ACK ***
        // Node A transport does NOT deliver the ACK (drop in transit).
        let _dropped_ack = node_a.step_at(sim_time).await; // drain or drop
        let _ = node_a.recv_inbound(); // Discard ACK

        // ---------------------------------------------------------------------
        // Step 4: Coordinator Timeout & Attempt 2 (Retry Verification)
        // ---------------------------------------------------------------------
        // Advance clock past ACK timeout (50ms)
        let sim_time_retry = sim_time + Duration::from_millis(60);
        let actions = coord_a.check_timeouts(sim_time_retry);
        assert_eq!(actions, vec![DeliveryAction::ReadyToRetry(logical_id)]);

        // Mark attempt 2 in flight
        coord_a
            .mark_in_flight(&logical_id, Duration::from_millis(100), sim_time_retry)
            .unwrap();
        outbox_a.mark_in_flight(&logical_id, 160).unwrap();

        // Transmit Retry (Attempt 2)
        // CRITICAL INVARIANT: Same logical_id, but send_encrypted generates:
        // 1. Fresh ephemeral packet_id
        // 2. Fresh strictly increasing Noise transport nonce
        let app_frame_2 = ApplicationFrame::new_message(
            logical_id, // Same logical ID!
            id_a.peer_id(),
            id_b.peer_id(),
            raw_payload.clone(),
            Duration::from_secs(60),
            160,
        );
        let padded_frame_2 = pad_inner_payload(&app_frame_2.encode(), true).unwrap();

        let packet_id_2 = session_mgr_a
            .send_encrypted(&mut node_a, s_b, &padded_frame_2)
            .await
            .unwrap();

        // VERIFY: Fresh ephemeral packet ID on retry!
        assert_ne!(
            packet_id_1, packet_id_2,
            "Retries must generate fresh packet IDs to prevent seen-cache suppression"
        );

        // ---------------------------------------------------------------------
        // Step 5: Recipient Deduplication & Fresh ACK Generation
        // ---------------------------------------------------------------------
        node_b.step_at(sim_time_retry).await.unwrap();
        let in_msg_2 = node_b.recv_inbound().expect("B receives attempt 2");

        let decrypted_2 = session_mgr_b
            .decrypt_inbound(&in_msg_2)
            .expect("B decrypts attempt 2 (valid fresh nonce in replay window)");

        let unpadded_2 = unpad_bucket(&decrypted_2.plaintext).unwrap();
        let parsed_frame_2 = ApplicationFrame::decode(&unpadded_2).unwrap();
        assert_eq!(parsed_frame_2.logical_id, logical_id);

        // VERIFY: Deduplicator identifies duplicate logical ID!
        let dedup_res_2 = dedup_b.check_or_insert(&id_a.peer_id(), &logical_id, sim_time_retry);
        assert_eq!(
            dedup_res_2,
            DedupStatus::Duplicate,
            "Recipient inbox deduplicator must catch duplicate logical ID"
        );

        // Recipient re-emits fresh authenticated ACK
        let ack_frame_2 =
            ApplicationFrame::new_ack(logical_id, id_b.peer_id(), id_a.peer_id(), 160);
        let padded_ack_2 = pad_inner_payload(&ack_frame_2.encode(), false).unwrap();
        session_mgr_b
            .send_encrypted(&mut node_b, s_a, &padded_ack_2)
            .await
            .unwrap();

        // ---------------------------------------------------------------------
        // Step 6: ACK Reception & Terminal Outbox Completion
        // ---------------------------------------------------------------------
        node_a.step_at(sim_time_retry).await.unwrap();
        let ack_msg = node_a.recv_inbound().expect("A receives second ACK");
        assert_eq!(ack_msg.msg_type, MsgType::Chat);

        let decrypted_ack = session_mgr_a
            .decrypt_inbound(&ack_msg)
            .expect("A decrypts ACK");
        assert_eq!(decrypted_ack.remote_peer_id, id_b.peer_id());

        let unpadded_ack = unpad_bucket(&decrypted_ack.plaintext).unwrap();
        let parsed_ack = ApplicationFrame::decode(&unpadded_ack).unwrap();
        assert_eq!(parsed_ack.kind, ApplicationFrameKind::Ack);
        assert_eq!(parsed_ack.logical_id, logical_id);

        // Process ACK in DeliveryCoordinator
        let delivered = coord_a
            .handle_ack(&parsed_ack, &decrypted_ack.remote_peer_id)
            .unwrap();
        assert!(delivered);

        // Mark Delivered in OutboxManager
        outbox_a.mark_delivered(&logical_id).unwrap();

        // VERIFY: Final terminal state across all subsystems
        assert_eq!(
            coord_a.get_state(&logical_id),
            Some(DeliveryState::Delivered)
        );
        let outbox_rec = outbox_a.get(&logical_id).unwrap().unwrap();
        assert_eq!(outbox_rec.state, DeliveryState::Delivered);
        assert_eq!(outbox_rec.attempt_count, 2);

        // VERIFY: Terminal state immutability — subsequent timeouts cannot downgrade Delivered
        let post_actions = coord_a.check_timeouts(sim_time_retry + Duration::from_secs(100));
        assert!(post_actions.is_empty());
        assert_eq!(
            coord_a.get_state(&logical_id),
            Some(DeliveryState::Delivered)
        );
    })
    .await
    .expect("test_phase4_delivery_e2e_lifecycle timed out");
}
