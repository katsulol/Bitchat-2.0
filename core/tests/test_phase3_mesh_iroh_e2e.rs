use std::sync::Arc;
use std::time::{Duration, Instant};

use bitchat2_core::envelope::MsgType;
use bitchat2_core::gateway::GatewayBridge;
use bitchat2_core::identity::Identity;
use bitchat2_core::mesh::peer::PeerSessionManager;
use bitchat2_core::mesh::policy::AdaptiveFlood;
use bitchat2_core::mesh::relay::MeshNode;
use bitchat2_core::transport::sim::{SimNetwork, SimTransport};
use iroh::RelayMode;

async fn pump_node(node: &mut MeshNode<SimTransport>) {
    let now = Instant::now();
    for _ in 0..10 {
        if !node.poll_step(now).await.unwrap_or(false) {
            break;
        }
    }
}

#[tokio::test]
async fn test_phase3_four_node_e2e_mesh_over_real_iroh() {
    // -------------------------------------------------------------------------
    // 1. SETUP TWO DISJOINT SIMULATED MESHES (Radio Partitions)
    // Mesh 1: Phone A (offline) <-> Gateway B (online)
    // Mesh 2: Gateway C (online) <-> Phone D (offline)
    // There are NO direct radio links between Mesh 1 and Mesh 2.
    // -------------------------------------------------------------------------
    let net1 = SimNetwork::new(100);
    let net2 = SimNetwork::new(200);

    let id_a = Identity::generate();
    let id_b = Identity::generate();
    let id_c = Identity::generate();
    let id_d = Identity::generate();

    let short_a = id_a.short_id();
    let short_b = id_b.short_id();
    let short_c = id_c.short_id();
    let short_d = id_d.short_id();

    // Transport handles in Mesh 1
    let trans_a = net1.create_node(short_a);
    let trans_b = net1.create_node(short_b);
    net1.set_bidirectional_link(short_a, short_b, Duration::from_millis(0), 0.0);

    // Transport handles in Mesh 2
    let trans_c = net2.create_node(short_c);
    let trans_d = net2.create_node(short_d);
    net2.set_bidirectional_link(short_c, short_d, Duration::from_millis(0), 0.0);

    // Build MeshNodes
    let mut node_a = MeshNode::new(
        Identity::from_bytes(&id_a.to_bytes()),
        trans_a,
        Box::new(AdaptiveFlood::default()),
    );
    let mut node_b = MeshNode::new(
        Identity::from_bytes(&id_b.to_bytes()),
        trans_b,
        Box::new(AdaptiveFlood::default()),
    );
    let mut node_c = MeshNode::new(
        Identity::from_bytes(&id_c.to_bytes()),
        trans_c,
        Box::new(AdaptiveFlood::default()),
    );
    let mut node_d = MeshNode::new(
        Identity::from_bytes(&id_d.to_bytes()),
        trans_d,
        Box::new(AdaptiveFlood::default()),
    );

    let mut peer_mgr_a = PeerSessionManager::new();
    let mut peer_mgr_d = PeerSessionManager::new();

    // -------------------------------------------------------------------------
    // 2. SETUP REAL IROH GATEWAY BRIDGE BETWEEN B AND C
    // -------------------------------------------------------------------------
    let bridge_b = GatewayBridge::new(
        Identity::from_bytes(&id_b.to_bytes()),
        None,
        RelayMode::Disabled,
    )
    .await
    .expect("Failed to create Gateway B");
    let bridge_c = GatewayBridge::new(
        Identity::from_bytes(&id_c.to_bytes()),
        None,
        RelayMode::Disabled,
    )
    .await
    .expect("Failed to create Gateway C");

    // Mutual Contact Exchange and Approval
    let contact_b = bridge_b.export_contact().await.expect("Export contact B");
    let contact_c = bridge_c.export_contact().await.expect("Export contact C");

    bridge_b
        .import_and_approve_contact(&contact_c.export_to_string().unwrap())
        .await
        .expect("B approve C");
    bridge_c
        .import_and_approve_contact(&contact_b.export_to_string().unwrap())
        .await
        .expect("C approve B");

    // Configure manual bidirectional routes:
    // Gateway B: route to Recipient D via Gateway C's EndpointId
    // Gateway C: route to Recipient A via Gateway B's EndpointId
    bridge_b
        .routes()
        .write()
        .await
        .set_route(short_d, bridge_c.endpoint_id());
    bridge_c
        .routes()
        .write()
        .await
        .set_route(short_a, bridge_b.endpoint_id());

    // Connect Gateway B -> Gateway C over authenticated Iroh link
    let accept_bridge_c = bridge_c.endpoint().clone();
    let accept_task = tokio::spawn(async move {
        let (remote_id, conn) =
            bitchat2_core::gateway::iroh_link::accept_gateway_conn(&accept_bridge_c)
                .await
                .expect("C accept connection");
        (remote_id, conn)
    });

    bridge_b
        .connect_to_contact(&contact_c)
        .await
        .expect("B connect to C");

    let (_remote_b_id, conn_from_b) = accept_task.await.expect("Accept task panicked");
    bridge_c
        .spawn_session_handler(conn_from_b, None)
        .await
        .expect("C spawn session handler");

    // Attach bridges to gateway mesh nodes
    let bridge_b = Arc::new(bridge_b);
    let bridge_c = Arc::new(bridge_c);
    node_b.set_gateway_bridge(Arc::clone(&bridge_b));
    node_c.set_gateway_bridge(Arc::clone(&bridge_c));

    // Wait 50ms for QUIC stream synchronization
    tokio::time::sleep(Duration::from_millis(50)).await;

    // -------------------------------------------------------------------------
    // 3. STEP NOISE HANDSHAKE: A -> B -> Iroh -> C -> D and back
    // -------------------------------------------------------------------------
    // Node A initiates handshake with D
    let _ = peer_mgr_a.initiate_handshake(&mut node_a, short_d).await;

    // Pump packets through the network
    for _ in 0..40 {
        pump_node(&mut node_a).await;
        pump_node(&mut node_b).await;
        tokio::time::sleep(Duration::from_millis(5)).await;
        pump_node(&mut node_c).await;
        pump_node(&mut node_d).await;

        // D processes incoming handshake
        while let Some(msg) = node_d.recv_inbound() {
            if msg.msg_type == MsgType::Handshake {
                let _ = peer_mgr_d.handle_handshake(&mut node_d, &msg).await;
            }
        }

        pump_node(&mut node_d).await;
        pump_node(&mut node_c).await;
        tokio::time::sleep(Duration::from_millis(5)).await;
        pump_node(&mut node_b).await;
        pump_node(&mut node_a).await;

        // A processes reply
        while let Some(msg) = node_a.recv_inbound() {
            if msg.msg_type == MsgType::Handshake {
                let _ = peer_mgr_a.handle_handshake(&mut node_a, &msg).await;
            }
        }

        if let (Some(s_a), Some(s_d)) = (
            peer_mgr_a.get_session(&short_d),
            peer_mgr_d.get_session(&short_a),
        ) {
            if s_a.is_handshake_finished() && s_d.is_handshake_finished() {
                break;
            }
        }
    }

    // Verify Noise session established on both ends
    let session_a = peer_mgr_a.get_session(&short_d).expect("Session A exists");
    assert!(
        session_a.is_handshake_finished(),
        "Handshake on node A must be finished"
    );

    let session_d = peer_mgr_d.get_session(&short_a).expect("Session D exists");
    assert!(
        session_d.is_handshake_finished(),
        "Handshake on node D must be finished"
    );

    // -------------------------------------------------------------------------
    // 4. VERIFY BIDIRECTIONAL ENCRYPTED MESSAGING A -> D AND D -> A
    // -------------------------------------------------------------------------
    let secret_from_a = b"CONFIDENTIAL: Hello Phone D from offline Phone A over BLE and Iroh!";
    let _msg_id_a = peer_mgr_a
        .send_encrypted(&mut node_a, short_d, secret_from_a)
        .await
        .expect("A send encrypted to D");

    // Pump messages across bridge
    for _ in 0..30 {
        pump_node(&mut node_a).await;
        pump_node(&mut node_b).await;
        tokio::time::sleep(Duration::from_millis(5)).await;
        pump_node(&mut node_c).await;
        pump_node(&mut node_d).await;
        if node_d.inbound_count() > 0 {
            break;
        }
    }

    let received_by_d = node_d
        .recv_inbound()
        .expect("D must receive message from A");
    let decrypted_by_d = peer_mgr_d
        .decrypt_inbound(&received_by_d)
        .expect("D decrypt message");

    assert_eq!(decrypted_by_d.origin_sender, short_a);
    assert_eq!(decrypted_by_d.plaintext, secret_from_a);

    // D replies to A with encrypted message
    let reply_from_d = b"ACK: Received your encrypted message loud and clear, A!";
    let _msg_id_d = peer_mgr_d
        .send_encrypted(&mut node_d, short_a, reply_from_d)
        .await
        .expect("D send encrypted to A");

    for _ in 0..30 {
        pump_node(&mut node_d).await;
        pump_node(&mut node_c).await;
        tokio::time::sleep(Duration::from_millis(5)).await;
        pump_node(&mut node_b).await;
        pump_node(&mut node_a).await;
        if node_a.inbound_count() > 0 {
            break;
        }
    }

    let received_by_a = node_a.recv_inbound().expect("A must receive reply from D");
    let decrypted_by_a = peer_mgr_a
        .decrypt_inbound(&received_by_a)
        .expect("A decrypt reply");

    assert_eq!(decrypted_by_a.origin_sender, short_d);
    assert_eq!(decrypted_by_a.plaintext, reply_from_d);

    // -------------------------------------------------------------------------
    // 5. VERIFY BROADCAST ISOLATION (Broadcasts NEVER cross the gateway)
    // -------------------------------------------------------------------------
    let broadcast_text = b"Public broadcast for local mesh 1 only";
    node_a
        .send_broadcast(broadcast_text.to_vec())
        .await
        .expect("A send broadcast");

    for _ in 0..15 {
        pump_node(&mut node_a).await;
        pump_node(&mut node_b).await;
        tokio::time::sleep(Duration::from_millis(5)).await;
        pump_node(&mut node_c).await;
        pump_node(&mut node_d).await;
    }

    // D must NOT receive A's broadcast!
    assert!(
        node_d.recv_inbound().is_none(),
        "Public broadcasts must NEVER cross the internet gateway"
    );

    // -------------------------------------------------------------------------
    // 6. CLEAN SHUTDOWN
    // -------------------------------------------------------------------------
    bridge_b.close().await;
    bridge_c.close().await;
}
