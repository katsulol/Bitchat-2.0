use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bitchat2_core::api::{BitchatCoreService, BridgeMessage};
use bitchat2_core::identity::Identity;
use bitchat2_core::storage::key::{FailingKeyProvider, InjectedKeyProvider, KeyProvider};
use bitchat2_core::trust::{
    AuthOutcome, Capability, PeerTrustState, SqliteTrustStore, TrustQuotaConfig, TrustStore,
};

fn fresh_temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bitchat_p6_trust_{label}_{}",
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
    dir: PathBuf,
    dek: [u8; 32],
}

impl NodeHarness {
    async fn new_with_dir(dir: PathBuf, dek: [u8; 32]) -> Result<Self, String> {
        let mut svc = BitchatCoreService::new_with_key(
            dir.to_str().expect("utf8 path").to_string(),
            Some(dek),
            1,
        )
        .await?;
        svc.set_ack_timeout(Duration::from_millis(80));
        Ok(Self {
            svc,
            inbox: Vec::new(),
            neighbors: HashSet::new(),
            dir,
            dek,
        })
    }

    async fn new(label: &str) -> Self {
        let dir = fresh_temp_dir(label);
        Self::new_with_dir(dir, [0x51u8; 32])
            .await
            .expect("init node")
    }

    async fn restart(mut self) -> Result<Self, String> {
        let _ = self.svc.gateway_stop().await;
        let dir = self.dir;
        let dek = self.dek;
        Self::new_with_dir(dir, dek).await
    }

    async fn shutdown(mut self) {
        let _ = self.svc.gateway_stop().await;
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

async fn setup_ble_pair(nodes: &mut [NodeHarness], u: usize, v: usize) {
    connect_ble(nodes, u, v).await;
    let u_short = nodes[u].svc.my_short_id().to_string();
    let v_short = nodes[v].svc.my_short_id().to_string();
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
async fn test_t2_approve_block_revoke_voucher_persist_across_restart() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut bob = NodeHarness::new("p6_t2_bob").await;
        let eve = NodeHarness::new("p6_t2_eve").await;
        let dave = NodeHarness::new("p6_t2_dave").await;
        let mut carol = NodeHarness::new("p6_t2_carol").await;

        let bob_pid = bob.svc.my_peer_id();
        let eve_pid = eve.svc.my_peer_id();
        let dave_pid = dave.svc.my_peer_id();
        let carol_pid = carol.svc.my_peer_id();

        bob.svc
            .trust_set_peer_state(eve_pid.to_string(), "block".to_string())
            .await
            .unwrap();
        bob.svc
            .trust_set_peer_state(carol_pid.to_string(), "introducer".to_string())
            .await
            .unwrap();
        let voucher_uri = carol
            .svc
            .trust_issue_voucher(
                dave_pid.to_string(),
                bob_pid.to_string(),
                "direct_message".to_string(),
                600,
            )
            .await
            .unwrap();
        bob.svc.trust_import_voucher(voucher_uri).await.unwrap();

        let bob_dir = bob.dir.clone();
        // Restart Bob cleanly from disk
        bob = bob.restart().await.unwrap();

        {
            let ts_arc = bob.svc.trust_store_arc();
            let ts = ts_arc.read().await;
            assert!(matches!(
                ts.peer_state(&eve_pid),
                PeerTrustState::Blocked { .. }
            ));
            assert!(ts.is_approved_introducer(&carol_pid));
            assert_eq!(ts.active_voucher_count(), 1);
        }

        // Verify persisted block & persisted voucher enforce real BLE admission after restart
        let mut nodes = vec![bob, eve, dave];
        setup_ble_pair(&mut nodes, 0, 1).await;
        setup_ble_pair(&mut nodes, 0, 2).await;

        let _ = nodes[1]
            .svc
            .send_message(bob_pid.to_string(), "blocked after reboot".to_string(), 0)
            .await
            .unwrap();
        let _ = nodes[2]
            .svc
            .send_message(bob_pid.to_string(), "vouched after reboot".to_string(), 0)
            .await
            .unwrap();
        pump_ble_and_inbound(&mut nodes, 14).await;

        assert_eq!(nodes[0].inbox.len(), 1);
        assert_eq!(nodes[0].inbox[0].text, "vouched after reboot");
        assert_eq!(nodes[0].inbox[0].auth_status, "Allowed");

        // Now revoke Dave/Carol on Bob and restart again -> voucher invalidated across reboot!
        nodes[0]
            .svc
            .trust_set_peer_state(carol_pid.to_string(), "revoke".to_string())
            .await
            .unwrap();
        let bob_restarted = nodes.remove(0).restart().await.unwrap();
        {
            let ts_arc = bob_restarted.svc.trust_store_arc();
            let ts = ts_arc.read().await;
            assert!(matches!(
                ts.peer_state(&carol_pid),
                PeerTrustState::Revoked { .. }
            ));
            assert_eq!(ts.active_voucher_count(), 0);
        }

        bob_restarted.shutdown().await;
        for n in nodes {
            n.shutdown().await;
        }
        carol.shutdown().await;
        let _ = std::fs::remove_dir_all(bob_dir);
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn test_t2_gateway_started_before_and_after_ui_actions_plus_block_restart() {
    tokio::time::timeout(Duration::from_secs(20), async {
        // Case 1: Start gateway BEFORE blocking peer, approve dialer via pairing contact, block peer, restart, start gateway AFTER -> still blocked!
        let mut host = NodeHarness::new("p6_t2_gw_host").await;
        let mut dialer = NodeHarness::new("p6_t2_gw_dialer").await;

        let host_dir = host.dir.clone();
        let dialer_dir = dialer.dir.clone();

        host.svc
            .gateway_start(Some("disabled".to_string()))
            .await
            .unwrap();
        dialer
            .svc
            .gateway_start(Some("disabled".to_string()))
            .await
            .unwrap();

        let pairing_contact = host.svc.gateway_export_contact().await.unwrap();
        dialer
            .svc
            .gateway_import_and_approve_contact(pairing_contact.clone())
            .await
            .unwrap();
        dialer
            .svc
            .gateway_connect_to_contact(pairing_contact.clone())
            .await
            .unwrap();

        // Wait until host approves dialer via single-use invitation
        let mut dialer_ep = None;
        for _ in 0..40 {
            let stats = host.svc.gateway_get_stats().await.unwrap();
            if stats.contains("\"active_connections\":1") {
                let b = host.svc.gateway_bridge_arc().unwrap();
                let cfg_arc = b.config();
                let eps = cfg_arc.read().await;
                if eps.is_approved(&dialer.svc.gateway_bridge_arc().unwrap().endpoint_id()) {
                    dialer_ep = Some(dialer.svc.gateway_bridge_arc().unwrap().endpoint_id());
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let dialer_ep = dialer_ep.expect("Dialer must be admitted initially");

        // Block dialer gateway via UI action WHILE gateway is running
        host.svc
            .gateway_revoke_or_block_peer(dialer_ep.to_string(), true)
            .await
            .unwrap();

        // Export a fresh pairing contact later -> even old/new invitation must NOT undo block after restart!
        host = host.restart().await.unwrap();
        host.svc
            .gateway_start(Some("disabled".to_string()))
            .await
            .unwrap();
        let fresh_contact_str = host.svc.gateway_export_contact().await.unwrap();
        assert!(
            host.svc
                .gateway_approve_peer(dialer_ep.to_string())
                .await
                .is_err(),
            "Persisted gateway block must survive restart and reject re-approval"
        );
        assert!(
            host.svc
                .gateway_import_and_approve_contact(
                    dialer.svc.gateway_export_contact().await.unwrap()
                )
                .await
                .is_err(),
            "Old or re-imported invitation/contact must NEVER undo persisted block"
        );

        // Attempt dial -> host gateway rejects because persisted Blocked state seeded on gateway_start
        let fresh_contact =
            bitchat2_core::gateway::GatewayContact::import_from_str(&fresh_contact_str).unwrap();
        let _ = dialer
            .svc
            .gateway_connect_to_contact(fresh_contact_str)
            .await;
        tokio::time::sleep(Duration::from_millis(80)).await;
        let _ = fresh_contact;
        assert!(
            !host
                .svc
                .gateway_bridge_arc()
                .unwrap()
                .config()
                .read()
                .await
                .is_approved(&dialer_ep),
            "Blocked gateway endpoint must stay unapproved across restart"
        );

        host.shutdown().await;
        dialer.shutdown().await;
        let _ = std::fs::remove_dir_all(host_dir);
        let _ = std::fs::remove_dir_all(dialer_dir);
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn test_t2_unavailable_key_and_tampered_trust_db_fail_closed() {
    let dir = fresh_temp_dir("p6_t2_fail_closed");
    let dek = [0x33u8; 32];
    let target_pid = Identity::generate().peer_id();

    {
        let mut svc =
            BitchatCoreService::new_with_key(dir.to_str().unwrap().to_string(), Some(dek), 1)
                .await
                .unwrap();
        svc.trust_set_peer_state(target_pid.to_string(), "block".to_string())
            .await
            .unwrap();
    }

    // 1. Missing platform DEK (`None`) while trust/outbox DB exists -> fails closed!
    let missing_res =
        BitchatCoreService::new_with_key(dir.to_str().unwrap().to_string(), None, 1).await;
    assert!(missing_res.is_err(), "Missing DEK must fail closed");

    // 2. FailingKeyProvider -> fails closed!
    let failing_prov: Arc<dyn KeyProvider> = Arc::new(FailingKeyProvider::new("keystore locked"));
    let fail_res =
        BitchatCoreService::new_with_key_provider(dir.to_str().unwrap().to_string(), failing_prov)
            .await;
    assert!(fail_res.is_err(), "FailingKeyProvider must fail closed");

    // 3. Wrong DEK -> fails closed!
    let wrong_res =
        BitchatCoreService::new_with_key(dir.to_str().unwrap().to_string(), Some([0x99u8; 32]), 1)
            .await;
    assert!(wrong_res.is_err(), "Wrong DEK must fail closed");

    // 4. SQL column tamper (`state_tag = 2` Approved) -> fails AEAD and refuses to start!
    {
        let db_path = dir.join("bitchat_trust.db");
        let raw_conn = rusqlite::Connection::open(&db_path).unwrap();
        raw_conn
            .execute("UPDATE peer_trust SET state_tag = 2", [])
            .unwrap();
    }
    let tamper_res =
        BitchatCoreService::new_with_key(dir.to_str().unwrap().to_string(), Some(dek), 1).await;
    assert!(
        tamper_res.is_err(),
        "Tampered trust DB row must fail closed, never fall back to empty TrustStore"
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn test_t2_expired_voucher_rollback_sealed_concurrent_updates_and_legacy_migration() {
    let dir = fresh_temp_dir("p6_t2_misc");
    let local = Identity::generate().peer_id();
    let introducer = Identity::generate();
    let subject = Identity::generate();
    let kp: Arc<dyn KeyProvider> = Arc::new(InjectedKeyProvider::new([0x88u8; 32], 1));
    let db_path = dir.join("bitchat_trust.db");

    // 1. Legacy migration: explicit consent -> Approved, unproven -> PendingConsent
    let proven_peer = Identity::generate().peer_id();
    let legacy_peer = Identity::generate().peer_id();
    {
        let db = SqliteTrustStore::open(&db_path, local, Arc::clone(&kp)).unwrap();
        let mut caps = HashSet::new();
        caps.insert(Capability::ReceiveDirectMessage);
        db.migrate_legacy_approvals(&[
            (proven_peer, caps.clone(), 1_700_000_000, true),
            (legacy_peer, caps, 1_700_000_000, false),
        ])
        .unwrap();
    }

    // 2. Concurrent updates + expired voucher sealing across restart + rollback
    let db = Arc::new(SqliteTrustStore::open(&db_path, local, Arc::clone(&kp)).unwrap());
    let mut store = TrustStore::new(local, TrustQuotaConfig::default());
    db.load_into(&mut store, Instant::now(), Some(1_700_000_010))
        .unwrap();
    assert!(matches!(
        store.peer_state(&proven_peer),
        PeerTrustState::Approved { .. }
    ));
    assert_eq!(
        store.peer_state(&legacy_peer),
        PeerTrustState::IdentityVerifiedPendingConsent
    );

    let mut handles = Vec::new();
    for i in 0..8 {
        let db_c = Arc::clone(&db);
        handles.push(tokio::spawn(async move {
            let p = Identity::generate().peer_id();
            db_c.upsert_peer_state(
                &p,
                &PeerTrustState::Revoked {
                    revoked_at_unix: 1_700_000_100 + i,
                },
                bitchat2_core::trust::TrustProvenance::ExplicitLocal,
            )
            .unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    // Insert voucher expiring at 1_700_000_060, then load at 1_700_000_080 (expired -> permanently sealed)
    let mut store_v = TrustStore::new(local, TrustQuotaConfig::default());
    let mut intro_caps = HashSet::new();
    intro_caps.insert(Capability::ActAsIntroducer);
    store_v
        .approve_peer(introducer.peer_id(), intro_caps, 1_700_000_000)
        .unwrap();
    db.upsert_peer_state(
        &introducer.peer_id(),
        &store_v.peer_state(&introducer.peer_id()),
        bitchat2_core::trust::TrustProvenance::ExplicitLocal,
    )
    .unwrap();
    let v = bitchat2_core::trust::SignedVoucher::issue(
        &introducer,
        subject.peer_id(),
        local,
        Capability::ReceiveDirectMessage,
        1_700_000_000,
        1_700_000_060,
        1,
    )
    .unwrap();
    store_v
        .import_voucher(&v, Instant::now(), Some(1_700_000_005))
        .unwrap();
    db.upsert_voucher(store_v.get_voucher(&v.credential_id).unwrap())
        .unwrap();
    drop(db);

    // Process restart after expiry (wall clock 1_700_000_080) -> seals permanent expiry to disk
    {
        let db_after = SqliteTrustStore::open(&db_path, local, Arc::clone(&kp)).unwrap();
        let mut s_after = TrustStore::new(local, TrustQuotaConfig::default());
        db_after
            .load_into(&mut s_after, Instant::now(), Some(1_700_000_080))
            .unwrap();
    }

    // Roll wall clock BACK to 1_700_000_010 on fresh restart -> voucher MUST NOT resurrect!
    {
        let db_rb = SqliteTrustStore::open(&db_path, local, Arc::clone(&kp)).unwrap();
        let mut s_rb = TrustStore::new(local, TrustQuotaConfig::default());
        db_rb
            .load_into(&mut s_rb, Instant::now(), Some(1_700_000_010))
            .unwrap();
        let dec = bitchat2_core::trust::TrustPolicy::authorize(
            &mut s_rb,
            &subject.peer_id(),
            Capability::ReceiveDirectMessage,
            &bitchat2_core::trust::AuthorizationContext {
                now_monotonic: Instant::now(),
                now_unix: Some(1_700_000_010),
                gateway_opted_in: true,
                allow_vouched_gateway_forwarding: false,
            },
        );
        assert_ne!(
            dec.outcome,
            AuthOutcome::Allow,
            "Expired voucher must never resurrect after clock rollback + restart"
        );
    }

    let _ = std::fs::remove_dir_all(dir);
}
