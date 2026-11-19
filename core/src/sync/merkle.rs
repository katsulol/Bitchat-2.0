// CRYPTO-REVIEW:
// What it does: Deterministic radix-interval Merkle tree over canonical `SyncRecord` snapshots
// using pinned `blake3` with explicit domain separation.
// Primitives / crates: `blake3`.
// Reviewer checklist:
// 1. Leaf, empty-interval, and internal-node hashes carry distinct domain prefixes.
// 2. Partitioning uses fixed `u16` key-space intervals `[start, end)` (`0..=65536`) over `logical_id[0..2]`
//    with deterministic sort order `(radix_key, created_at_unix, logical_id, kind)` so index
//    shifts on one side do not cascade across unrelated branches and never depend on `HashMap` iteration.
// 3. Depth strictly bounded (`<= MAX_MERKLE_DEPTH = 12`).

use crate::error::{Error, Result};
use crate::sync::record::{SyncRecord, SyncRecordKind, MAX_SYNC_RECORDS_PER_CONVERSATION};

pub const MAX_MERKLE_DEPTH: u8 = 12;
pub const KEYSPACE_END: u32 = 65_536;
pub const LEAF_BUCKET_THRESHOLD: usize = 8;

const EMPTY_DOMAIN: &[u8] = b"bitchat2/merkle/empty/v1";
const LEAF_DOMAIN: &[u8] = b"bitchat2/merkle/leaf-range/v1";
const NODE_DOMAIN: &[u8] = b"bitchat2/merkle/node/v1";
const SNAP_DOMAIN: &[u8] = b"bitchat2/merkle/snap/v1";

pub fn empty_range_hash(conv_id: &[u8; 32], start: u32, end: u32) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(EMPTY_DOMAIN);
    h.update(conv_id);
    h.update(&start.to_be_bytes());
    h.update(&end.to_be_bytes());
    *h.finalize().as_bytes()
}

pub fn leaf_range_hash(
    conv_id: &[u8; 32],
    start: u32,
    end: u32,
    items: &[&SyncRecord],
) -> [u8; 32] {
    if items.is_empty() {
        return empty_range_hash(conv_id, start, end);
    }
    let mut h = blake3::Hasher::new();
    h.update(LEAF_DOMAIN);
    h.update(conv_id);
    h.update(&start.to_be_bytes());
    h.update(&end.to_be_bytes());
    h.update(&(items.len() as u32).to_be_bytes());
    for rec in items {
        h.update(&rec.created_at_unix.to_be_bytes());
        h.update(&rec.logical_id);
        h.update(&[rec.kind as u8]);
        h.update(&rec.digest());
    }
    *h.finalize().as_bytes()
}

pub fn internal_node_hash(
    conv_id: &[u8; 32],
    depth: u8,
    left: &[u8; 32],
    right: &[u8; 32],
) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(NODE_DOMAIN);
    h.update(conv_id);
    h.update(&[depth]);
    h.update(left);
    h.update(right);
    *h.finalize().as_bytes()
}

/// Inspection output of one key-space interval `[start, end)` in a `SyncSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeInspection {
    Split {
        mid: u32,
        left_hash: [u8; 32],
        right_hash: [u8; 32],
    },
    LeafItems(Vec<([u8; 16], SyncRecordKind)>),
}

/// Immutable, canonically ordered conversation snapshot with stable version ID and Merkle root.
#[derive(Debug, Clone)]
pub struct SyncSnapshot {
    pub conv_id: [u8; 32],
    pub snapshot_id: [u8; 16],
    pub horizon_unix: u64,
    pub records: Vec<SyncRecord>,
    pub root_hash: [u8; 32],
}

impl SyncSnapshot {
    pub fn build(conv_id: [u8; 32], horizon_unix: u64, mut raw: Vec<SyncRecord>) -> Result<Self> {
        raw.sort_by_key(SyncRecord::sort_key);

        // Deduplicate by logical_id with Tombstone dominance preserving canonical sort order
        let mut deduped: Vec<SyncRecord> = Vec::with_capacity(raw.len());
        let mut by_id = std::collections::HashMap::<[u8; 16], usize>::new();
        for rec in raw {
            if let Some(&idx) = by_id.get(&rec.logical_id) {
                if rec.kind == SyncRecordKind::Tombstone
                    && deduped[idx].kind != SyncRecordKind::Tombstone
                {
                    deduped[idx] = rec;
                }
            } else {
                by_id.insert(rec.logical_id, deduped.len());
                deduped.push(rec);
            }
        }
        deduped.sort_by_key(SyncRecord::sort_key);
        if deduped.len() > MAX_SYNC_RECORDS_PER_CONVERSATION {
            let overflow = deduped.len() - MAX_SYNC_RECORDS_PER_CONVERSATION;
            deduped.drain(0..overflow);
        }

        let mut snap = Self {
            conv_id,
            snapshot_id: [0u8; 16],
            horizon_unix,
            records: deduped,
            root_hash: [0u8; 32],
        };
        snap.root_hash = snap.range_hash(0, KEYSPACE_END, 0)?;

        let mut id_hasher = blake3::Hasher::new();
        id_hasher.update(SNAP_DOMAIN);
        id_hasher.update(&conv_id);
        id_hasher.update(&horizon_unix.to_be_bytes());
        id_hasher.update(&(snap.records.len() as u32).to_be_bytes());
        id_hasher.update(&snap.root_hash);
        snap.snapshot_id
            .copy_from_slice(&id_hasher.finalize().as_bytes()[0..16]);

        Ok(snap)
    }

    pub fn records_in_range(&self, start: u32, end: u32) -> Vec<&SyncRecord> {
        self.records
            .iter()
            .filter(|r| {
                let k = r.radix_key() as u32;
                k >= start && k < end
            })
            .collect()
    }

    pub fn range_hash(&self, start: u32, end: u32, depth: u8) -> Result<[u8; 32]> {
        if start > end || end > KEYSPACE_END {
            return Err(Error::Crypto("Invalid Merkle range bounds".into()));
        }
        if depth > MAX_MERKLE_DEPTH {
            return Err(Error::Crypto("Merkle depth bound exceeded".into()));
        }
        let items = self.records_in_range(start, end);
        if items.is_empty() {
            return Ok(empty_range_hash(&self.conv_id, start, end));
        }
        if items.len() <= LEAF_BUCKET_THRESHOLD || end - start <= 1 || depth == MAX_MERKLE_DEPTH {
            return Ok(leaf_range_hash(&self.conv_id, start, end, &items));
        }
        let mid = start + (end - start) / 2;
        let left = self.range_hash(start, mid, depth + 1)?;
        let right = self.range_hash(mid, end, depth + 1)?;
        Ok(internal_node_hash(&self.conv_id, depth, &left, &right))
    }

    pub fn inspect_range(&self, start: u32, end: u32, depth: u8) -> Result<RangeInspection> {
        if start > end || end > KEYSPACE_END {
            return Err(Error::Crypto("Invalid Merkle inspect range".into()));
        }
        if depth > MAX_MERKLE_DEPTH {
            return Err(Error::Crypto("Merkle inspect depth exceeded".into()));
        }
        let items = self.records_in_range(start, end);
        if items.len() <= LEAF_BUCKET_THRESHOLD || end - start <= 1 || depth == MAX_MERKLE_DEPTH {
            let entries = items.into_iter().map(|r| (r.logical_id, r.kind)).collect();
            return Ok(RangeInspection::LeafItems(entries));
        }
        let mid = start + (end - start) / 2;
        let left_hash = self.range_hash(start, mid, depth + 1)?;
        let right_hash = self.range_hash(mid, end, depth + 1)?;
        Ok(RangeInspection::Split {
            mid,
            left_hash,
            right_hash,
        })
    }
}
