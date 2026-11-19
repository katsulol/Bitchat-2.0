// CRYPTO-REVIEW:
// What it does: Canonical immutable Normal-mode sync record format (`SyncRecordV1`),
// pairwise conversation derivation, and Ed25519 original-author signature verification.
// Primitives / crates: `blake3`, `ed25519-dalek`.
// Reviewer checklist:
// 1. Uses existing Ed25519 `Identity::sign` / `Identity::verify` over domain-separated
//    canonical big-endian payload (`b"bitchat2/sync-record/v1"`).
// 2. Binds `conv_id`, `logical_id`, `author`, `recipient`, timestamps, `kind`, and `blake3(body)`.
// 3. Strictly rejects cross-conversation records, mismatched authors, expired records,
//    and non-empty Tombstone bodies.

use ed25519_dalek::Signature;

use crate::error::{Error, Result};
use crate::identity::{Identity, PeerId};

pub const SYNC_RECORD_VERSION: u8 = 1;
pub const MAX_SYNC_RECORDS_PER_CONVERSATION: usize = 1024;
pub const MAX_SYNC_BODY_BYTES: usize = 4096;
pub const TOMBSTONE_RETENTION_SECS: u64 = 7 * 24 * 60 * 60;
const RECORD_WIRE_HEADER_LEN: usize = 197; // 1+32+16+32+32+8+8+1+2+64 + 1? exact: 1+32+16+32+32+8+8+1+2+64 = 196!
const CONV_DOMAIN: &[u8] = b"bitchat2/conv/v1";
const RECORD_SIGN_DOMAIN: &[u8] = b"bitchat2/sync-record/v1";

/// Deterministic pairwise conversation identifier invariant to participant argument order.
pub fn conversation_id(a: &PeerId, b: &PeerId) -> [u8; 32] {
    let (lo, hi) = if a.0 <= b.0 { (a.0, b.0) } else { (b.0, a.0) };
    let mut h = blake3::Hasher::new();
    h.update(CONV_DOMAIN);
    h.update(&lo);
    h.update(&hi);
    *h.finalize().as_bytes()
}

/// Kind of immutable history entry stored in opt-in Normal-mode sync archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum SyncRecordKind {
    Message = 1,
    Tombstone = 2,
}

impl SyncRecordKind {
    pub fn from_u8(val: u8) -> Result<Self> {
        match val {
            1 => Ok(Self::Message),
            2 => Ok(Self::Tombstone),
            other => Err(Error::Crypto(format!("Invalid SyncRecordKind: {other}"))),
        }
    }
}

/// Immutable Normal-mode retained history entry signed by its originating author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncRecord {
    pub version: u8,
    pub conv_id: [u8; 32],
    pub logical_id: [u8; 16],
    pub author: PeerId,
    pub recipient: PeerId,
    pub created_at_unix: u64,
    pub expires_at_unix: u64,
    pub kind: SyncRecordKind,
    pub body: Vec<u8>,
    pub signature: [u8; 64],
}

impl SyncRecord {
    /// Deterministic radix bucket key derived from immutable `logical_id`.
    pub fn radix_key(&self) -> u16 {
        u16::from_be_bytes([self.logical_id[0], self.logical_id[1]])
    }

    /// Canonical sort key independent of SQLite insertion order.
    pub fn sort_key(&self) -> (u16, u64, [u8; 16], u8) {
        (
            self.radix_key(),
            self.created_at_unix,
            self.logical_id,
            self.kind as u8,
        )
    }

    /// Canonical signing byte sequence covering all immutable metadata and `blake3(body)`.
    pub fn signing_payload(&self) -> Vec<u8> {
        let body_hash = blake3::hash(&self.body);
        let mut out = Vec::with_capacity(
            RECORD_SIGN_DOMAIN.len() + 1 + 32 + 16 + 32 + 32 + 8 + 8 + 1 + 4 + 32,
        );
        out.extend_from_slice(RECORD_SIGN_DOMAIN);
        out.push(self.version);
        out.extend_from_slice(&self.conv_id);
        out.extend_from_slice(&self.logical_id);
        out.extend_from_slice(self.author.as_bytes());
        out.extend_from_slice(self.recipient.as_bytes());
        out.extend_from_slice(&self.created_at_unix.to_be_bytes());
        out.extend_from_slice(&self.expires_at_unix.to_be_bytes());
        out.push(self.kind as u8);
        out.extend_from_slice(&(self.body.len() as u32).to_be_bytes());
        out.extend_from_slice(body_hash.as_bytes());
        out
    }

    /// Hash digest binding all immutable fields + Ed25519 signature.
    pub fn digest(&self) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        h.update(&self.signing_payload());
        h.update(&self.signature);
        *h.finalize().as_bytes()
    }

    /// Construct and sign a new immutable `SyncRecordV1`.
    pub fn create_signed(
        identity: &Identity,
        recipient: PeerId,
        logical_id: [u8; 16],
        created_at_unix: u64,
        expires_at_unix: u64,
        kind: SyncRecordKind,
        body: Vec<u8>,
    ) -> Result<Self> {
        let author = identity.peer_id();
        if author == recipient {
            return Err(Error::Crypto(
                "SyncRecord author and recipient cannot be identical".into(),
            ));
        }
        if expires_at_unix <= created_at_unix {
            return Err(Error::Crypto(
                "SyncRecord expiry must exceed created_at timestamp".into(),
            ));
        }
        if kind == SyncRecordKind::Tombstone && !body.is_empty() {
            return Err(Error::Crypto(
                "SyncRecord Tombstone must carry an empty body".into(),
            ));
        }
        if kind == SyncRecordKind::Message && body.is_empty() {
            return Err(Error::Crypto(
                "SyncRecord Message cannot carry an empty body".into(),
            ));
        }
        if body.len() > MAX_SYNC_BODY_BYTES {
            return Err(Error::Crypto(format!(
                "SyncRecord body {} exceeds max {MAX_SYNC_BODY_BYTES}",
                body.len()
            )));
        }

        let conv_id = conversation_id(&author, &recipient);
        let mut rec = Self {
            version: SYNC_RECORD_VERSION,
            conv_id,
            logical_id,
            author,
            recipient,
            created_at_unix,
            expires_at_unix,
            kind,
            body,
            signature: [0u8; 64],
        };
        let sig = identity.sign(&rec.signing_payload());
        rec.signature = sig.to_bytes();
        Ok(rec)
    }

    /// Verify version, conversation participant binding, expiry, horizon, and Ed25519 signature.
    pub fn verify_for_conversation(
        &self,
        local: &PeerId,
        remote: &PeerId,
        local_horizon_unix: u64,
        now_unix: u64,
    ) -> Result<()> {
        if self.version != SYNC_RECORD_VERSION {
            return Err(Error::Crypto(format!(
                "Unsupported SyncRecord version: {}",
                self.version
            )));
        }
        let expected_conv = conversation_id(local, remote);
        if self.conv_id != expected_conv {
            return Err(Error::Crypto(
                "SyncRecord conv_id does not match active conversation".into(),
            ));
        }
        let valid_pair = (self.author == *local && self.recipient == *remote)
            || (self.author == *remote && self.recipient == *local);
        if !valid_pair || self.author == self.recipient {
            return Err(Error::Crypto(
                "SyncRecord participants do not belong to active conversation".into(),
            ));
        }
        if self.body.len() > MAX_SYNC_BODY_BYTES {
            return Err(Error::Crypto(
                "SyncRecord body exceeds maximum bound".into(),
            ));
        }
        if self.kind == SyncRecordKind::Tombstone && !self.body.is_empty() {
            return Err(Error::Crypto(
                "Tombstone SyncRecord must not contain message body".into(),
            ));
        }
        if self.kind == SyncRecordKind::Message && self.body.is_empty() {
            return Err(Error::Crypto("Message SyncRecord cannot be empty".into()));
        }
        if self.expires_at_unix <= self.created_at_unix || now_unix >= self.expires_at_unix {
            return Err(Error::Crypto("Expired SyncRecord rejected".into()));
        }
        if self.created_at_unix < local_horizon_unix {
            return Err(Error::Crypto(
                "SyncRecord older than local deletion horizon rejected (anti-resurrection)".into(),
            ));
        }

        let sig = Signature::from_bytes(&self.signature);
        Identity::verify(&self.author, &self.signing_payload(), &sig).map_err(|e| {
            Error::Crypto(format!(
                "SyncRecord original author Ed25519 verification failed: {e}"
            ))
        })?;
        Ok(())
    }

    /// Encode canonical wire representation with exact length prefix.
    pub fn encode_wire(&self) -> Result<Vec<u8>> {
        if self.body.len() > MAX_SYNC_BODY_BYTES {
            return Err(Error::Crypto("SyncRecord body exceeds wire bound".into()));
        }
        let total = 1 + 32 + 16 + 32 + 32 + 8 + 8 + 1 + 2 + 64 + self.body.len();
        let _ = RECORD_WIRE_HEADER_LEN; // Compile-time documentation anchor
        let mut out = Vec::with_capacity(total);
        out.push(self.version);
        out.extend_from_slice(&self.conv_id);
        out.extend_from_slice(&self.logical_id);
        out.extend_from_slice(self.author.as_bytes());
        out.extend_from_slice(self.recipient.as_bytes());
        out.extend_from_slice(&self.created_at_unix.to_be_bytes());
        out.extend_from_slice(&self.expires_at_unix.to_be_bytes());
        out.push(self.kind as u8);
        out.extend_from_slice(&(self.body.len() as u16).to_be_bytes());
        out.extend_from_slice(&self.signature);
        out.extend_from_slice(&self.body);
        Ok(out)
    }

    /// Decode a canonical wire record, validating `body_len <= MAX_SYNC_BODY_BYTES` before allocating.
    pub fn decode_wire(buf: &[u8]) -> Result<(Self, usize)> {
        const FIXED: usize = 1 + 32 + 16 + 32 + 32 + 8 + 8 + 1 + 2 + 64;
        if buf.len() < FIXED {
            return Err(Error::Crypto(format!(
                "SyncRecord buffer truncated: {} < {FIXED}",
                buf.len()
            )));
        }
        let version = buf[0];
        if version != SYNC_RECORD_VERSION {
            return Err(Error::Crypto(format!(
                "Unsupported SyncRecord wire version: {version}"
            )));
        }
        let mut conv_id = [0u8; 32];
        conv_id.copy_from_slice(&buf[1..33]);
        let mut logical_id = [0u8; 16];
        logical_id.copy_from_slice(&buf[33..49]);
        let mut author = [0u8; 32];
        author.copy_from_slice(&buf[49..81]);
        let mut recipient = [0u8; 32];
        recipient.copy_from_slice(&buf[81..113]);
        let mut ts_buf = [0u8; 8];
        ts_buf.copy_from_slice(&buf[113..121]);
        let created_at_unix = u64::from_be_bytes(ts_buf);
        ts_buf.copy_from_slice(&buf[121..129]);
        let expires_at_unix = u64::from_be_bytes(ts_buf);
        let kind = SyncRecordKind::from_u8(buf[129])?;
        let body_len = u16::from_be_bytes([buf[130], buf[131]]) as usize;
        if body_len > MAX_SYNC_BODY_BYTES {
            return Err(Error::Crypto(format!(
                "SyncRecord body_len {body_len} exceeds bound {MAX_SYNC_BODY_BYTES}"
            )));
        }
        let total = FIXED + body_len;
        if buf.len() < total {
            return Err(Error::Crypto("Truncated SyncRecord body".into()));
        }
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&buf[132..196]);
        let body = buf[196..total].to_vec();

        Ok((
            Self {
                version,
                conv_id,
                logical_id,
                author: PeerId(author),
                recipient: PeerId(recipient),
                created_at_unix,
                expires_at_unix,
                kind,
                body,
                signature,
            },
            total,
        ))
    }
}
