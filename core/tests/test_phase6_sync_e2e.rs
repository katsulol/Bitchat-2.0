use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use bitchat2_core::api::{BitchatCoreService, BridgeMessage};
use bitchat2_core::identity::{Identity, PeerId};
use bitchat2_core::storage::{InjectedKeyProvider, KeyProvider};
use bitchat2_core::sync::{
    conversation_id, protocol::encode_sync_chunks, SqliteSyncStore, SyncFrame, SyncRecord,
    SyncRecordKind, SyncSessionManager, SyncSnapshot, MAX_MERKLE_NODES_PER_SESSION,
    MAX_SYNC_SESSION_BYTES,
};

struct SyncNodeBox {
    dir: PathBuf,
    dek: [u8; 32],
    id_copy: Identity,
    svc: BitchatCoreService,
    inbox: Vec<BridgeMessage>,
}

impl SyncNodeBox {
    async fn new(tag: &str, key_byte: u8) -> Self {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("bitchat_sync_{tag}_{ts}_{}", rand::random::<u32>()));
        std::fs::create_dir_all(&dir).unwrap();
        let dek = [key_byte; 32];
        let id_copy = Identity::generate();
        let kp: Arc<dyn KeyProvider> = Arc::new(InjectedKeyProvider::new(dek, 1));
        // Pre-seed identity via ProtectedIdentityStore? No, let service create & read from disk if needed,
        // or use store helper when signing records via service API!
        let svc = BitchatCoreService::new_with_key_provider(dir.to_string_lossy().into_owned(), kp)
            .await
            .unwrap();
        Self {
            dir,
            dek,
            id_copy,
            svc,
            inbox: Vec::new(),
        }
    }

    async fn restart(&mut self) {
        let kp: Arc<dyn KeyProvider> = Arc::new(InjectedKeyProvider::new(self.dek, 1));
        let mut dummy = Self::new("tmp_sync_swap", 0xEE).await;
        std::mem::swap(&mut self.svc, &mut dummy.svc);
        drop(dummy.svc);
        let _ = std::fs::remove_dir_all(&dummy.dir);
        self.svc =
            BitchatCoreService::new_with_key_provider(self.dir.to_string_lossy().into_owned(), kp)
                .await
                .unwrap();
    }

    fn pid(&self) -> PeerId {
        self.svc.my_peer_id()
    }

    fn pid_hex(&self) -> String {
        self.svc.my_peer_id().to_string()
    }

    fn short_hex(&self) -> String {
        self.svc.my_short_id().to_string()
    }

    async fn pump(&mut self) {
        let _ = self.svc.pump_outbox().await;
        for _ in 0..64 {
            match self.svc.poll_inbound().await {
                Ok(Some(m)) => self.inbox.push(m),
                _ => break,
            }
        }
    }

    async fn cleanup(mut self) {
        let _ = self.svc.gateway_stop().await;
        let _ = self.id_copy.peer_id();
        drop(self.svc);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn connect_pair(a: &mut SyncNodeBox, b: &mut SyncNodeBox) {
    let a_sh = a.short_hex();
    let b_sh = b.short_hex();
    a.svc.on_peer_connected(b_sh.clone()).await.unwrap();
    b.svc.on_peer_connected(a_sh.clone()).await.unwrap();
    a.svc
        .trust_set_peer_state(b.pid_hex(), "approve".into())
        .await
        .unwrap();
    b.svc
        .trust_set_peer_state(a.pid_hex(), "approve".into())
        .await
        .unwrap();
    for _ in 0..30 {
        while let Some(f) = a.svc.poll_outbound() {
            let _ = b.svc.on_frame_received(a_sh.clone(), f.bytes);
        }
        while let Some(f) = b.svc.poll_outbound() {
            let _ = a.svc.on_frame_received(b_sh.clone(), f.bytes);
        }
        a.pump().await;
        b.pump().await;
        if a.svc.is_session_ready(&b_sh).unwrap() && b.svc.is_session_ready(&a_sh).unwrap() {
            break;
        }
    }
    assert!(a.svc.is_session_ready(&b_sh).unwrap());
    assert!(b.svc.is_session_ready(&a_sh).unwrap());
}

async fn exchange_all(a: &mut SyncNodeBox, b: &mut SyncNodeBox) {
    let a_sh = a.short_hex();
    let b_sh = b.short_hex();
    for _ in 0..40 {
        let mut moved = false;
        while let Some(f) = a.svc.poll_outbound() {
            let _ = b.svc.on_frame_received(a_sh.clone(), f.bytes);
            moved = true;
        }
        b.pump().await;
        while let Some(f) = b.svc.poll_outbound() {
            let _ = a.svc.on_frame_received(b_sh.clone(), f.bytes);
            moved = true;
        }
        a.pump().await;
        if !moved {
            break;
        }
    }
}

fn temp_store_pair() -> (
    PathBuf,
    Identity,
    Identity,
    SqliteSyncStore,
    SqliteSyncStore,
) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("bitchat_sync_bench_{ts}_{}", rand::random::<u32>()));
    std::fs::create_dir_all(&dir).unwrap();
    let id_a = Identity::generate();
    let id_b = Identity::generate();
    let kp_a: Arc<dyn KeyProvider> = Arc::new(InjectedKeyProvider::new([0x41u8; 32], 1));
    let kp_b: Arc<dyn KeyProvider> = Arc::new(InjectedKeyProvider::new([0x42u8; 32], 1));
    let st_a = SqliteSyncStore::open(&dir.join("a.db"), id_a.peer_id(), kp_a).unwrap();
    let st_b = SqliteSyncStore::open(&dir.join("b.db"), id_b.peer_id(), kp_b).unwrap();
    (dir, id_a, id_b, st_a, st_b)
}

fn make_record(
    author: &Identity,
    recipient: PeerId,
    seed: u64,
    created_at: u64,
    kind: SyncRecordKind,
    body_text: &str,
) -> SyncRecord {
    let h = blake3::hash(&seed.to_be_bytes());
    let mut lid = [0u8; 16];
    lid.copy_from_slice(&h.as_bytes()[0..16]);
    let body = if kind == SyncRecordKind::Tombstone {
        Vec::new()
    } else {
        body_text.as_bytes().to_vec()
    };
    SyncRecord::create_signed(
        author,
        recipient,
        lid,
        created_at,
        created_at + 86_400,
        kind,
        body,
    )
    .unwrap()
}

fn run_sync_session_pair(
    id_a: &Identity,
    st_a: &SqliteSyncStore,
    id_b: &Identity,
    st_b: &SqliteSyncStore,
    use_merkle: bool,
    now_unix: u64,
) -> (usize, u32, usize) {
    let mut mgr_a = SyncSessionManager::new(true);
    let mut mgr_b = SyncSessionManager::new(true);
    let now_inst = Instant::now();

    let mut q_a_to_b = mgr_a
        .initiate_session(
            id_a.peer_id(),
            id_b.peer_id(),
            st_a,
            use_merkle,
            now_inst,
            now_unix,
        )
        .unwrap();
    let mut q_b_to_a: Vec<Vec<u8>> = Vec::new();
    let mut total_wire_bytes: usize = q_a_to_b.iter().map(|c| c.len()).sum();

    for _ in 0..500 {
        if q_a_to_b.is_empty() && q_b_to_a.is_empty() {
            break;
        }
        let batch_a = std::mem::take(&mut q_a_to_b);
        for chunk in batch_a {
            let out = mgr_b.handle_inbound_chunk(
                id_b.peer_id(),
                id_a.peer_id(),
                &chunk,
                Some(st_b),
                now_inst,
                now_unix,
            );
            for oc in out.outbound_chunks {
                total_wire_bytes += oc.len();
                q_b_to_a.push(oc);
            }
        }
        let batch_b = std::mem::take(&mut q_b_to_a);
        for chunk in batch_b {
            let out = mgr_a.handle_inbound_chunk(
                id_a.peer_id(),
                id_b.peer_id(),
                &chunk,
                Some(st_a),
                now_inst,
                now_unix,
            );
            for oc in out.outbound_chunks {
                total_wire_bytes += oc.len();
                q_a_to_b.push(oc);
            }
        }
    }

    let ma = mgr_a.metrics_for(&id_b.peer_id()).unwrap();
    let mb = mgr_b.metrics_for(&id_a.peer_id()).unwrap();
    assert!(
        ma.completed,
        "Alice sync must finish cleanly: {:?}",
        ma.aborted_reason
    );
    assert!(
        mb.completed,
        "Bob sync must finish cleanly: {:?}",
        mb.aborted_reason
    );
    (
        total_wire_bytes,
        ma.rounds.max(mb.rounds),
        ma.merkle_nodes_queried.max(mb.merkle_nodes_queried),
    )
}

#[tokio::test]
async fn test_t9_sync_empty_equal_disjoint_partial_both_modes() {
    for use_merkle in [false, true] {
        // 1. Empty sets
        let mut a = SyncNodeBox::new("empty_a", 0x11).await;
        let mut b = SyncNodeBox::new("empty_b", 0x12).await;
        connect_pair(&mut a, &mut b).await;
        a.svc.set_sync_enabled(true).unwrap();
        b.svc.set_sync_enabled(true).unwrap();

        a.svc.initiate_sync(&b.pid_hex(), use_merkle).await.unwrap();
        exchange_all(&mut a, &mut b).await;
        assert!(a
            .svc
            .list_sync_records(&b.pid_hex())
            .await
            .unwrap()
            .is_empty());
        assert!(b
            .svc
            .list_sync_records(&a.pid_hex())
            .await
            .unwrap()
            .is_empty());

        // 2. Partial overlap / disjoint: Alice sends 3 Normal msgs, Bob sends 2 Normal msgs (with ACKs delivered!)
        for i in 0..3 {
            a.svc
                .send_message(b.pid_hex(), format!("alice-{i}"), 0)
                .await
                .unwrap();
            exchange_all(&mut a, &mut b).await;
        }
        for i in 0..2 {
            b.svc
                .send_message(a.pid_hex(), format!("bob-{i}"), 0)
                .await
                .unwrap();
            exchange_all(&mut a, &mut b).await;
        }
        // Right now Alice has alice-0..2 (signed by Alice) and Bob has bob-0..1 (signed by Bob)! Disjoint archives!
        a.svc.initiate_sync(&b.pid_hex(), use_merkle).await.unwrap();
        exchange_all(&mut a, &mut b).await;

        let recs_a = a.svc.list_sync_records(&b.pid_hex()).await.unwrap();
        let recs_b = b.svc.list_sync_records(&a.pid_hex()).await.unwrap();
        assert_eq!(recs_a.len(), 5);
        assert_eq!(recs_b.len(), 5);
        assert_eq!(recs_a, recs_b, "Both nodes must converge deterministically");

        // 3. Equal sets now -> idempotent second sync imports 0 new records!
        a.svc.initiate_sync(&b.pid_hex(), use_merkle).await.unwrap();
        exchange_all(&mut a, &mut b).await;
        assert_eq!(
            a.svc.last_sync_metrics(&b.pid()).unwrap().records_imported,
            0
        );
        assert_eq!(
            b.svc.last_sync_metrics(&a.pid()).unwrap().records_imported,
            0
        );

        a.cleanup().await;
        b.cleanup().await;
    }
}

#[test]
fn test_t9_deterministic_roots_order_independent() {
    let id_a = Identity::generate();
    let id_b = Identity::generate();
    let cid = conversation_id(&id_a.peer_id(), &id_b.peer_id());
    let recs: Vec<SyncRecord> = (1..=30u64)
        .map(|i| {
            make_record(
                &id_a,
                id_b.peer_id(),
                i * 7,
                1000 + i,
                SyncRecordKind::Message,
                "m",
            )
        })
        .collect();
    let mut rev = recs.clone();
    rev.reverse();

    let s1 = SyncSnapshot::build(cid, 0, recs).unwrap();
    let s2 = SyncSnapshot::build(cid, 0, rev).unwrap();
    assert_eq!(s1.root_hash, s2.root_hash);
    assert_eq!(s1.snapshot_id, s2.snapshot_id);
}

#[test]
fn test_t9_concurrent_mutation_snapshot_drift_reconciles() {
    let (dir, id_a, id_b, st_a, st_b) = temp_store_pair();
    let cid = conversation_id(&id_a.peer_id(), &id_b.peer_id());
    let now_unix = 50_000;

    // Seed 20 distinct items so Merkle takes multiple NodeRequests
    let recs_a: Vec<_> = (1..=20)
        .map(|i| {
            make_record(
                &id_a,
                id_b.peer_id(),
                i,
                10_000 + i,
                SyncRecordKind::Message,
                "a",
            )
        })
        .collect();
    let recs_b: Vec<_> = (100..=120)
        .map(|i| {
            make_record(
                &id_b,
                id_a.peer_id(),
                i,
                10_000 + i,
                SyncRecordKind::Message,
                "b",
            )
        })
        .collect();
    st_a.upsert_records_atomic(&cid, &recs_a, now_unix).unwrap();
    st_b.upsert_records_atomic(&cid, &recs_b, now_unix).unwrap();

    let mut mgr_a = SyncSessionManager::new(true);
    let mut mgr_b = SyncSessionManager::new(true);
    let now_inst = Instant::now();

    let mut q_ab = mgr_a
        .initiate_session(
            id_a.peer_id(),
            id_b.peer_id(),
            &st_a,
            true,
            now_inst,
            now_unix,
        )
        .unwrap();
    let mut q_ba = Vec::new();
    let mut injected = false;

    for step in 0..200 {
        if q_ab.is_empty() && q_ba.is_empty() {
            break;
        }
        // Mutate Bob's SQLite store right after initial RootOffer!
        if step == 3 && !injected {
            injected = true;
            let late_rec = make_record(
                &id_b,
                id_a.peer_id(),
                9999,
                10_999,
                SyncRecordKind::Message,
                "late",
            );
            st_b.upsert_records_atomic(&cid, &[late_rec], now_unix)
                .unwrap();
        }
        for c in std::mem::take(&mut q_ab) {
            q_ba.extend(
                mgr_b
                    .handle_inbound_chunk(
                        id_b.peer_id(),
                        id_a.peer_id(),
                        &c,
                        Some(&st_b),
                        now_inst,
                        now_unix,
                    )
                    .outbound_chunks,
            );
        }
        for c in std::mem::take(&mut q_ba) {
            q_ab.extend(
                mgr_a
                    .handle_inbound_chunk(
                        id_a.peer_id(),
                        id_b.peer_id(),
                        &c,
                        Some(&st_a),
                        now_inst,
                        now_unix,
                    )
                    .outbound_chunks,
            );
        }
    }

    assert!(injected);
    assert!(
        mgr_b
            .metrics_for(&id_a.peer_id())
            .unwrap()
            .snapshot_restarts
            >= 1,
        "Concurrent mutation must trigger bounded snapshot restart"
    );
    let final_a = st_a.list_records(&cid, now_unix).unwrap();
    let final_b = st_b.list_records(&cid, now_unix).unwrap();
    assert_eq!(final_a.len(), 42);
    assert_eq!(final_a, final_b);

    drop(st_a);
    drop(st_b);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_t9_malformed_ranges_bounds_and_quota_attack() {
    let (dir, id_a, id_b, st_a, st_b) = temp_store_pair();
    let cid = conversation_id(&id_a.peer_id(), &id_b.peer_id());
    let now_inst = Instant::now();
    let now_unix = 50_000;

    // Wire codec rejects invalid NodeRequest ranges/depth
    let bad_req = SyncFrame::NodeRequest {
        conv_id: cid,
        snapshot_id: [1u8; 16],
        start: 100,
        end: 50,
        depth: 0,
    }
    .encode()
    .unwrap();
    assert!(SyncFrame::decode(&bad_req).is_err());

    let deep_req = SyncFrame::NodeRequest {
        conv_id: cid,
        snapshot_id: [1u8; 16],
        start: 0,
        end: 65536,
        depth: 99,
    }
    .encode()
    .unwrap();
    assert!(SyncFrame::decode(&deep_req).is_err());

    // Node query budget enforcement (> MAX_MERKLE_NODES_PER_SESSION = 256)
    let mut mgr_b = SyncSessionManager::new(true);
    let hello = encode_sync_chunks(
        1,
        &SyncFrame::Hello {
            conv_id: cid,
            use_merkle: true,
        },
    )
    .unwrap();
    let _ = mgr_b.handle_inbound_chunk(
        id_b.peer_id(),
        id_a.peer_id(),
        &hello[0],
        Some(&st_b),
        now_inst,
        now_unix,
    );
    let mut seq = 2u16;
    for _ in 0..=(MAX_MERKLE_NODES_PER_SESSION + 5) {
        seq = seq.wrapping_add(1);
        let nr = encode_sync_chunks(
            seq,
            &SyncFrame::NodeRequest {
                conv_id: cid,
                snapshot_id: st_b
                    .snapshot_for_conversation(&cid, now_unix)
                    .unwrap()
                    .snapshot_id,
                start: 0,
                end: 65536,
                depth: 0,
            },
        )
        .unwrap();
        let out = mgr_b.handle_inbound_chunk(
            id_b.peer_id(),
            id_a.peer_id(),
            &nr[0],
            Some(&st_b),
            now_inst,
            now_unix,
        );
        if mgr_b
            .metrics_for(&id_a.peer_id())
            .and_then(|m| m.aborted_reason.clone())
            .is_some()
        {
            let _ = out;
            break;
        }
    }
    assert_eq!(
        mgr_b
            .metrics_for(&id_a.peer_id())
            .unwrap()
            .aborted_reason
            .as_deref(),
        Some("MERKLE_NODE_BUDGET_EXCEEDED")
    );

    // Byte quota > 256 KiB attack aborts cleanly
    let mut mgr_q = SyncSessionManager::new(true);
    let _ = mgr_q.handle_inbound_chunk(
        id_b.peer_id(),
        id_a.peer_id(),
        &hello[0],
        Some(&st_a),
        now_inst,
        now_unix,
    );
    let dummy_chunks = encode_sync_chunks(
        9,
        &SyncFrame::InventoryOffer {
            conv_id: cid,
            snapshot_id: [0u8; 16],
            horizon_unix: 0,
            offset: 0,
            total: 100,
            done: false,
            items: vec![([1u8; 16], SyncRecordKind::Message); 64],
        },
    )
    .unwrap();
    for _ in 0..((MAX_SYNC_SESSION_BYTES / 256) + 20) {
        for c in &dummy_chunks {
            let _ = mgr_q.handle_inbound_chunk(
                id_b.peer_id(),
                id_a.peer_id(),
                c,
                Some(&st_a),
                now_inst,
                now_unix,
            );
        }
        if mgr_q
            .metrics_for(&id_a.peer_id())
            .and_then(|m| m.aborted_reason.as_deref())
            == Some("SESSION_BYTE_QUOTA_EXCEEDED")
        {
            break;
        }
    }
    assert_eq!(
        mgr_q
            .metrics_for(&id_a.peer_id())
            .unwrap()
            .aborted_reason
            .as_deref(),
        Some("SESSION_BYTE_QUOTA_EXCEEDED")
    );

    drop(st_a);
    drop(st_b);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_t9_forged_signature_wrong_conv_and_expired_rejected() {
    let (dir, id_a, id_b, st_a, st_b) = temp_store_pair();
    let id_carol = Identity::generate();
    let cid_ab = conversation_id(&id_a.peer_id(), &id_b.peer_id());
    let now_unix = 50_000;

    // 1. Forged signature record
    let mut forged = make_record(
        &id_b,
        id_a.peer_id(),
        1,
        10_000,
        SyncRecordKind::Message,
        "forged",
    );
    forged.signature[0] ^= 0xFF;

    // 2. Wrong conversation record (Carol <-> Alice presented to Bob <-> Alice session!)
    let foreign = make_record(
        &id_carol,
        id_a.peer_id(),
        2,
        10_001,
        SyncRecordKind::Message,
        "foreign",
    );

    // 3. Expired record (`expires_at <= now_unix`)
    let expired = SyncRecord::create_signed(
        &id_b,
        id_a.peer_id(),
        [0x55u8; 16],
        10_000,
        10_500, // < now_unix (50_000)!
        SyncRecordKind::Message,
        b"expired".to_vec(),
    )
    .unwrap();

    // 4. One genuinely valid record for comparison
    let good = make_record(
        &id_b,
        id_a.peer_id(),
        4,
        49_000,
        SyncRecordKind::Message,
        "valid",
    );
    let orig_exp = good.expires_at_unix;

    let mut mgr_a = SyncSessionManager::new(true);
    let now_inst = Instant::now();
    let hello = encode_sync_chunks(
        1,
        &SyncFrame::Hello {
            conv_id: cid_ab,
            use_merkle: true,
        },
    )
    .unwrap();
    let _ = mgr_a.handle_inbound_chunk(
        id_a.peer_id(),
        id_b.peer_id(),
        &hello[0],
        Some(&st_a),
        now_inst,
        now_unix,
    );

    let attack_batch = encode_sync_chunks(
        2,
        &SyncFrame::RecordBatch {
            conv_id: cid_ab,
            snapshot_id: [0x11u8; 16],
            records: vec![forged, foreign, expired, good],
            done: true,
        },
    )
    .unwrap();
    for c in &attack_batch {
        let _ = mgr_a.handle_inbound_chunk(
            id_a.peer_id(),
            id_b.peer_id(),
            c,
            Some(&st_a),
            now_inst,
            now_unix,
        );
    }

    let ma = mgr_a.metrics_for(&id_b.peer_id()).unwrap();
    assert_eq!(
        ma.records_rejected, 3,
        "Forged, foreign, and expired must be rejected"
    );
    assert_eq!(ma.records_imported, 1);

    let stored = st_a.list_records(&cid_ab, now_unix).unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].body, b"valid");
    assert_eq!(
        stored[0].expires_at_unix, orig_exp,
        "TTL must NEVER be extended"
    );

    drop(st_b);
    drop(st_a);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_t9_tombstone_dominance_horizon_non_resurrection_and_mode_exclusions() {
    let mut a = SyncNodeBox::new("tomb_a", 0x51).await;
    let mut b = SyncNodeBox::new("tomb_b", 0x52).await;
    connect_pair(&mut a, &mut b).await;
    a.svc.set_sync_enabled(true).unwrap();
    b.svc.set_sync_enabled(true).unwrap();

    // Normal message stored & synced to Bob
    let lid = a
        .svc
        .send_message(b.pid_hex(), "will-be-deleted".into(), 0)
        .await
        .unwrap();
    exchange_all(&mut a, &mut b).await;
    a.svc.initiate_sync(&b.pid_hex(), true).await.unwrap();
    exchange_all(&mut a, &mut b).await;
    assert_eq!(
        b.svc.list_sync_records(&a.pid_hex()).await.unwrap().len(),
        1
    );

    // Delete on Alice -> Tombstone synced to Bob -> wipes plaintext body cleanly
    a.svc.delete_sync_record(&b.pid_hex(), &lid).await.unwrap();
    a.svc.initiate_sync(&b.pid_hex(), true).await.unwrap();
    exchange_all(&mut a, &mut b).await;
    let b_recs = b.svc.list_sync_records(&a.pid_hex()).await.unwrap();
    assert_eq!(b_recs.len(), 1);
    assert_eq!(b_recs[0].kind, SyncRecordKind::Tombstone);
    assert!(b_recs[0].body.is_empty());
    let created_ts = b_recs[0].created_at_unix;

    // Advance tombstone eviction horizon on Alice so Tombstone is compacted away
    let horizon = a
        .svc
        .evict_sync_tombstones_before(&b.pid_hex(), created_ts)
        .await
        .unwrap();
    assert!(horizon > created_ts);

    // Attempting to reimport an older Message created at <= created_ts fails non-resurrection!
    let fake_old_msg = make_record(
        &a.id_copy,
        b.pid(),
        77,
        created_ts,
        SyncRecordKind::Message,
        "zombie",
    );
    let cid = conversation_id(&a.pid(), &b.pid());
    assert!(fake_old_msg
        .verify_for_conversation(&a.pid(), &b.pid(), horizon, created_ts + 1)
        .is_err());
    let _ = cid;

    // Privacy mode (`mode = 1`) NEVER touches sync store
    a.svc
        .send_message(b.pid_hex(), "privacy-never-syncs".into(), 1)
        .await
        .unwrap();
    exchange_all(&mut a, &mut b).await;
    assert!(a
        .svc
        .list_sync_records(&b.pid_hex())
        .await
        .unwrap()
        .is_empty());

    // Volatile negotiated mode (`BC2V`) NEVER touches sync store
    a.svc
        .set_volatile_session(b.short_hex(), 120)
        .await
        .unwrap();
    exchange_all(&mut a, &mut b).await;
    a.svc
        .send_message(b.pid_hex(), "volatile-never-syncs".into(), 0)
        .await
        .unwrap();
    exchange_all(&mut a, &mut b).await;
    assert!(a
        .svc
        .list_sync_records(&b.pid_hex())
        .await
        .unwrap()
        .is_empty());

    a.cleanup().await;
    b.cleanup().await;
}

#[tokio::test]
async fn test_t9_unsupported_restart_disconnect_and_mid_sync_block() {
    let mut a = SyncNodeBox::new("misc_a", 0x61).await;
    let mut b = SyncNodeBox::new("misc_b", 0x62).await;
    connect_pair(&mut a, &mut b).await;

    // Unsupported peer (`b` has sync OFF by default) continues normal messaging cleanly!
    a.svc.set_sync_enabled(true).unwrap();
    a.svc.initiate_sync(&b.pid_hex(), true).await.unwrap();
    exchange_all(&mut a, &mut b).await;
    assert_eq!(
        a.svc
            .last_sync_metrics(&b.pid())
            .unwrap()
            .aborted_reason
            .as_deref(),
        Some("UNSUPPORTED_BY_PEER")
    );

    let chat_id = a
        .svc
        .send_message(b.pid_hex(), "normal-still-works".into(), 0)
        .await
        .unwrap();
    exchange_all(&mut a, &mut b).await;
    assert_eq!(
        a.svc.get_delivery_state(chat_id).await.unwrap(),
        "Delivered"
    );
    assert_eq!(b.inbox.len(), 1);

    // Enable Bob, disconnect mid-way -> reconnect & restart -> idempotent sync succeeds
    b.svc.set_sync_enabled(true).unwrap();
    a.svc.initiate_sync(&b.pid_hex(), true).await.unwrap();
    a.svc.on_peer_disconnected(b.short_hex()).await.unwrap();
    b.svc.on_peer_disconnected(a.short_hex()).await.unwrap();
    assert_eq!(
        a.svc
            .last_sync_metrics(&b.pid())
            .unwrap()
            .aborted_reason
            .as_deref(),
        Some("PEER_REVOKED_OR_DISCONNECTED")
    );

    a.restart().await;
    b.restart().await;
    a.svc.set_sync_enabled(true).unwrap();
    b.svc.set_sync_enabled(true).unwrap();
    connect_pair(&mut a, &mut b).await;
    a.svc.initiate_sync(&b.pid_hex(), true).await.unwrap();
    exchange_all(&mut a, &mut b).await;
    assert_eq!(
        b.svc.list_sync_records(&a.pid_hex()).await.unwrap().len(),
        1
    );

    // Block peer mid-sync -> session aborts immediately
    b.svc
        .send_message(a.pid_hex(), "blocked-before-sync".into(), 0)
        .await
        .unwrap();
    exchange_all(&mut a, &mut b).await;
    b.svc.initiate_sync(&a.pid_hex(), true).await.unwrap();
    a.svc
        .trust_set_peer_state(b.pid_hex(), "block".into())
        .await
        .unwrap();
    exchange_all(&mut a, &mut b).await;
    // Alice STILL only has 1 record because she blocked Bob mid-sync!
    assert_eq!(
        a.svc.list_sync_records(&b.pid_hex()).await.unwrap().len(),
        1
    );

    a.cleanup().await;
    b.cleanup().await;
}

#[test]
fn test_t9_benchmark_id_inventory_vs_merkle_matrix() {
    let now_unix = 50_000u64;
    let sizes = [0usize, 8, 32, 128, 256, 512];
    let overlaps = [100usize, 95, 50, 0];

    println!("BENCHMARK_CSV_HEADER:set_size,overlap_pct,inv_bytes,inv_rounds,merkle_bytes,merkle_rounds,merkle_nodes");
    for &n in &sizes {
        for &ov in &overlaps {
            if n == 0 && ov != 100 {
                continue;
            }
            let (dir, id_a, id_b, st_a, st_b) = temp_store_pair();
            let cid = conversation_id(&id_a.peer_id(), &id_b.peer_id());
            let shared_count = (n * ov) / 100;
            let diff_count = n - shared_count;

            let mut shared = Vec::with_capacity(shared_count);
            for i in 0..shared_count {
                shared.push(make_record(
                    &id_a,
                    id_b.peer_id(),
                    1_000 + i as u64,
                    10_000 + i as u64,
                    SyncRecordKind::Message,
                    "shared-payload-0123456789abcdef",
                ));
            }
            let mut only_a = Vec::with_capacity(diff_count);
            let mut only_b = Vec::with_capacity(diff_count);
            for i in 0..diff_count {
                only_a.push(make_record(
                    &id_a,
                    id_b.peer_id(),
                    100_000 + i as u64,
                    20_000 + i as u64,
                    SyncRecordKind::Message,
                    "only-a-payload-0123456789abcdef",
                ));
                only_b.push(make_record(
                    &id_b,
                    id_a.peer_id(),
                    200_000 + i as u64,
                    30_000 + i as u64,
                    SyncRecordKind::Message,
                    "only-b-payload-0123456789abcdef",
                ));
            }

            // 512 records on A + 512 disjoint records on B (~232+ KiB per peer + framing) exceeds
            // MAX_SYNC_SESSION_BYTES (256 KiB): verify clean fail-closed quota enforcement on fresh DBs.
            if n == 512 && ov == 0 {
                drop(st_a);
                drop(st_b);
                let _ = std::fs::remove_dir_all(&dir);

                for use_merkle in [false, true] {
                    let (qdir, qid_a, qid_b, qst_a, qst_b) = temp_store_pair();
                    let qcid = conversation_id(&qid_a.peer_id(), &qid_b.peer_id());
                    let q_only_a: Vec<SyncRecord> = (0..diff_count)
                        .map(|i| {
                            make_record(
                                &qid_a,
                                qid_b.peer_id(),
                                100_000 + i as u64,
                                20_000 + i as u64,
                                SyncRecordKind::Message,
                                "only-a-payload-0123456789abcdef",
                            )
                        })
                        .collect();
                    let q_only_b: Vec<SyncRecord> = (0..diff_count)
                        .map(|i| {
                            make_record(
                                &qid_b,
                                qid_a.peer_id(),
                                200_000 + i as u64,
                                30_000 + i as u64,
                                SyncRecordKind::Message,
                                "only-b-payload-0123456789abcdef",
                            )
                        })
                        .collect();
                    qst_a
                        .upsert_records_atomic(&qcid, &q_only_a, now_unix)
                        .unwrap();
                    qst_b
                        .upsert_records_atomic(&qcid, &q_only_b, now_unix)
                        .unwrap();

                    let mut mgr_a = SyncSessionManager::new(true);
                    let mut mgr_b = SyncSessionManager::new(true);
                    let now_inst = Instant::now();
                    let mut q_ab = mgr_a
                        .initiate_session(
                            qid_a.peer_id(),
                            qid_b.peer_id(),
                            &qst_a,
                            use_merkle,
                            now_inst,
                            now_unix,
                        )
                        .unwrap();
                    let mut q_ba = Vec::new();
                    for _ in 0..500 {
                        if q_ab.is_empty() && q_ba.is_empty() {
                            break;
                        }
                        for c in std::mem::take(&mut q_ab) {
                            let o = mgr_b.handle_inbound_chunk(
                                qid_b.peer_id(),
                                qid_a.peer_id(),
                                &c,
                                Some(&qst_b),
                                now_inst,
                                now_unix,
                            );
                            q_ba.extend(o.outbound_chunks);
                        }
                        for c in std::mem::take(&mut q_ba) {
                            let o = mgr_a.handle_inbound_chunk(
                                qid_a.peer_id(),
                                qid_b.peer_id(),
                                &c,
                                Some(&qst_a),
                                now_inst,
                                now_unix,
                            );
                            q_ab.extend(o.outbound_chunks);
                        }
                    }
                    let ma = mgr_a.metrics_for(&qid_b.peer_id()).unwrap();
                    let mb = mgr_b.metrics_for(&qid_a.peer_id()).unwrap();
                    assert!(
                        ma.aborted_reason.as_deref() == Some("SESSION_BYTE_QUOTA_EXCEEDED")
                            || mb.aborted_reason.as_deref() == Some("SESSION_BYTE_QUOTA_EXCEEDED"),
                        "Expected SESSION_BYTE_QUOTA_EXCEEDED for 512@0% (use_merkle={use_merkle}), got a={ma:?} b={mb:?}"
                    );
                    assert!(
                        ma.wire_bytes_sent + ma.wire_bytes_recv <= MAX_SYNC_SESSION_BYTES + 2048
                    );

                    drop(qst_a);
                    drop(qst_b);
                    let _ = std::fs::remove_dir_all(&qdir);
                }
                continue;
            }

            // Run 1: ID Inventory baseline
            {
                let mut a_set = shared.clone();
                a_set.extend(only_a.clone());
                let mut b_set = shared.clone();
                b_set.extend(only_b.clone());
                st_a.upsert_records_atomic(&cid, &a_set, now_unix).unwrap();
                st_b.upsert_records_atomic(&cid, &b_set, now_unix).unwrap();
            }
            let (inv_bytes, inv_rounds, _) =
                run_sync_session_pair(&id_a, &st_a, &id_b, &st_b, false, now_unix);
            let conv_inv_a: BTreeSet<_> = st_a
                .list_records(&cid, now_unix)
                .unwrap()
                .into_iter()
                .map(|r| r.logical_id)
                .collect();
            let conv_inv_b: BTreeSet<_> = st_b
                .list_records(&cid, now_unix)
                .unwrap()
                .into_iter()
                .map(|r| r.logical_id)
                .collect();
            assert_eq!(conv_inv_a.len(), n + diff_count);
            assert_eq!(conv_inv_a, conv_inv_b);

            drop(st_a);
            drop(st_b);
            let _ = std::fs::remove_dir_all(&dir);

            // Run 2: Bounded Merkle (fresh matching databases)
            let (dir2, id_a2, id_b2, st_a2, st_b2) = temp_store_pair();
            let cid2 = conversation_id(&id_a2.peer_id(), &id_b2.peer_id());
            let mut shared2 = Vec::with_capacity(shared_count);
            for i in 0..shared_count {
                shared2.push(make_record(
                    &id_a2,
                    id_b2.peer_id(),
                    1_000 + i as u64,
                    10_000 + i as u64,
                    SyncRecordKind::Message,
                    "shared-payload-0123456789abcdef",
                ));
            }
            let mut only_a2 = Vec::with_capacity(diff_count);
            let mut only_b2 = Vec::with_capacity(diff_count);
            for i in 0..diff_count {
                only_a2.push(make_record(
                    &id_a2,
                    id_b2.peer_id(),
                    100_000 + i as u64,
                    20_000 + i as u64,
                    SyncRecordKind::Message,
                    "only-a-payload-0123456789abcdef",
                ));
                only_b2.push(make_record(
                    &id_b2,
                    id_a2.peer_id(),
                    200_000 + i as u64,
                    30_000 + i as u64,
                    SyncRecordKind::Message,
                    "only-b-payload-0123456789abcdef",
                ));
            }
            let mut a_set2 = shared2.clone();
            a_set2.extend(only_a2);
            let mut b_set2 = shared2;
            b_set2.extend(only_b2);
            st_a2
                .upsert_records_atomic(&cid2, &a_set2, now_unix)
                .unwrap();
            st_b2
                .upsert_records_atomic(&cid2, &b_set2, now_unix)
                .unwrap();

            let (merkle_bytes, merkle_rounds, merkle_nodes) =
                run_sync_session_pair(&id_a2, &st_a2, &id_b2, &st_b2, true, now_unix);
            let conv_mk_a: BTreeSet<_> = st_a2
                .list_records(&cid2, now_unix)
                .unwrap()
                .into_iter()
                .map(|r| r.logical_id)
                .collect();
            let conv_mk_b: BTreeSet<_> = st_b2
                .list_records(&cid2, now_unix)
                .unwrap()
                .into_iter()
                .map(|r| r.logical_id)
                .collect();
            assert_eq!(conv_mk_a.len(), n + diff_count);
            assert_eq!(conv_mk_a, conv_mk_b);

            println!(
                "BENCHMARK_CSV:{n},{ov},{inv_bytes},{inv_rounds},{merkle_bytes},{merkle_rounds},{merkle_nodes}"
            );

            drop(st_a2);
            drop(st_b2);
            let _ = std::fs::remove_dir_all(&dir2);
        }
    }
}
