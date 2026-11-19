// CRYPTO-REVIEW:
// What it does: Bounded Normal-mode conversation reconciliation protocol (`BC2S` v1)
// supporting both `IdInventory` baseline and bounded `blake3` Merkle reconciliation inside
// mutually authenticated E2E Noise `ApplicationFrame` sessions.
// Primitives / crates: `blake3`, `ed25519-dalek` (via `SyncRecord::verify_for_conversation`).
// Reviewer checklist:
// 1. Default OFF (`SyncSessionManager::default().is_enabled() == false`).
// 2. Runs exclusively over Noise XX `ApplicationFrame`s on approved peer conversations;
//    gateway nodes never see plaintext records or sync inventory.
// 3. Rejects mismatched `conv_id`, malformed ranges/depths, duplicate IDs, >256 KiB byte budgets,
//    >256 Merkle node queries, >30s deadlines, and >2 concurrent snapshot restarts.
// 4. Every imported `SyncRecord` undergoes full Ed25519 author verification, expiry check,
//    tombstone dominance check, and `horizon_unix` non-resurrection check before SQLite commit.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::identity::PeerId;
use crate::sync::merkle::{
    empty_range_hash, RangeInspection, SyncSnapshot, KEYSPACE_END, MAX_MERKLE_DEPTH,
};
use crate::sync::record::{
    conversation_id, SyncRecord, SyncRecordKind, MAX_SYNC_RECORDS_PER_CONVERSATION,
};
use crate::sync::store::SqliteSyncStore;

pub const SYNC_MAGIC: &[u8; 4] = b"BC2S";
pub const SYNC_WIRE_VERSION: u8 = 1;
pub const SYNC_CHUNK_HEADER_LEN: usize = 9; // 4 magic + 1 ver + 2 msg_seq + 1 idx + 1 total
pub const MAX_SYNC_CHUNK_PAYLOAD: usize = 136;
pub const MAX_SYNC_CHUNKS_PER_FRAME: u8 = 80;
pub const MAX_SYNC_RAW_FRAME_BYTES: usize = 10_240;
pub const MAX_SYNC_SESSION_BYTES: usize = 256 * 1024;
pub const MAX_MERKLE_NODES_PER_SESSION: usize = 256;
pub const SYNC_SESSION_DEADLINE: Duration = Duration::from_secs(30);
pub const MAX_SNAPSHOT_RESTARTS: u8 = 2;
pub const MAX_SYNC_RECORDS_PER_FRAME: usize = 8;
pub const MAX_BATCH_RAW_BYTES: usize = 8192;
pub const MAX_INVENTORY_ITEMS_PER_FRAME: usize = 64;
const MAX_ABORT_REASON_LEN: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum FrameTag {
    Hello = 1,
    HelloAck = 2,
    RootOffer = 3,
    NodeRequest = 4,
    NodeResponse = 5,
    InventoryOffer = 6,
    RecordRequest = 7,
    RecordBatch = 8,
    SnapshotMismatch = 9,
    Done = 10,
    Abort = 11,
}

impl FrameTag {
    fn from_u8(val: u8) -> Result<Self> {
        match val {
            1 => Ok(Self::Hello),
            2 => Ok(Self::HelloAck),
            3 => Ok(Self::RootOffer),
            4 => Ok(Self::NodeRequest),
            5 => Ok(Self::NodeResponse),
            6 => Ok(Self::InventoryOffer),
            7 => Ok(Self::RecordRequest),
            8 => Ok(Self::RecordBatch),
            9 => Ok(Self::SnapshotMismatch),
            10 => Ok(Self::Done),
            11 => Ok(Self::Abort),
            other => Err(Error::Crypto(format!("Invalid SyncFrame tag: {other}"))),
        }
    }
}

/// Bounded wire frame variants for `BC2S` reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncFrame {
    Hello {
        conv_id: [u8; 32],
        use_merkle: bool,
    },
    HelloAck {
        conv_id: [u8; 32],
        supported: bool,
        use_merkle: bool,
    },
    RootOffer {
        conv_id: [u8; 32],
        snapshot_id: [u8; 16],
        count: u16,
        horizon_unix: u64,
        root_hash: [u8; 32],
    },
    NodeRequest {
        conv_id: [u8; 32],
        snapshot_id: [u8; 16],
        start: u32,
        end: u32,
        depth: u8,
    },
    NodeResponse {
        conv_id: [u8; 32],
        snapshot_id: [u8; 16],
        start: u32,
        mid: u32,
        end: u32,
        is_leaf: bool,
        left_hash: [u8; 32],
        right_hash: [u8; 32],
        leaf_items: Vec<([u8; 16], SyncRecordKind)>,
    },
    InventoryOffer {
        conv_id: [u8; 32],
        snapshot_id: [u8; 16],
        horizon_unix: u64,
        offset: u16,
        total: u16,
        done: bool,
        items: Vec<([u8; 16], SyncRecordKind)>,
    },
    RecordRequest {
        conv_id: [u8; 32],
        snapshot_id: [u8; 16],
        done: bool,
        ids: Vec<[u8; 16]>,
    },
    RecordBatch {
        conv_id: [u8; 32],
        snapshot_id: [u8; 16],
        records: Vec<SyncRecord>,
        done: bool,
    },
    SnapshotMismatch {
        conv_id: [u8; 32],
    },
    Done {
        conv_id: [u8; 32],
    },
    Abort {
        conv_id: [u8; 32],
        reason: String,
    },
}

impl SyncFrame {
    pub fn conv_id(&self) -> &[u8; 32] {
        match self {
            Self::Hello { conv_id, .. }
            | Self::HelloAck { conv_id, .. }
            | Self::RootOffer { conv_id, .. }
            | Self::NodeRequest { conv_id, .. }
            | Self::NodeResponse { conv_id, .. }
            | Self::InventoryOffer { conv_id, .. }
            | Self::RecordRequest { conv_id, .. }
            | Self::RecordBatch { conv_id, .. }
            | Self::SnapshotMismatch { conv_id }
            | Self::Done { conv_id }
            | Self::Abort { conv_id, .. } => conv_id,
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(128);
        match self {
            Self::Hello {
                conv_id,
                use_merkle,
            } => {
                out.push(FrameTag::Hello as u8);
                out.extend_from_slice(conv_id);
                out.push(u8::from(*use_merkle));
            }
            Self::HelloAck {
                conv_id,
                supported,
                use_merkle,
            } => {
                out.push(FrameTag::HelloAck as u8);
                out.extend_from_slice(conv_id);
                out.push(u8::from(*supported));
                out.push(u8::from(*use_merkle));
            }
            Self::RootOffer {
                conv_id,
                snapshot_id,
                count,
                horizon_unix,
                root_hash,
            } => {
                out.push(FrameTag::RootOffer as u8);
                out.extend_from_slice(conv_id);
                out.extend_from_slice(snapshot_id);
                out.extend_from_slice(&count.to_be_bytes());
                out.extend_from_slice(&horizon_unix.to_be_bytes());
                out.extend_from_slice(root_hash);
            }
            Self::NodeRequest {
                conv_id,
                snapshot_id,
                start,
                end,
                depth,
            } => {
                out.push(FrameTag::NodeRequest as u8);
                out.extend_from_slice(conv_id);
                out.extend_from_slice(snapshot_id);
                out.extend_from_slice(&start.to_be_bytes());
                out.extend_from_slice(&end.to_be_bytes());
                out.push(*depth);
            }
            Self::NodeResponse {
                conv_id,
                snapshot_id,
                start,
                mid,
                end,
                is_leaf,
                left_hash,
                right_hash,
                leaf_items,
            } => {
                if leaf_items.len() > MAX_INVENTORY_ITEMS_PER_FRAME {
                    return Err(Error::Crypto(
                        "NodeResponse leaf_items bound exceeded".into(),
                    ));
                }
                out.push(FrameTag::NodeResponse as u8);
                out.extend_from_slice(conv_id);
                out.extend_from_slice(snapshot_id);
                out.extend_from_slice(&start.to_be_bytes());
                out.extend_from_slice(&mid.to_be_bytes());
                out.extend_from_slice(&end.to_be_bytes());
                out.push(u8::from(*is_leaf));
                out.extend_from_slice(left_hash);
                out.extend_from_slice(right_hash);
                out.extend_from_slice(&(leaf_items.len() as u16).to_be_bytes());
                for (id, kind) in leaf_items {
                    out.extend_from_slice(id);
                    out.push(*kind as u8);
                }
            }
            Self::InventoryOffer {
                conv_id,
                snapshot_id,
                horizon_unix,
                offset,
                total,
                done,
                items,
            } => {
                if items.len() > MAX_INVENTORY_ITEMS_PER_FRAME {
                    return Err(Error::Crypto(
                        "InventoryOffer item batch bound exceeded".into(),
                    ));
                }
                out.push(FrameTag::InventoryOffer as u8);
                out.extend_from_slice(conv_id);
                out.extend_from_slice(snapshot_id);
                out.extend_from_slice(&horizon_unix.to_be_bytes());
                out.extend_from_slice(&offset.to_be_bytes());
                out.extend_from_slice(&total.to_be_bytes());
                out.push(u8::from(*done));
                out.extend_from_slice(&(items.len() as u16).to_be_bytes());
                for (id, kind) in items {
                    out.extend_from_slice(id);
                    out.push(*kind as u8);
                }
            }
            Self::RecordRequest {
                conv_id,
                snapshot_id,
                done,
                ids,
            } => {
                if ids.len() > MAX_SYNC_RECORDS_PER_FRAME {
                    return Err(Error::Crypto("RecordRequest ID count exceeded".into()));
                }
                out.push(FrameTag::RecordRequest as u8);
                out.extend_from_slice(conv_id);
                out.extend_from_slice(snapshot_id);
                out.push(u8::from(*done));
                out.push(ids.len() as u8);
                for id in ids {
                    out.extend_from_slice(id);
                }
            }
            Self::RecordBatch {
                conv_id,
                snapshot_id,
                records,
                done,
            } => {
                if records.len() > MAX_SYNC_RECORDS_PER_FRAME {
                    return Err(Error::Crypto("RecordBatch record count exceeded".into()));
                }
                out.push(FrameTag::RecordBatch as u8);
                out.extend_from_slice(conv_id);
                out.extend_from_slice(snapshot_id);
                out.push(u8::from(*done));
                out.push(records.len() as u8);
                for rec in records {
                    let wire = rec.encode_wire()?;
                    out.extend_from_slice(&(wire.len() as u16).to_be_bytes());
                    out.extend_from_slice(&wire);
                }
            }
            Self::SnapshotMismatch { conv_id } => {
                out.push(FrameTag::SnapshotMismatch as u8);
                out.extend_from_slice(conv_id);
            }
            Self::Done { conv_id } => {
                out.push(FrameTag::Done as u8);
                out.extend_from_slice(conv_id);
            }
            Self::Abort { conv_id, reason } => {
                out.push(FrameTag::Abort as u8);
                out.extend_from_slice(conv_id);
                let mut r_bytes = reason.as_bytes();
                if r_bytes.len() > MAX_ABORT_REASON_LEN {
                    r_bytes = &r_bytes[..MAX_ABORT_REASON_LEN];
                }
                out.push(r_bytes.len() as u8);
                out.extend_from_slice(r_bytes);
            }
        }
        if out.len() > MAX_SYNC_RAW_FRAME_BYTES {
            return Err(Error::Crypto(format!(
                "Encoded SyncFrame {} exceeds max {}",
                out.len(),
                MAX_SYNC_RAW_FRAME_BYTES
            )));
        }
        Ok(out)
    }

    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < 33 || buf.len() > MAX_SYNC_RAW_FRAME_BYTES {
            return Err(Error::Crypto("Invalid SyncFrame buffer bounds".into()));
        }
        let tag = FrameTag::from_u8(buf[0])?;
        let conv_id = read_32(buf, 1)?;
        let rest = &buf[33..];

        match tag {
            FrameTag::Hello => {
                if rest.len() != 1 {
                    return Err(Error::Crypto("Malformed Hello SyncFrame".into()));
                }
                Ok(Self::Hello {
                    conv_id,
                    use_merkle: rest[0] != 0,
                })
            }
            FrameTag::HelloAck => {
                if rest.len() != 2 {
                    return Err(Error::Crypto("Malformed HelloAck SyncFrame".into()));
                }
                Ok(Self::HelloAck {
                    conv_id,
                    supported: rest[0] != 0,
                    use_merkle: rest[1] != 0,
                })
            }
            FrameTag::RootOffer => {
                if rest.len() != 16 + 2 + 8 + 32 {
                    return Err(Error::Crypto("Malformed RootOffer SyncFrame".into()));
                }
                let snapshot_id = read_16(rest, 0)?;
                let count = u16::from_be_bytes([rest[16], rest[17]]);
                let horizon_unix = read_u64(rest, 18)?;
                let root_hash = read_32(rest, 26)?;
                if count as usize > MAX_SYNC_RECORDS_PER_CONVERSATION {
                    return Err(Error::Crypto("RootOffer count exceeds max records".into()));
                }
                Ok(Self::RootOffer {
                    conv_id,
                    snapshot_id,
                    count,
                    horizon_unix,
                    root_hash,
                })
            }
            FrameTag::NodeRequest => {
                if rest.len() != 16 + 4 + 4 + 1 {
                    return Err(Error::Crypto("Malformed NodeRequest SyncFrame".into()));
                }
                let snapshot_id = read_16(rest, 0)?;
                let start = read_u32(rest, 16)?;
                let end = read_u32(rest, 20)?;
                let depth = rest[24];
                if start >= end || end > KEYSPACE_END || depth > MAX_MERKLE_DEPTH {
                    return Err(Error::Crypto("Malformed Merkle NodeRequest range".into()));
                }
                Ok(Self::NodeRequest {
                    conv_id,
                    snapshot_id,
                    start,
                    end,
                    depth,
                })
            }
            FrameTag::NodeResponse => {
                const MIN: usize = 16 + 4 + 4 + 4 + 1 + 32 + 32 + 2;
                if rest.len() < MIN {
                    return Err(Error::Crypto("Truncated NodeResponse SyncFrame".into()));
                }
                let snapshot_id = read_16(rest, 0)?;
                let start = read_u32(rest, 16)?;
                let mid = read_u32(rest, 20)?;
                let end = read_u32(rest, 24)?;
                let is_leaf = rest[28] != 0;
                let left_hash = read_32(rest, 29)?;
                let right_hash = read_32(rest, 61)?;
                let n_items = u16::from_be_bytes([rest[93], rest[94]]) as usize;
                if start >= end || end > KEYSPACE_END || mid < start || mid > end {
                    return Err(Error::Crypto("Invalid NodeResponse bounds".into()));
                }
                if n_items > MAX_INVENTORY_ITEMS_PER_FRAME {
                    return Err(Error::Crypto(
                        "NodeResponse leaf count exceeds limit".into(),
                    ));
                }
                let items_bytes = n_items * 17;
                if rest.len() != MIN + items_bytes {
                    return Err(Error::Crypto("NodeResponse length mismatch".into()));
                }
                let mut leaf_items = Vec::with_capacity(n_items);
                for i in 0..n_items {
                    let base = MIN + i * 17;
                    let id = read_16(rest, base)?;
                    let kind = SyncRecordKind::from_u8(rest[base + 16])?;
                    leaf_items.push((id, kind));
                }
                Ok(Self::NodeResponse {
                    conv_id,
                    snapshot_id,
                    start,
                    mid,
                    end,
                    is_leaf,
                    left_hash,
                    right_hash,
                    leaf_items,
                })
            }
            FrameTag::InventoryOffer => {
                const MIN: usize = 16 + 8 + 2 + 2 + 1 + 2;
                if rest.len() < MIN {
                    return Err(Error::Crypto("Truncated InventoryOffer SyncFrame".into()));
                }
                let snapshot_id = read_16(rest, 0)?;
                let horizon_unix = read_u64(rest, 16)?;
                let offset = u16::from_be_bytes([rest[24], rest[25]]);
                let total = u16::from_be_bytes([rest[26], rest[27]]);
                let done = rest[28] != 0;
                let n_items = u16::from_be_bytes([rest[29], rest[30]]) as usize;
                if n_items > MAX_INVENTORY_ITEMS_PER_FRAME
                    || total as usize > MAX_SYNC_RECORDS_PER_CONVERSATION
                {
                    return Err(Error::Crypto("InventoryOffer exceeds bounds".into()));
                }
                if rest.len() != MIN + n_items * 17 {
                    return Err(Error::Crypto("InventoryOffer length mismatch".into()));
                }
                let mut items = Vec::with_capacity(n_items);
                for i in 0..n_items {
                    let base = MIN + i * 17;
                    let id = read_16(rest, base)?;
                    let kind = SyncRecordKind::from_u8(rest[base + 16])?;
                    items.push((id, kind));
                }
                Ok(Self::InventoryOffer {
                    conv_id,
                    snapshot_id,
                    horizon_unix,
                    offset,
                    total,
                    done,
                    items,
                })
            }
            FrameTag::RecordRequest => {
                if rest.len() < 18 {
                    return Err(Error::Crypto("Truncated RecordRequest SyncFrame".into()));
                }
                let snapshot_id = read_16(rest, 0)?;
                let done = rest[16] != 0;
                let n = rest[17] as usize;
                if n > MAX_SYNC_RECORDS_PER_FRAME || rest.len() != 18 + n * 16 {
                    return Err(Error::Crypto("Invalid RecordRequest bounds".into()));
                }
                let mut ids = Vec::with_capacity(n);
                for i in 0..n {
                    ids.push(read_16(rest, 18 + i * 16)?);
                }
                Ok(Self::RecordRequest {
                    conv_id,
                    snapshot_id,
                    done,
                    ids,
                })
            }
            FrameTag::RecordBatch => {
                if rest.len() < 18 {
                    return Err(Error::Crypto("Truncated RecordBatch SyncFrame".into()));
                }
                let snapshot_id = read_16(rest, 0)?;
                let done = rest[16] != 0;
                let n = rest[17] as usize;
                if n > MAX_SYNC_RECORDS_PER_FRAME {
                    return Err(Error::Crypto(
                        "RecordBatch count exceeds frame limit".into(),
                    ));
                }
                let mut offset = 18usize;
                let mut records = Vec::with_capacity(n);
                for _ in 0..n {
                    if offset + 2 > rest.len() {
                        return Err(Error::Crypto("Truncated RecordBatch entry len".into()));
                    }
                    let wire_len = u16::from_be_bytes([rest[offset], rest[offset + 1]]) as usize;
                    offset += 2;
                    if offset + wire_len > rest.len() {
                        return Err(Error::Crypto("Truncated RecordBatch entry".into()));
                    }
                    let (rec, consumed) =
                        SyncRecord::decode_wire(&rest[offset..offset + wire_len])?;
                    if consumed != wire_len {
                        return Err(Error::Crypto(
                            "Trailing bytes inside RecordBatch item".into(),
                        ));
                    }
                    records.push(rec);
                    offset += wire_len;
                }
                if offset != rest.len() {
                    return Err(Error::Crypto("Trailing bytes in RecordBatch".into()));
                }
                Ok(Self::RecordBatch {
                    conv_id,
                    snapshot_id,
                    records,
                    done,
                })
            }
            FrameTag::SnapshotMismatch => {
                if !rest.is_empty() {
                    return Err(Error::Crypto("Malformed SnapshotMismatch SyncFrame".into()));
                }
                Ok(Self::SnapshotMismatch { conv_id })
            }
            FrameTag::Done => {
                if !rest.is_empty() {
                    return Err(Error::Crypto("Malformed Done SyncFrame".into()));
                }
                Ok(Self::Done { conv_id })
            }
            FrameTag::Abort => {
                if rest.is_empty() {
                    return Err(Error::Crypto("Malformed Abort SyncFrame".into()));
                }
                let rlen = (rest[0] as usize).min(MAX_ABORT_REASON_LEN);
                if rest.len() != 1 + rlen {
                    return Err(Error::Crypto("Malformed Abort reason len".into()));
                }
                let reason = String::from_utf8_lossy(&rest[1..]).into_owned();
                Ok(Self::Abort { conv_id, reason })
            }
        }
    }
}

fn read_16(buf: &[u8], off: usize) -> Result<[u8; 16]> {
    let slice = buf
        .get(off..off + 16)
        .ok_or_else(|| Error::Crypto("Out of bounds 16B read".into()))?;
    let mut out = [0u8; 16];
    out.copy_from_slice(slice);
    Ok(out)
}

fn read_32(buf: &[u8], off: usize) -> Result<[u8; 32]> {
    let slice = buf
        .get(off..off + 32)
        .ok_or_else(|| Error::Crypto("Out of bounds 32B read".into()))?;
    let mut out = [0u8; 32];
    out.copy_from_slice(slice);
    Ok(out)
}

fn read_u32(buf: &[u8], off: usize) -> Result<u32> {
    let slice = buf
        .get(off..off + 4)
        .ok_or_else(|| Error::Crypto("Out of bounds u32 read".into()))?;
    Ok(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_u64(buf: &[u8], off: usize) -> Result<u64> {
    let slice = buf
        .get(off..off + 8)
        .ok_or_else(|| Error::Crypto("Out of bounds u64 read".into()))?;
    Ok(u64::from_be_bytes([
        slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
    ]))
}

pub fn encode_sync_chunks(msg_seq: u16, frame: &SyncFrame) -> Result<Vec<Vec<u8>>> {
    let raw = frame.encode()?;
    let total = raw.len().div_ceil(MAX_SYNC_CHUNK_PAYLOAD).max(1);
    if total > MAX_SYNC_CHUNKS_PER_FRAME as usize {
        return Err(Error::Crypto("SyncFrame chunk limit exceeded".into()));
    }
    let total_u8 = total as u8;
    let mut chunks = Vec::with_capacity(total);
    for (idx, part) in raw.chunks(MAX_SYNC_CHUNK_PAYLOAD).enumerate() {
        let mut c = Vec::with_capacity(SYNC_CHUNK_HEADER_LEN + part.len());
        c.extend_from_slice(SYNC_MAGIC);
        c.push(SYNC_WIRE_VERSION);
        c.extend_from_slice(&msg_seq.to_be_bytes());
        c.push(idx as u8);
        c.push(total_u8);
        c.extend_from_slice(part);
        chunks.push(c);
    }
    Ok(chunks)
}

struct PartialChunkAssembly {
    total: u8,
    received: u8,
    total_bytes: usize,
    parts: Vec<Option<Vec<u8>>>,
}

struct ChunkAssembler {
    pending: HashMap<u16, PartialChunkAssembly>,
    order: VecDeque<u16>,
}

impl ChunkAssembler {
    fn new() -> Self {
        Self {
            pending: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn feed(&mut self, chunk: &[u8]) -> Result<Option<Vec<u8>>> {
        if chunk.len() < SYNC_CHUNK_HEADER_LEN || &chunk[0..4] != SYNC_MAGIC {
            return Err(Error::Crypto("Invalid BC2S chunk header".into()));
        }
        if chunk[4] != SYNC_WIRE_VERSION {
            return Err(Error::Crypto("Unsupported BC2S wire version".into()));
        }
        let msg_seq = u16::from_be_bytes([chunk[5], chunk[6]]);
        let idx = chunk[7];
        let total = chunk[8];
        if total == 0 || total > MAX_SYNC_CHUNKS_PER_FRAME || idx >= total {
            return Err(Error::Crypto("Malformed BC2S chunk index/total".into()));
        }
        let payload = &chunk[SYNC_CHUNK_HEADER_LEN..];
        if total == 1 {
            return Ok(Some(payload.to_vec()));
        }

        if !self.pending.contains_key(&msg_seq) {
            if self.pending.len() >= 16 {
                if let Some(old_seq) = self.order.pop_front() {
                    self.pending.remove(&old_seq);
                }
            }
            self.order.push_back(msg_seq);
            self.pending.insert(
                msg_seq,
                PartialChunkAssembly {
                    total,
                    received: 0,
                    total_bytes: 0,
                    parts: vec![None; total as usize],
                },
            );
        }

        let complete = if let Some(entry) = self.pending.get_mut(&msg_seq) {
            if entry.total != total {
                self.pending.remove(&msg_seq);
                return Err(Error::Crypto("Mismatched BC2S chunk total".into()));
            }
            if entry.parts[idx as usize].is_none() {
                if entry.total_bytes + payload.len() > MAX_SYNC_RAW_FRAME_BYTES {
                    self.pending.remove(&msg_seq);
                    return Err(Error::Crypto("BC2S assembled frame limit exceeded".into()));
                }
                entry.total_bytes += payload.len();
                entry.parts[idx as usize] = Some(payload.to_vec());
                entry.received += 1;
            }
            entry.received == entry.total
        } else {
            false
        };

        if complete {
            if let Some(entry) = self.pending.remove(&msg_seq) {
                let mut assembled = Vec::with_capacity(entry.total_bytes);
                for p in entry.parts.into_iter().flatten() {
                    assembled.extend_from_slice(&p);
                }
                return Ok(Some(assembled));
            }
        }
        Ok(None)
    }
}

/// Session-level accounting metrics (also drives Task 9/12 ID-inventory vs Merkle comparison).
#[derive(Debug, Clone, Default)]
pub struct SyncSessionMetrics {
    pub wire_bytes_sent: usize,
    pub wire_bytes_recv: usize,
    pub rounds: u32,
    pub merkle_nodes_queried: usize,
    pub records_imported: usize,
    pub records_sent: usize,
    pub records_rejected: usize,
    pub snapshot_restarts: u8,
    pub completed: bool,
    pub aborted_reason: Option<String>,
}

/// Outcome returned from processing an inbound chunk (outbound chunks + newly imported Message IDs).
#[derive(Debug, Default)]
pub struct SyncInboundOutcome {
    pub outbound_chunks: Vec<Vec<u8>>,
    pub imported_message_ids: Vec<[u8; 16]>,
}

struct ActiveSyncSession {
    conv_id: [u8; 32],
    is_initiator: bool,
    use_merkle: bool,
    deadline: Instant,
    local_snap: SyncSnapshot,
    local_snap_stamp: (u64, i64),
    remote_snap_id: Option<[u8; 16]>,
    restarts: u8,
    assembler: ChunkAssembler,
    next_msg_seq: u16,
    pending_ranges: Vec<(u32, u32, u8)>,
    pull_ids: BTreeSet<[u8; 16]>,
    push_ids: BTreeSet<[u8; 16]>,
    remote_inventory: BTreeMap<[u8; 16], SyncRecordKind>,
    metrics: SyncSessionMetrics,
}

impl ActiveSyncSession {
    fn emit_frames(&mut self, frames: Vec<SyncFrame>) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for f in frames {
            self.next_msg_seq = self.next_msg_seq.wrapping_add(1);
            if let Ok(chunks) = encode_sync_chunks(self.next_msg_seq, &f) {
                for c in chunks {
                    self.metrics.wire_bytes_sent =
                        self.metrics.wire_bytes_sent.saturating_add(c.len());
                    out.push(c);
                }
            }
        }
        out
    }

    fn emit_batches_for_push(
        &self,
        store: Option<&SqliteSyncStore>,
        push_ids: &BTreeSet<[u8; 16]>,
        now_unix: u64,
        mark_done: bool,
    ) -> Result<Vec<SyncFrame>> {
        if push_ids.is_empty() {
            return Ok(Vec::new());
        }
        let wanted: Vec<[u8; 16]> = push_ids.iter().copied().collect();
        let records: Vec<SyncRecord> = if let Some(s) = store {
            s.get_records_by_ids(&self.conv_id, &wanted, now_unix)?
        } else {
            self.local_snap
                .records
                .iter()
                .filter(|r| push_ids.contains(&r.logical_id))
                .cloned()
                .collect()
        };

        let mut frames = Vec::new();
        let mut current = Vec::new();
        let mut current_bytes = 0usize;

        for rec in records {
            let rec_len = rec.encode_wire()?.len();
            if !current.is_empty()
                && (current.len() >= MAX_SYNC_RECORDS_PER_FRAME
                    || current_bytes + rec_len > MAX_BATCH_RAW_BYTES)
            {
                frames.push(SyncFrame::RecordBatch {
                    conv_id: self.conv_id,
                    snapshot_id: self.local_snap.snapshot_id,
                    records: std::mem::take(&mut current),
                    done: false,
                });
                current_bytes = 0;
            }
            current_bytes += rec_len;
            current.push(rec);
        }
        if !current.is_empty() {
            frames.push(SyncFrame::RecordBatch {
                conv_id: self.conv_id,
                snapshot_id: self.local_snap.snapshot_id,
                records: current,
                done: mark_done,
            });
        }
        Ok(frames)
    }

    fn emit_requests_for_pull(&self, pull_ids: &BTreeSet<[u8; 16]>) -> Vec<SyncFrame> {
        let mut frames = Vec::new();
        let all: Vec<[u8; 16]> = pull_ids.iter().copied().collect();
        let chunks: Vec<_> = all.chunks(MAX_SYNC_RECORDS_PER_FRAME).collect();
        let total = chunks.len();
        for (i, chunk) in chunks.into_iter().enumerate() {
            frames.push(SyncFrame::RecordRequest {
                conv_id: self.conv_id,
                snapshot_id: self.remote_snap_id.unwrap_or([0u8; 16]),
                done: i + 1 == total,
                ids: chunk.to_vec(),
            });
        }
        frames
    }
}

/// Coordinator managing bounded per-peer sync sessions (Default OFF).
pub struct SyncSessionManager {
    enabled: bool,
    sessions: HashMap<PeerId, ActiveSyncSession>,
    pub last_metrics: HashMap<PeerId, SyncSessionMetrics>,
}

impl Default for SyncSessionManager {
    fn default() -> Self {
        Self::new(false)
    }
}

impl SyncSessionManager {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            sessions: HashMap::new(),
            last_metrics: HashMap::new(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.sessions.clear();
        }
    }

    /// Immediately terminate active sync state for `peer` (called on revoke/block/disconnect).
    pub fn abort_peer(&mut self, peer: &PeerId) {
        if let Some(mut sess) = self.sessions.remove(peer) {
            sess.metrics.aborted_reason = Some("PEER_REVOKED_OR_DISCONNECTED".into());
            self.last_metrics.insert(*peer, sess.metrics);
        }
    }

    pub fn active_session_count(&self) -> usize {
        self.sessions.len()
    }

    pub fn metrics_for(&self, peer: &PeerId) -> Option<&SyncSessionMetrics> {
        self.sessions
            .get(peer)
            .map(|s| &s.metrics)
            .or_else(|| self.last_metrics.get(peer))
    }

    /// Start a bounded reconciliation session with `remote_peer`.
    pub fn initiate_session(
        &mut self,
        local_peer: PeerId,
        remote_peer: PeerId,
        store: &SqliteSyncStore,
        use_merkle: bool,
        now_inst: Instant,
        now_unix: u64,
    ) -> Result<Vec<Vec<u8>>> {
        if !self.enabled {
            return Err(Error::Crypto("Sync feature is disabled".into()));
        }
        let conv_id = conversation_id(&local_peer, &remote_peer);
        let snap = store.snapshot_for_conversation(&conv_id, now_unix)?;
        let snap_stamp = store.change_stamp().unwrap_or((0, 0));
        let mut sess = ActiveSyncSession {
            conv_id,
            is_initiator: true,
            use_merkle,
            deadline: now_inst + SYNC_SESSION_DEADLINE,
            local_snap: snap,
            local_snap_stamp: snap_stamp,
            remote_snap_id: None,
            restarts: 0,
            assembler: ChunkAssembler::new(),
            next_msg_seq: 0,
            pending_ranges: Vec::new(),
            pull_ids: BTreeSet::new(),
            push_ids: BTreeSet::new(),
            remote_inventory: BTreeMap::new(),
            metrics: SyncSessionMetrics::default(),
        };
        let chunks = sess.emit_frames(vec![SyncFrame::Hello {
            conv_id,
            use_merkle,
        }]);
        sess.metrics.rounds += 1;
        self.sessions.insert(remote_peer, sess);
        Ok(chunks)
    }

    /// Handle one inbound `BC2S` wire chunk from verified approved peer `remote_peer`.
    pub fn handle_inbound_chunk(
        &mut self,
        local_peer: PeerId,
        remote_peer: PeerId,
        chunk: &[u8],
        store: Option<&SqliteSyncStore>,
        now_inst: Instant,
        now_unix: u64,
    ) -> SyncInboundOutcome {
        let mut outcome = SyncInboundOutcome::default();
        let expected_conv = conversation_id(&local_peer, &remote_peer);

        // Enforce disabled state without storing state or panicking
        if !self.enabled || store.is_none() {
            let mut tmp_asm = ChunkAssembler::new();
            if let Ok(Some(raw)) = tmp_asm.feed(chunk) {
                if let Ok(SyncFrame::Hello { conv_id, .. }) = SyncFrame::decode(&raw) {
                    if conv_id == expected_conv {
                        if let Ok(ack) = encode_sync_chunks(
                            1,
                            &SyncFrame::HelloAck {
                                conv_id,
                                supported: false,
                                use_merkle: false,
                            },
                        ) {
                            outcome.outbound_chunks = ack;
                        }
                    }
                }
            }
            return outcome;
        }
        let Some(store) = store else {
            return outcome;
        };

        // Ensure an inbound Hello can bootstrap a responder session cleanly
        if let std::collections::hash_map::Entry::Vacant(e) = self.sessions.entry(remote_peer) {
            let mut probe = ChunkAssembler::new();
            if let Ok(Some(raw)) = probe.feed(chunk) {
                if let Ok(SyncFrame::Hello {
                    conv_id,
                    use_merkle,
                }) = SyncFrame::decode(&raw)
                {
                    if conv_id != expected_conv {
                        return outcome;
                    }
                    let Ok(snap) = store.snapshot_for_conversation(&conv_id, now_unix) else {
                        return outcome;
                    };
                    let snap_stamp = store.change_stamp().unwrap_or((0, 0));
                    let mut sess = ActiveSyncSession {
                        conv_id,
                        is_initiator: false,
                        use_merkle,
                        deadline: now_inst + SYNC_SESSION_DEADLINE,
                        local_snap: snap,
                        local_snap_stamp: snap_stamp,
                        remote_snap_id: None,
                        restarts: 0,
                        assembler: ChunkAssembler::new(),
                        next_msg_seq: 0,
                        pending_ranges: Vec::new(),
                        pull_ids: BTreeSet::new(),
                        push_ids: BTreeSet::new(),
                        remote_inventory: BTreeMap::new(),
                        metrics: SyncSessionMetrics {
                            wire_bytes_recv: chunk.len(),
                            rounds: 1,
                            ..SyncSessionMetrics::default()
                        },
                    };
                    let chunks = sess.emit_frames(vec![SyncFrame::HelloAck {
                        conv_id,
                        supported: true,
                        use_merkle,
                    }]);
                    e.insert(sess);
                    outcome.outbound_chunks = chunks;
                    return outcome;
                }
            }
            return outcome;
        }

        let Some(mut sess) = self.sessions.remove(&remote_peer) else {
            return outcome;
        };

        if now_inst > sess.deadline {
            sess.metrics.aborted_reason = Some("SESSION_DEADLINE_EXCEEDED".into());
            outcome.outbound_chunks = sess.emit_frames(vec![SyncFrame::Abort {
                conv_id: sess.conv_id,
                reason: "SESSION_DEADLINE_EXCEEDED".into(),
            }]);
            self.last_metrics.insert(remote_peer, sess.metrics);
            return outcome;
        }

        sess.metrics.wire_bytes_recv = sess.metrics.wire_bytes_recv.saturating_add(chunk.len());
        if sess.metrics.wire_bytes_sent + sess.metrics.wire_bytes_recv > MAX_SYNC_SESSION_BYTES {
            sess.metrics.aborted_reason = Some("SESSION_BYTE_QUOTA_EXCEEDED".into());
            outcome.outbound_chunks = sess.emit_frames(vec![SyncFrame::Abort {
                conv_id: sess.conv_id,
                reason: "SESSION_BYTE_QUOTA_EXCEEDED".into(),
            }]);
            self.last_metrics.insert(remote_peer, sess.metrics);
            return outcome;
        }

        let raw_opt = match sess.assembler.feed(chunk) {
            Ok(v) => v,
            Err(e) => {
                sess.metrics.aborted_reason = Some(format!("CHUNK_ERR: {e}"));
                outcome.outbound_chunks = sess.emit_frames(vec![SyncFrame::Abort {
                    conv_id: sess.conv_id,
                    reason: "MALFORMED_CHUNK".into(),
                }]);
                self.last_metrics.insert(remote_peer, sess.metrics);
                return outcome;
            }
        };
        let Some(raw) = raw_opt else {
            // Waiting for remaining chunks of same SyncFrame
            self.sessions.insert(remote_peer, sess);
            return outcome;
        };

        let frame = match SyncFrame::decode(&raw) {
            Ok(f) => f,
            Err(e) => {
                sess.metrics.aborted_reason = Some(format!("DECODE_ERR: {e}"));
                outcome.outbound_chunks = sess.emit_frames(vec![SyncFrame::Abort {
                    conv_id: sess.conv_id,
                    reason: "MALFORMED_FRAME".into(),
                }]);
                self.last_metrics.insert(remote_peer, sess.metrics);
                return outcome;
            }
        };

        if *frame.conv_id() != expected_conv {
            sess.metrics.aborted_reason = Some("CONV_ID_MISMATCH".into());
            outcome.outbound_chunks = sess.emit_frames(vec![SyncFrame::Abort {
                conv_id: sess.conv_id,
                reason: "CONV_ID_MISMATCH".into(),
            }]);
            self.last_metrics.insert(remote_peer, sess.metrics);
            return outcome;
        }

        sess.metrics.rounds += 1;
        let mut keep_session = true;

        match frame {
            SyncFrame::Hello { use_merkle, .. } => {
                let Ok(snap) = store.snapshot_for_conversation(&expected_conv, now_unix) else {
                    return outcome;
                };
                sess.use_merkle = use_merkle;
                sess.local_snap = snap;
                sess.local_snap_stamp = store.change_stamp().unwrap_or((0, 0));
                outcome.outbound_chunks = sess.emit_frames(vec![SyncFrame::HelloAck {
                    conv_id: expected_conv,
                    supported: true,
                    use_merkle,
                }]);
            }
            SyncFrame::HelloAck {
                supported,
                use_merkle,
                ..
            } => {
                if !supported {
                    sess.metrics.completed = true;
                    sess.metrics.aborted_reason = Some("UNSUPPORTED_BY_PEER".into());
                    keep_session = false;
                } else if use_merkle {
                    outcome.outbound_chunks = sess.emit_frames(vec![SyncFrame::RootOffer {
                        conv_id: expected_conv,
                        snapshot_id: sess.local_snap.snapshot_id,
                        count: sess.local_snap.records.len() as u16,
                        horizon_unix: sess.local_snap.horizon_unix,
                        root_hash: sess.local_snap.root_hash,
                    }]);
                } else {
                    // Send IdInventory baseline
                    let all_items: Vec<([u8; 16], SyncRecordKind)> = sess
                        .local_snap
                        .records
                        .iter()
                        .map(|r| (r.logical_id, r.kind))
                        .collect();
                    let total = all_items.len() as u16;
                    let mut out_frames = Vec::new();
                    if all_items.is_empty() {
                        out_frames.push(SyncFrame::InventoryOffer {
                            conv_id: expected_conv,
                            snapshot_id: sess.local_snap.snapshot_id,
                            horizon_unix: sess.local_snap.horizon_unix,
                            offset: 0,
                            total: 0,
                            done: true,
                            items: Vec::new(),
                        });
                    } else {
                        let chunks_vec: Vec<_> =
                            all_items.chunks(MAX_INVENTORY_ITEMS_PER_FRAME).collect();
                        let num = chunks_vec.len();
                        for (i, ch) in chunks_vec.into_iter().enumerate() {
                            out_frames.push(SyncFrame::InventoryOffer {
                                conv_id: expected_conv,
                                snapshot_id: sess.local_snap.snapshot_id,
                                horizon_unix: sess.local_snap.horizon_unix,
                                offset: (i * MAX_INVENTORY_ITEMS_PER_FRAME) as u16,
                                total,
                                done: i + 1 == num,
                                items: ch.to_vec(),
                            });
                        }
                    }
                    outcome.outbound_chunks = sess.emit_frames(out_frames);
                }
            }
            SyncFrame::RootOffer {
                snapshot_id,
                root_hash,
                ..
            } => {
                // Refresh snapshot at start/restart of tree compare
                if let Ok(fresh) = store.snapshot_for_conversation(&expected_conv, now_unix) {
                    sess.local_snap = fresh;
                    sess.local_snap_stamp = store.change_stamp().unwrap_or((0, 0));
                }
                if sess.local_snap.root_hash == root_hash {
                    sess.metrics.completed = true;
                    outcome.outbound_chunks = sess.emit_frames(vec![SyncFrame::Done {
                        conv_id: expected_conv,
                    }]);
                    keep_session = false;
                } else if sess.remote_snap_id.is_none() {
                    sess.remote_snap_id = Some(snapshot_id);
                    if !sess.is_initiator {
                        outcome.outbound_chunks = sess.emit_frames(vec![SyncFrame::RootOffer {
                            conv_id: expected_conv,
                            snapshot_id: sess.local_snap.snapshot_id,
                            count: sess.local_snap.records.len() as u16,
                            horizon_unix: sess.local_snap.horizon_unix,
                            root_hash: sess.local_snap.root_hash,
                        }]);
                    } else {
                        sess.metrics.merkle_nodes_queried += 1;
                        outcome.outbound_chunks = sess.emit_frames(vec![SyncFrame::NodeRequest {
                            conv_id: expected_conv,
                            snapshot_id,
                            start: 0,
                            end: KEYSPACE_END,
                            depth: 0,
                        }]);
                    }
                } else {
                    // Restart RootOffer arrived
                    sess.remote_snap_id = Some(snapshot_id);
                    sess.pending_ranges.clear();
                    sess.pull_ids.clear();
                    sess.push_ids.clear();
                    sess.metrics.merkle_nodes_queried += 1;
                    outcome.outbound_chunks = sess.emit_frames(vec![SyncFrame::NodeRequest {
                        conv_id: expected_conv,
                        snapshot_id,
                        start: 0,
                        end: KEYSPACE_END,
                        depth: 0,
                    }]);
                }
            }
            SyncFrame::NodeRequest {
                snapshot_id,
                start,
                end,
                depth,
                ..
            } => {
                sess.metrics.merkle_nodes_queried += 1;
                if sess.metrics.merkle_nodes_queried > MAX_MERKLE_NODES_PER_SESSION {
                    sess.metrics.aborted_reason = Some("MERKLE_NODE_BUDGET_EXCEEDED".into());
                    outcome.outbound_chunks = sess.emit_frames(vec![SyncFrame::Abort {
                        conv_id: expected_conv,
                        reason: "MERKLE_NODE_BUDGET_EXCEEDED".into(),
                    }]);
                    keep_session = false;
                } else {
                    // Detect concurrent SQLite mutation between NodeRequests in O(1) unless DB mutated
                    let mut drifted = snapshot_id != sess.local_snap.snapshot_id;
                    let mut refreshed_snap: Option<(SyncSnapshot, (u64, i64))> = None;
                    if !drifted {
                        match store.change_stamp() {
                            Ok(cur_stamp) if cur_stamp == sess.local_snap_stamp => {}
                            Ok(cur_stamp) => {
                                if let Ok(cur) =
                                    store.snapshot_for_conversation(&expected_conv, now_unix)
                                {
                                    if cur.snapshot_id != sess.local_snap.snapshot_id {
                                        drifted = true;
                                        refreshed_snap = Some((cur, cur_stamp));
                                    } else {
                                        sess.local_snap_stamp = cur_stamp;
                                    }
                                } else {
                                    drifted = true;
                                }
                            }
                            Err(_) => drifted = true,
                        }
                    }
                    if drifted {
                        if sess.restarts < MAX_SNAPSHOT_RESTARTS {
                            sess.restarts += 1;
                            sess.metrics.snapshot_restarts = sess.restarts;
                            if let Some((cur, cur_stamp)) = refreshed_snap {
                                sess.local_snap = cur;
                                sess.local_snap_stamp = cur_stamp;
                            } else if let Ok(cur) =
                                store.snapshot_for_conversation(&expected_conv, now_unix)
                            {
                                sess.local_snap = cur;
                                sess.local_snap_stamp = store.change_stamp().unwrap_or((0, 0));
                            }
                            outcome.outbound_chunks =
                                sess.emit_frames(vec![SyncFrame::SnapshotMismatch {
                                    conv_id: expected_conv,
                                }]);
                        } else {
                            sess.metrics.aborted_reason = Some("SNAPSHOT_RESTART_LIMIT".into());
                            outcome.outbound_chunks = sess.emit_frames(vec![SyncFrame::Abort {
                                conv_id: expected_conv,
                                reason: "SNAPSHOT_RESTART_LIMIT".into(),
                            }]);
                            keep_session = false;
                        }
                    } else if let Ok(insp) = sess.local_snap.inspect_range(start, end, depth) {
                        let resp = match insp {
                            RangeInspection::Split {
                                mid,
                                left_hash,
                                right_hash,
                            } => SyncFrame::NodeResponse {
                                conv_id: expected_conv,
                                snapshot_id,
                                start,
                                mid,
                                end,
                                is_leaf: false,
                                left_hash,
                                right_hash,
                                leaf_items: Vec::new(),
                            },
                            RangeInspection::LeafItems(items) => SyncFrame::NodeResponse {
                                conv_id: expected_conv,
                                snapshot_id,
                                start,
                                mid: start,
                                end,
                                is_leaf: true,
                                left_hash: [0u8; 32],
                                right_hash: [0u8; 32],
                                leaf_items: items,
                            },
                        };
                        outcome.outbound_chunks = sess.emit_frames(vec![resp]);
                    } else {
                        sess.metrics.aborted_reason = Some("INVALID_MERKLE_RANGE".into());
                        outcome.outbound_chunks = sess.emit_frames(vec![SyncFrame::Abort {
                            conv_id: expected_conv,
                            reason: "INVALID_MERKLE_RANGE".into(),
                        }]);
                        keep_session = false;
                    }
                }
            }
            SyncFrame::SnapshotMismatch { .. } => {
                if sess.restarts < MAX_SNAPSHOT_RESTARTS {
                    sess.restarts += 1;
                    sess.metrics.snapshot_restarts = sess.restarts;
                    if let Ok(cur) = store.snapshot_for_conversation(&expected_conv, now_unix) {
                        sess.local_snap = cur;
                        sess.local_snap_stamp = store.change_stamp().unwrap_or((0, 0));
                    }
                    sess.remote_snap_id = None;
                    sess.pending_ranges.clear();
                    sess.pull_ids.clear();
                    sess.push_ids.clear();
                    outcome.outbound_chunks = sess.emit_frames(vec![SyncFrame::RootOffer {
                        conv_id: expected_conv,
                        snapshot_id: sess.local_snap.snapshot_id,
                        count: sess.local_snap.records.len() as u16,
                        horizon_unix: sess.local_snap.horizon_unix,
                        root_hash: sess.local_snap.root_hash,
                    }]);
                } else {
                    sess.metrics.aborted_reason = Some("SNAPSHOT_RESTART_LIMIT".into());
                    keep_session = false;
                }
            }
            SyncFrame::NodeResponse {
                snapshot_id,
                start,
                mid,
                end,
                is_leaf,
                left_hash,
                right_hash,
                leaf_items,
                ..
            } => {
                // Verify initiator local snapshot hasn't mutated mid-walk in O(1) unless DB mutated
                let mut local_drifted = false;
                let mut refreshed_snap: Option<(SyncSnapshot, (u64, i64))> = None;
                match store.change_stamp() {
                    Ok(cur_stamp) if cur_stamp == sess.local_snap_stamp => {}
                    Ok(cur_stamp) => {
                        if let Ok(cur) = store.snapshot_for_conversation(&expected_conv, now_unix) {
                            if cur.snapshot_id != sess.local_snap.snapshot_id {
                                local_drifted = true;
                                refreshed_snap = Some((cur, cur_stamp));
                            } else {
                                sess.local_snap_stamp = cur_stamp;
                            }
                        } else {
                            local_drifted = true;
                        }
                    }
                    Err(_) => local_drifted = true,
                }
                if local_drifted {
                    if sess.restarts < MAX_SNAPSHOT_RESTARTS {
                        sess.restarts += 1;
                        sess.metrics.snapshot_restarts = sess.restarts;
                        if let Some((cur, cur_stamp)) = refreshed_snap {
                            sess.local_snap = cur;
                            sess.local_snap_stamp = cur_stamp;
                        }
                        sess.remote_snap_id = None;
                        sess.pending_ranges.clear();
                        sess.pull_ids.clear();
                        sess.push_ids.clear();
                        outcome.outbound_chunks = sess.emit_frames(vec![SyncFrame::RootOffer {
                            conv_id: expected_conv,
                            snapshot_id: sess.local_snap.snapshot_id,
                            count: sess.local_snap.records.len() as u16,
                            horizon_unix: sess.local_snap.horizon_unix,
                            root_hash: sess.local_snap.root_hash,
                        }]);
                    } else {
                        sess.metrics.aborted_reason = Some("SNAPSHOT_RESTART_LIMIT".into());
                        keep_session = false;
                    }
                } else {
                    let depth = depth_for_width(start, end);
                    if !is_leaf {
                        let empty_l = empty_range_hash(&expected_conv, start, mid);
                        let empty_r = empty_range_hash(&expected_conv, mid, end);
                        let local_l = sess
                            .local_snap
                            .range_hash(start, mid, depth + 1)
                            .unwrap_or(empty_l);
                        let local_r = sess
                            .local_snap
                            .range_hash(mid, end, depth + 1)
                            .unwrap_or(empty_r);

                        if local_l != left_hash {
                            if left_hash == empty_l {
                                for rec in sess.local_snap.records_in_range(start, mid) {
                                    sess.push_ids.insert(rec.logical_id);
                                }
                            } else {
                                sess.pending_ranges.push((start, mid, depth + 1));
                            }
                        }
                        if local_r != right_hash {
                            if right_hash == empty_r {
                                for rec in sess.local_snap.records_in_range(mid, end) {
                                    sess.push_ids.insert(rec.logical_id);
                                }
                            } else {
                                sess.pending_ranges.push((mid, end, depth + 1));
                            }
                        }
                    } else {
                        let mut remote_map = BTreeMap::new();
                        for (id, kind) in leaf_items {
                            remote_map.insert(id, kind);
                        }
                        let mut local_map = BTreeMap::new();
                        for rec in sess.local_snap.records_in_range(start, end) {
                            local_map.insert(rec.logical_id, rec.kind);
                        }
                        for (id, r_kind) in &remote_map {
                            match local_map.get(id) {
                                None => {
                                    sess.pull_ids.insert(*id);
                                }
                                Some(SyncRecordKind::Message)
                                    if *r_kind == SyncRecordKind::Tombstone =>
                                {
                                    sess.pull_ids.insert(*id);
                                }
                                _ => {}
                            }
                        }
                        for (id, l_kind) in &local_map {
                            match remote_map.get(id) {
                                None => {
                                    sess.push_ids.insert(*id);
                                }
                                Some(SyncRecordKind::Message)
                                    if *l_kind == SyncRecordKind::Tombstone =>
                                {
                                    sess.push_ids.insert(*id);
                                }
                                _ => {}
                            }
                        }
                    }

                    if let Some((ns, ne, nd)) = sess.pending_ranges.pop() {
                        sess.metrics.merkle_nodes_queried += 1;
                        if sess.metrics.merkle_nodes_queried > MAX_MERKLE_NODES_PER_SESSION {
                            sess.metrics.aborted_reason =
                                Some("MERKLE_NODE_BUDGET_EXCEEDED".into());
                            outcome.outbound_chunks = sess.emit_frames(vec![SyncFrame::Abort {
                                conv_id: expected_conv,
                                reason: "MERKLE_NODE_BUDGET_EXCEEDED".into(),
                            }]);
                            keep_session = false;
                        } else {
                            outcome.outbound_chunks =
                                sess.emit_frames(vec![SyncFrame::NodeRequest {
                                    conv_id: expected_conv,
                                    snapshot_id,
                                    start: ns,
                                    end: ne,
                                    depth: nd,
                                }]);
                        }
                    } else {
                        let mut out_frames = Vec::new();
                        let pull_empty = sess.pull_ids.is_empty();
                        if !sess.push_ids.is_empty() {
                            let push_set = std::mem::take(&mut sess.push_ids);
                            sess.metrics.records_sent += push_set.len();
                            if let Ok(mut batches) = sess.emit_batches_for_push(
                                Some(store),
                                &push_set,
                                now_unix,
                                pull_empty,
                            ) {
                                out_frames.append(&mut batches);
                            }
                        }
                        if !pull_empty {
                            let pull_set = std::mem::take(&mut sess.pull_ids);
                            out_frames.append(&mut sess.emit_requests_for_pull(&pull_set));
                        } else if out_frames.is_empty() {
                            sess.metrics.completed = true;
                            out_frames.push(SyncFrame::Done {
                                conv_id: expected_conv,
                            });
                            keep_session = false;
                        }
                        outcome.outbound_chunks = sess.emit_frames(out_frames);
                    }
                }
            }
            SyncFrame::InventoryOffer { done, items, .. } => {
                for (id, kind) in items {
                    if sess.remote_inventory.len() >= MAX_SYNC_RECORDS_PER_CONVERSATION {
                        break;
                    }
                    sess.remote_inventory.insert(id, kind);
                }
                if done {
                    if let Ok(fresh) = store.snapshot_for_conversation(&expected_conv, now_unix) {
                        sess.local_snap = fresh;
                        sess.local_snap_stamp = store.change_stamp().unwrap_or((0, 0));
                    }
                    let mut local_map = BTreeMap::new();
                    for rec in &sess.local_snap.records {
                        local_map.insert(rec.logical_id, rec.kind);
                    }
                    let mut push_set = BTreeSet::new();
                    let mut pull_set = BTreeSet::new();
                    for (id, r_kind) in &sess.remote_inventory {
                        match local_map.get(id) {
                            None => {
                                pull_set.insert(*id);
                            }
                            Some(SyncRecordKind::Message)
                                if *r_kind == SyncRecordKind::Tombstone =>
                            {
                                pull_set.insert(*id);
                            }
                            _ => {}
                        }
                    }
                    for (id, l_kind) in &local_map {
                        match sess.remote_inventory.get(id) {
                            None => {
                                push_set.insert(*id);
                            }
                            Some(SyncRecordKind::Message)
                                if *l_kind == SyncRecordKind::Tombstone =>
                            {
                                push_set.insert(*id);
                            }
                            _ => {}
                        }
                    }
                    let mut out_frames = Vec::new();
                    let no_pull = pull_set.is_empty();
                    if !push_set.is_empty() {
                        sess.metrics.records_sent += push_set.len();
                        if let Ok(mut batches) =
                            sess.emit_batches_for_push(Some(store), &push_set, now_unix, no_pull)
                        {
                            out_frames.append(&mut batches);
                        }
                    }
                    if !no_pull {
                        out_frames.append(&mut sess.emit_requests_for_pull(&pull_set));
                    } else if out_frames.is_empty() {
                        sess.metrics.completed = true;
                        out_frames.push(SyncFrame::Done {
                            conv_id: expected_conv,
                        });
                        keep_session = false;
                    }
                    outcome.outbound_chunks = sess.emit_frames(out_frames);
                }
            }
            SyncFrame::RecordRequest { done, ids, .. } => {
                let req_set: BTreeSet<[u8; 16]> = ids.into_iter().collect();
                sess.metrics.records_sent += req_set.len();
                if let Ok(mut batches) =
                    sess.emit_batches_for_push(Some(store), &req_set, now_unix, done)
                {
                    if batches.is_empty() && done {
                        batches.push(SyncFrame::Done {
                            conv_id: expected_conv,
                        });
                    }
                    outcome.outbound_chunks = sess.emit_frames(batches);
                }
            }
            SyncFrame::RecordBatch { records, done, .. } => {
                let horizon = store.horizon_for(&expected_conv).unwrap_or(0);
                let mut valid_records = Vec::with_capacity(records.len());
                for rec in records {
                    if rec
                        .verify_for_conversation(&local_peer, &remote_peer, horizon, now_unix)
                        .is_ok()
                    {
                        valid_records.push(rec);
                    } else {
                        sess.metrics.records_rejected += 1;
                    }
                }
                if !valid_records.is_empty() {
                    if let Ok(inserted) =
                        store.upsert_records_atomic(&expected_conv, &valid_records, now_unix)
                    {
                        sess.metrics.records_imported += inserted;
                        for r in &valid_records {
                            if r.kind == SyncRecordKind::Message {
                                outcome.imported_message_ids.push(r.logical_id);
                            }
                        }
                    }
                }
                if done {
                    sess.metrics.completed = true;
                    outcome.outbound_chunks = sess.emit_frames(vec![SyncFrame::Done {
                        conv_id: expected_conv,
                    }]);
                    keep_session = false;
                }
            }
            SyncFrame::Done { .. } => {
                sess.metrics.completed = true;
                keep_session = false;
            }
            SyncFrame::Abort { reason, .. } => {
                sess.metrics.aborted_reason = Some(reason);
                keep_session = false;
            }
        }

        if keep_session {
            self.sessions.insert(remote_peer, sess);
        } else {
            self.last_metrics.insert(remote_peer, sess.metrics);
        }
        outcome
    }
}

fn depth_for_width(start: u32, end: u32) -> u8 {
    let width = end.saturating_sub(start).max(1);
    let log2 = (KEYSPACE_END / width).ilog2();
    log2.min(MAX_MERKLE_DEPTH as u32) as u8
}
