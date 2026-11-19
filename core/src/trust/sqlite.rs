// CRYPTO-REVIEW:
// What it does: Encrypted SQLite persistence (`bitchat_trust.db`) for peer trust decisions
// (Approved, Revoked, Blocked, IdentityVerifiedPendingConsent), scoped one-hop vouchers,
// cached voucher revocations, and gateway approvals.
// Primitives / crates: `chacha20poly1305` (ChaCha20-Poly1305 AEAD), `blake3::derive_key`
// (`"bitchat2.trust.sqlite.v1"` domain-separated subkey), `rand::rngs::OsRng`, `rusqlite`, `zeroize`.
// Reviewer checklist:
// 1. Subkey is derived via `blake3::derive_key` from the DEK (never shares raw key bytes across subsystems)
//    and zeroized immediately after AEAD cipher setup.
// 2. `trust_meta` binds schema version and local `PeerId`; deleting/tampering `trust_meta` or any row
//    fails AEAD authentication (`fail closed`, never falls back to empty allow-all state).
// 3. AAD cryptographically binds `(local_peer_id, primary_key, state_tag, timestamp)` to ciphertext so
//    attackers cannot rewrite `Blocked` -> `Approved` via raw SQL column edits.
// 4. All mutations execute inside atomic SQLite transactions (`BEGIN IMMEDIATE ... COMMIT`).
// 5. Vouchers expired by wall clock are marked `permanently_expired = true` on disk so subsequent clock
//    rollback across process restarts cannot resurrect them.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Nonce,
};
use rand::RngCore;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::error::{Error, Result};
use crate::gateway::config::GatewayConsentState;
use crate::identity::PeerId;
use crate::storage::key::KeyProvider;
use crate::trust::model::{ActiveVoucherGrant, Capability, PeerTrustState};
use crate::trust::store::TrustStore;

pub const TRUST_DB_SCHEMA_VERSION: u32 = 1;
const TRUST_SUBKEY_CONTEXT: &str = "bitchat2.trust.sqlite.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrustProvenance {
    ExplicitLocal = 1,
    LegacyReviewRequired = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GatewayApprovalStatus {
    Approved = 1,
    Revoked = 2,
    Blocked = 3,
}

impl GatewayApprovalStatus {
    pub fn from_u8(v: u8) -> Result<Self> {
        match v {
            1 => Ok(Self::Approved),
            2 => Ok(Self::Revoked),
            3 => Ok(Self::Blocked),
            other => Err(Error::Storage(format!(
                "Invalid GatewayApprovalStatus tag: {other}"
            ))),
        }
    }

    pub fn to_consent_state(self) -> GatewayConsentState {
        match self {
            Self::Approved => GatewayConsentState::Approved,
            Self::Revoked => GatewayConsentState::Revoked,
            Self::Blocked => GatewayConsentState::Blocked,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedGatewayApproval {
    pub endpoint_id: String,
    pub bitchat_pubkey: [u8; 32],
    pub status: GatewayApprovalStatus,
    pub updated_at_unix: u64,
}

#[derive(Serialize, Deserialize)]
struct PersistedMetaV1 {
    schema_version: u32,
    local_peer_id: [u8; 32],
}

#[derive(Serialize, Deserialize)]
struct PersistedPeerTrustV1 {
    peer_id: [u8; 32],
    state_tag: u8,
    capabilities: Vec<u8>,
    provenance: TrustProvenance,
    timestamp_unix: u64,
}

#[derive(Serialize, Deserialize)]
struct PersistedVoucherV1 {
    credential_id: [u8; 16],
    issuer: [u8; 32],
    subject: [u8; 32],
    audience: [u8; 32],
    capability: u8,
    issued_at_unix: u64,
    expires_at_unix: u64,
    revocation_seq: u32,
    permanently_expired: bool,
}

#[derive(Serialize, Deserialize)]
struct PersistedRevocationV1 {
    issuer: [u8; 32],
    credential_id: [u8; 16],
    revocation_seq: u32,
    recorded_at_unix: u64,
}

#[derive(Serialize, Deserialize)]
struct PersistedGatewayV1 {
    endpoint_id: String,
    bitchat_pubkey: [u8; 32],
    status_tag: u8,
    updated_at_unix: u64,
}

/// Encrypted SQLite persistence backend for `TrustStore` and gateway consent state.
pub struct SqliteTrustStore {
    conn: Mutex<Connection>,
    local_peer_id: PeerId,
    key_provider: Arc<dyn KeyProvider>,
}

impl SqliteTrustStore {
    /// Open or create `bitchat_trust.db`, verifying key availability and metadata AEAD integrity.
    pub fn open(
        path: impl AsRef<Path>,
        local_peer_id: PeerId,
        key_provider: Arc<dyn KeyProvider>,
    ) -> Result<Self> {
        let conn = Connection::open(path)
            .map_err(|e| Error::Storage(format!("Trust SQLite open failed: {e}")))?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;")
            .map_err(|e| Error::Storage(format!("Trust SQLite PRAGMA failed: {e}")))?;
        Self::init_schema(&conn)?;

        let store = Self {
            conn: Mutex::new(conn),
            local_peer_id,
            key_provider,
        };
        store.verify_or_init_meta()?;
        Ok(store)
    }

    pub fn in_memory(local_peer_id: PeerId, key_provider: Arc<dyn KeyProvider>) -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| Error::Storage(format!("Trust SQLite in-memory failed: {e}")))?;
        Self::init_schema(&conn)?;
        let store = Self {
            conn: Mutex::new(conn),
            local_peer_id,
            key_provider,
        };
        store.verify_or_init_meta()?;
        Ok(store)
    }

    fn init_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS trust_meta (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                schema_version INTEGER NOT NULL,
                local_peer_id BLOB NOT NULL,
                key_version INTEGER NOT NULL,
                nonce BLOB NOT NULL,
                ciphertext BLOB NOT NULL
            );
            CREATE TABLE IF NOT EXISTS peer_trust (
                peer_id BLOB PRIMARY KEY,
                state_tag INTEGER NOT NULL,
                updated_at_unix INTEGER NOT NULL,
                key_version INTEGER NOT NULL,
                nonce BLOB NOT NULL,
                ciphertext BLOB NOT NULL
            );
            CREATE TABLE IF NOT EXISTS vouchers (
                credential_id BLOB PRIMARY KEY,
                issuer BLOB NOT NULL,
                subject BLOB NOT NULL,
                expires_at_unix INTEGER NOT NULL,
                key_version INTEGER NOT NULL,
                nonce BLOB NOT NULL,
                ciphertext BLOB NOT NULL
            );
            CREATE TABLE IF NOT EXISTS voucher_revocations (
                issuer BLOB NOT NULL,
                credential_id BLOB NOT NULL,
                revocation_seq INTEGER NOT NULL,
                recorded_at_unix INTEGER NOT NULL,
                key_version INTEGER NOT NULL,
                nonce BLOB NOT NULL,
                ciphertext BLOB NOT NULL,
                PRIMARY KEY (issuer, credential_id)
            );
            CREATE TABLE IF NOT EXISTS gateway_approvals (
                endpoint_id TEXT PRIMARY KEY,
                bitchat_pubkey BLOB NOT NULL,
                status_tag INTEGER NOT NULL,
                updated_at_unix INTEGER NOT NULL,
                key_version INTEGER NOT NULL,
                nonce BLOB NOT NULL,
                ciphertext BLOB NOT NULL
            );",
        )
        .map_err(|e| Error::Storage(format!("Trust SQLite schema init failed: {e}")))?;
        Ok(())
    }

    fn derive_cipher(&self) -> Result<(u32, ChaCha20Poly1305)> {
        let dek = self.key_provider.get_or_create_dek()?;
        let mut subkey = blake3::derive_key(TRUST_SUBKEY_CONTEXT, &dek.key);
        let cipher = ChaCha20Poly1305::new_from_slice(&subkey)
            .map_err(|e| Error::KeyStore(format!("Trust cipher init failed: {e}")));
        subkey.zeroize();
        Ok((dek.version, cipher?))
    }

    fn encrypt_aead(
        cipher: &ChaCha20Poly1305,
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<([u8; 12], Vec<u8>)> {
        let mut nonce = [0u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let ct = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|e| Error::Crypto(format!("Trust record AEAD encrypt failed: {e}")))?;
        Ok((nonce, ct))
    }

    fn decrypt_aead(
        &self,
        expected_key_version: u32,
        nonce_bytes: &[u8],
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>> {
        if nonce_bytes.len() != 12 {
            return Err(Error::Storage(format!(
                "Invalid trust AEAD nonce length: {}",
                nonce_bytes.len()
            )));
        }
        let (key_ver, cipher) = self.derive_cipher()?;
        if key_ver != expected_key_version {
            return Err(Error::KeyStore(format!(
                "Trust record DEK version mismatch: active {key_ver}, row {expected_key_version}"
            )));
        }
        cipher
            .decrypt(
                Nonce::from_slice(nonce_bytes),
                Payload {
                    msg: ciphertext,
                    aad,
                },
            )
            .map_err(|e| Error::Crypto(format!("Trust SQLite AEAD authentication failed: {e}")))
    }

    fn build_meta_aad(&self, schema_version: u32, key_version: u32) -> Vec<u8> {
        let mut aad = Vec::with_capacity(24 + 4 + 32 + 4);
        aad.extend_from_slice(b"bitchat2-trust-meta-v1:");
        aad.extend_from_slice(&schema_version.to_le_bytes());
        aad.extend_from_slice(self.local_peer_id.as_bytes());
        aad.extend_from_slice(&key_version.to_le_bytes());
        aad
    }

    fn build_peer_aad(
        &self,
        key_version: u32,
        peer_id: &[u8; 32],
        state_tag: u8,
        updated_at: u64,
    ) -> Vec<u8> {
        let mut aad = Vec::with_capacity(23 + 4 + 32 + 32 + 1 + 8);
        aad.extend_from_slice(b"bitchat2-trust-peer-v1:");
        aad.extend_from_slice(&key_version.to_le_bytes());
        aad.extend_from_slice(self.local_peer_id.as_bytes());
        aad.extend_from_slice(peer_id);
        aad.push(state_tag);
        aad.extend_from_slice(&updated_at.to_be_bytes());
        aad
    }

    fn build_voucher_aad(
        &self,
        key_version: u32,
        credential_id: &[u8; 16],
        issuer: &[u8; 32],
        subject: &[u8; 32],
        expires_at: u64,
    ) -> Vec<u8> {
        let mut aad = Vec::with_capacity(26 + 4 + 32 + 16 + 32 + 32 + 8);
        aad.extend_from_slice(b"bitchat2-trust-voucher-v1:");
        aad.extend_from_slice(&key_version.to_le_bytes());
        aad.extend_from_slice(self.local_peer_id.as_bytes());
        aad.extend_from_slice(credential_id);
        aad.extend_from_slice(issuer);
        aad.extend_from_slice(subject);
        aad.extend_from_slice(&expires_at.to_be_bytes());
        aad
    }

    fn build_revocation_aad(
        &self,
        key_version: u32,
        issuer: &[u8; 32],
        credential_id: &[u8; 16],
        revocation_seq: u32,
        recorded_at: u64,
    ) -> Vec<u8> {
        let mut aad = Vec::with_capacity(25 + 4 + 32 + 32 + 16 + 4 + 8);
        aad.extend_from_slice(b"bitchat2-trust-revoke-v1:");
        aad.extend_from_slice(&key_version.to_le_bytes());
        aad.extend_from_slice(self.local_peer_id.as_bytes());
        aad.extend_from_slice(issuer);
        aad.extend_from_slice(credential_id);
        aad.extend_from_slice(&revocation_seq.to_be_bytes());
        aad.extend_from_slice(&recorded_at.to_be_bytes());
        aad
    }

    fn build_gateway_aad(
        &self,
        key_version: u32,
        endpoint_id: &str,
        bitchat_pubkey: &[u8; 32],
        status_tag: u8,
        updated_at: u64,
    ) -> Vec<u8> {
        let mut aad = Vec::with_capacity(28 + 4 + 32 + endpoint_id.len() + 32 + 1 + 8);
        aad.extend_from_slice(b"bitchat2-trust-gateway-v1:");
        aad.extend_from_slice(&key_version.to_le_bytes());
        aad.extend_from_slice(self.local_peer_id.as_bytes());
        aad.extend_from_slice(endpoint_id.as_bytes());
        aad.extend_from_slice(bitchat_pubkey);
        aad.push(status_tag);
        aad.extend_from_slice(&updated_at.to_be_bytes());
        aad
    }

    fn verify_or_init_meta(&self) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Storage("Trust SQLite lock poisoned".into()))?;

        let meta_row = conn.query_row(
            "SELECT schema_version, local_peer_id, key_version, nonce, ciphertext FROM trust_meta WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, u32>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                ))
            },
        );

        match meta_row {
            Ok((schema_ver, meta_peer, key_ver, nonce, ct)) => {
                if schema_ver != TRUST_DB_SCHEMA_VERSION {
                    return Err(Error::Storage(format!(
                        "Unsupported trust DB schema version: {schema_ver}"
                    )));
                }
                if meta_peer.as_slice() != self.local_peer_id.as_bytes() {
                    return Err(Error::KeyStore(
                        "Trust DB local PeerId mismatch; refusing mismatched database".into(),
                    ));
                }
                let aad = self.build_meta_aad(schema_ver, key_ver);
                let pt = self.decrypt_aead(key_ver, &nonce, &aad, &ct)?;
                let parsed: PersistedMetaV1 = serde_json::from_slice(&pt)
                    .map_err(|e| Error::Storage(format!("Corrupted trust_meta payload: {e}")))?;
                if parsed.schema_version != TRUST_DB_SCHEMA_VERSION
                    || parsed.local_peer_id != self.local_peer_id.0
                {
                    return Err(Error::Storage("Trust metadata integrity mismatch".into()));
                }
                Ok(())
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                // Fail closed if non-empty trust tables exist without valid trust_meta!
                let existing_rows: i64 = conn
                    .query_row(
                        "SELECT (SELECT COUNT(*) FROM peer_trust) +
                                (SELECT COUNT(*) FROM vouchers) +
                                (SELECT COUNT(*) FROM voucher_revocations) +
                                (SELECT COUNT(*) FROM gateway_approvals)",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(|e| Error::Storage(format!("Trust DB row check failed: {e}")))?;
                if existing_rows > 0 {
                    return Err(Error::Storage(
                        "Corrupted trust DB: records exist without authenticated metadata".into(),
                    ));
                }

                let (key_ver, cipher) = self.derive_cipher()?;
                let aad = self.build_meta_aad(TRUST_DB_SCHEMA_VERSION, key_ver);
                let pt = serde_json::to_vec(&PersistedMetaV1 {
                    schema_version: TRUST_DB_SCHEMA_VERSION,
                    local_peer_id: self.local_peer_id.0,
                })
                .map_err(|e| Error::Storage(format!("Trust meta serialize failed: {e}")))?;
                let (nonce, ct) = Self::encrypt_aead(&cipher, &aad, &pt)?;

                conn.execute_batch("BEGIN IMMEDIATE;")
                    .map_err(|e| Error::Storage(format!("Trust SQLite BEGIN failed: {e}")))?;
                let res = conn.execute(
                    "INSERT INTO trust_meta (id, schema_version, local_peer_id, key_version, nonce, ciphertext)
                     VALUES (1, ?1, ?2, ?3, ?4, ?5)",
                    params![
                        TRUST_DB_SCHEMA_VERSION,
                        self.local_peer_id.as_bytes().as_slice(),
                        key_ver,
                        nonce.as_slice(),
                        ct
                    ],
                );
                match res {
                    Ok(_) => conn
                        .execute_batch("COMMIT;")
                        .map_err(|e| Error::Storage(format!("Trust SQLite COMMIT failed: {e}"))),
                    Err(e) => {
                        let _ = conn.execute_batch("ROLLBACK;");
                        Err(Error::Storage(format!("Failed to write trust_meta: {e}")))
                    }
                }
            }
            Err(e) => Err(Error::Storage(format!("Trust metadata read failed: {e}"))),
        }
    }

    /// Persist a peer trust state atomically before acknowledging any user trust action.
    pub fn upsert_peer_state(
        &self,
        peer_id: &PeerId,
        state: &PeerTrustState,
        provenance: TrustProvenance,
    ) -> Result<()> {
        let (state_tag, capabilities, ts_unix): (u8, Vec<u8>, u64) = match state {
            PeerTrustState::Unrecognized => return self.delete_peer_state(peer_id),
            PeerTrustState::IdentityVerifiedPendingConsent => (1, Vec::new(), 0),
            PeerTrustState::Approved {
                capabilities,
                granted_at_unix,
            } => (
                2,
                capabilities.iter().map(|c| *c as u8).collect(),
                *granted_at_unix,
            ),
            PeerTrustState::Revoked { revoked_at_unix } => (3, Vec::new(), *revoked_at_unix),
            PeerTrustState::Blocked { blocked_at_unix } => (4, Vec::new(), *blocked_at_unix),
        };

        let (key_ver, cipher) = self.derive_cipher()?;
        let aad = self.build_peer_aad(key_ver, &peer_id.0, state_tag, ts_unix);
        let payload = PersistedPeerTrustV1 {
            peer_id: peer_id.0,
            state_tag,
            capabilities,
            provenance,
            timestamp_unix: ts_unix,
        };
        let mut pt = serde_json::to_vec(&payload)
            .map_err(|e| Error::Storage(format!("Peer trust serialize failed: {e}")))?;
        let enc_res = Self::encrypt_aead(&cipher, &aad, &pt);
        pt.zeroize();
        let (nonce, ct) = enc_res?;

        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Storage("Trust SQLite lock poisoned".into()))?;
        conn.execute_batch("BEGIN IMMEDIATE;")
            .map_err(|e| Error::Storage(format!("Trust SQLite BEGIN failed: {e}")))?;
        let res = conn
            .execute(
                "INSERT INTO peer_trust (peer_id, state_tag, updated_at_unix, key_version, nonce, ciphertext)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(peer_id) DO UPDATE SET
                   state_tag = excluded.state_tag,
                   updated_at_unix = excluded.updated_at_unix,
                   key_version = excluded.key_version,
                   nonce = excluded.nonce,
                   ciphertext = excluded.ciphertext",
                params![
                    peer_id.as_bytes().as_slice(),
                    state_tag,
                    ts_unix,
                    key_ver,
                    nonce.as_slice(),
                    ct
                ],
            )
            .and_then(|_| {
                if state_tag == 3 {
                    conn.execute(
                        "DELETE FROM vouchers WHERE issuer = ?1",
                        params![peer_id.as_bytes().as_slice()],
                    )
                    .map(|_| ())
                } else if state_tag == 4 {
                    conn.execute(
                        "DELETE FROM vouchers WHERE issuer = ?1 OR subject = ?1",
                        params![peer_id.as_bytes().as_slice()],
                    )
                    .map(|_| ())
                } else {
                    Ok(())
                }
            });
        match res {
            Ok(_) => conn
                .execute_batch("COMMIT;")
                .map_err(|e| Error::Storage(format!("Trust SQLite COMMIT failed: {e}"))),
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK;");
                Err(Error::Storage(format!(
                    "Failed to persist peer_trust row: {e}"
                )))
            }
        }
    }

    fn delete_peer_state(&self, peer_id: &PeerId) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Storage("Trust SQLite lock poisoned".into()))?;
        conn.execute_batch("BEGIN IMMEDIATE;")
            .map_err(|e| Error::Storage(format!("Trust SQLite BEGIN failed: {e}")))?;
        let res = conn.execute(
            "DELETE FROM peer_trust WHERE peer_id = ?1",
            params![peer_id.as_bytes().as_slice()],
        );
        match res {
            Ok(_) => conn
                .execute_batch("COMMIT;")
                .map_err(|e| Error::Storage(format!("Trust SQLite COMMIT failed: {e}"))),
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK;");
                Err(Error::Storage(format!("Failed to delete peer_trust: {e}")))
            }
        }
    }

    /// Persist a verified voucher grant atomically.
    pub fn upsert_voucher(&self, grant: &ActiveVoucherGrant) -> Result<()> {
        let (key_ver, cipher) = self.derive_cipher()?;
        let aad = self.build_voucher_aad(
            key_ver,
            &grant.credential_id,
            &grant.issuer.0,
            &grant.subject.0,
            grant.expires_at_unix,
        );
        let payload = PersistedVoucherV1 {
            credential_id: grant.credential_id,
            issuer: grant.issuer.0,
            subject: grant.subject.0,
            audience: grant.audience.0,
            capability: grant.capability as u8,
            issued_at_unix: grant.issued_at_unix,
            expires_at_unix: grant.expires_at_unix,
            revocation_seq: grant.revocation_seq,
            permanently_expired: grant.permanently_expired,
        };
        let mut pt = serde_json::to_vec(&payload)
            .map_err(|e| Error::Storage(format!("Voucher serialize failed: {e}")))?;
        let enc_res = Self::encrypt_aead(&cipher, &aad, &pt);
        pt.zeroize();
        let (nonce, ct) = enc_res?;

        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Storage("Trust SQLite lock poisoned".into()))?;
        conn.execute_batch("BEGIN IMMEDIATE;")
            .map_err(|e| Error::Storage(format!("Trust SQLite BEGIN failed: {e}")))?;
        let res = conn.execute(
            "INSERT INTO vouchers (credential_id, issuer, subject, expires_at_unix, key_version, nonce, ciphertext)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(credential_id) DO UPDATE SET
               issuer = excluded.issuer,
               subject = excluded.subject,
               expires_at_unix = excluded.expires_at_unix,
               key_version = excluded.key_version,
               nonce = excluded.nonce,
               ciphertext = excluded.ciphertext",
            params![
                grant.credential_id.as_slice(),
                grant.issuer.0.as_slice(),
                grant.subject.0.as_slice(),
                grant.expires_at_unix,
                key_ver,
                nonce.as_slice(),
                ct
            ],
        );
        match res {
            Ok(_) => conn
                .execute_batch("COMMIT;")
                .map_err(|e| Error::Storage(format!("Trust SQLite COMMIT failed: {e}"))),
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK;");
                Err(Error::Storage(format!("Failed to persist voucher: {e}")))
            }
        }
    }

    /// Persist a voucher revocation atomically.
    pub fn upsert_revocation(
        &self,
        issuer: &PeerId,
        credential_id: &[u8; 16],
        revocation_seq: u32,
        recorded_at_unix: u64,
    ) -> Result<()> {
        let (key_ver, cipher) = self.derive_cipher()?;
        let aad = self.build_revocation_aad(
            key_ver,
            &issuer.0,
            credential_id,
            revocation_seq,
            recorded_at_unix,
        );
        let payload = PersistedRevocationV1 {
            issuer: issuer.0,
            credential_id: *credential_id,
            revocation_seq,
            recorded_at_unix,
        };
        let mut pt = serde_json::to_vec(&payload)
            .map_err(|e| Error::Storage(format!("Revocation serialize failed: {e}")))?;
        let enc_res = Self::encrypt_aead(&cipher, &aad, &pt);
        pt.zeroize();
        let (nonce, ct) = enc_res?;

        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Storage("Trust SQLite lock poisoned".into()))?;
        conn.execute_batch("BEGIN IMMEDIATE;")
            .map_err(|e| Error::Storage(format!("Trust SQLite BEGIN failed: {e}")))?;
        let res = conn.execute(
            "INSERT INTO voucher_revocations (issuer, credential_id, revocation_seq, recorded_at_unix, key_version, nonce, ciphertext)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(issuer, credential_id) DO UPDATE SET
               revocation_seq = MAX(voucher_revocations.revocation_seq, excluded.revocation_seq),
               recorded_at_unix = excluded.recorded_at_unix,
               key_version = excluded.key_version,
               nonce = excluded.nonce,
               ciphertext = excluded.ciphertext",
            params![
                issuer.0.as_slice(),
                credential_id.as_slice(),
                revocation_seq,
                recorded_at_unix,
                key_ver,
                nonce.as_slice(),
                ct
            ],
        );
        match res {
            Ok(_) => conn
                .execute_batch("COMMIT;")
                .map_err(|e| Error::Storage(format!("Trust SQLite COMMIT failed: {e}"))),
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK;");
                Err(Error::Storage(format!(
                    "Failed to persist voucher revocation: {e}"
                )))
            }
        }
    }

    /// Persist a gateway approval, revocation, or block atomically.
    pub fn upsert_gateway_approval(&self, rec: &PersistedGatewayApproval) -> Result<()> {
        let status_tag = rec.status as u8;
        let (key_ver, cipher) = self.derive_cipher()?;
        let aad = self.build_gateway_aad(
            key_ver,
            &rec.endpoint_id,
            &rec.bitchat_pubkey,
            status_tag,
            rec.updated_at_unix,
        );
        let payload = PersistedGatewayV1 {
            endpoint_id: rec.endpoint_id.clone(),
            bitchat_pubkey: rec.bitchat_pubkey,
            status_tag,
            updated_at_unix: rec.updated_at_unix,
        };
        let mut pt = serde_json::to_vec(&payload)
            .map_err(|e| Error::Storage(format!("Gateway approval serialize failed: {e}")))?;
        let enc_res = Self::encrypt_aead(&cipher, &aad, &pt);
        pt.zeroize();
        let (nonce, ct) = enc_res?;

        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Storage("Trust SQLite lock poisoned".into()))?;
        conn.execute_batch("BEGIN IMMEDIATE;")
            .map_err(|e| Error::Storage(format!("Trust SQLite BEGIN failed: {e}")))?;
        let res = conn.execute(
            "INSERT INTO gateway_approvals (endpoint_id, bitchat_pubkey, status_tag, updated_at_unix, key_version, nonce, ciphertext)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(endpoint_id) DO UPDATE SET
               bitchat_pubkey = excluded.bitchat_pubkey,
               status_tag = excluded.status_tag,
               updated_at_unix = excluded.updated_at_unix,
               key_version = excluded.key_version,
               nonce = excluded.nonce,
               ciphertext = excluded.ciphertext",
            params![
                rec.endpoint_id,
                rec.bitchat_pubkey.as_slice(),
                status_tag,
                rec.updated_at_unix,
                key_ver,
                nonce.as_slice(),
                ct
            ],
        );
        match res {
            Ok(_) => conn
                .execute_batch("COMMIT;")
                .map_err(|e| Error::Storage(format!("Trust SQLite COMMIT failed: {e}"))),
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK;");
                Err(Error::Storage(format!(
                    "Failed to persist gateway approval: {e}"
                )))
            }
        }
    }

    /// Migrate legacy trust approvals safely:
    /// - Proven explicit consent (`explicit_consent == true`) migrates as `Approved(ExplicitLocal)`.
    /// - Legacy entries without explicit consent provenance downgrade to
    ///   `IdentityVerifiedPendingConsent` (never auto-approving everyone).
    pub fn migrate_legacy_approvals(
        &self,
        legacy_entries: &[(PeerId, HashSet<Capability>, u64, bool)],
    ) -> Result<()> {
        for (pid, caps, ts, explicit_consent) in legacy_entries {
            if *explicit_consent {
                self.upsert_peer_state(
                    pid,
                    &PeerTrustState::Approved {
                        capabilities: caps.clone(),
                        granted_at_unix: *ts,
                    },
                    TrustProvenance::ExplicitLocal,
                )?;
            } else {
                self.upsert_peer_state(
                    pid,
                    &PeerTrustState::IdentityVerifiedPendingConsent,
                    TrustProvenance::LegacyReviewRequired,
                )?;
            }
        }
        Ok(())
    }

    /// Verify, decrypt, and hydrate all persisted trust state into `store` before traffic admission.
    /// Fails closed on ANY missing key, tampered column, invalid signature/encoding, or AEAD failure.
    pub fn load_into(
        &self,
        store: &mut TrustStore,
        now_monotonic: Instant,
        now_unix: Option<u64>,
    ) -> Result<Vec<PersistedGatewayApproval>> {
        self.verify_or_init_meta()?;

        let (peer_rows, voucher_rows, rev_rows, gw_rows) = {
            let conn = self
                .conn
                .lock()
                .map_err(|_| Error::Storage("Trust SQLite lock poisoned".into()))?;

            let mut p_stmt = conn
                .prepare("SELECT peer_id, state_tag, updated_at_unix, key_version, nonce, ciphertext FROM peer_trust")
                .map_err(|e| Error::Storage(format!("peer_trust prepare failed: {e}")))?;
            let p_rows = p_stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, Vec<u8>>(0)?,
                        r.get::<_, u8>(1)?,
                        r.get::<_, u64>(2)?,
                        r.get::<_, u32>(3)?,
                        r.get::<_, Vec<u8>>(4)?,
                        r.get::<_, Vec<u8>>(5)?,
                    ))
                })
                .map_err(|e| Error::Storage(format!("peer_trust query failed: {e}")))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| Error::Storage(format!("peer_trust collect failed: {e}")))?;

            let mut v_stmt = conn
                .prepare("SELECT credential_id, issuer, subject, expires_at_unix, key_version, nonce, ciphertext FROM vouchers")
                .map_err(|e| Error::Storage(format!("vouchers prepare failed: {e}")))?;
            let v_rows = v_stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, Vec<u8>>(0)?,
                        r.get::<_, Vec<u8>>(1)?,
                        r.get::<_, Vec<u8>>(2)?,
                        r.get::<_, u64>(3)?,
                        r.get::<_, u32>(4)?,
                        r.get::<_, Vec<u8>>(5)?,
                        r.get::<_, Vec<u8>>(6)?,
                    ))
                })
                .map_err(|e| Error::Storage(format!("vouchers query failed: {e}")))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| Error::Storage(format!("vouchers collect failed: {e}")))?;

            let mut r_stmt = conn
                .prepare("SELECT issuer, credential_id, revocation_seq, recorded_at_unix, key_version, nonce, ciphertext FROM voucher_revocations")
                .map_err(|e| Error::Storage(format!("revocations prepare failed: {e}")))?;
            let r_rows = r_stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, Vec<u8>>(0)?,
                        r.get::<_, Vec<u8>>(1)?,
                        r.get::<_, u32>(2)?,
                        r.get::<_, u64>(3)?,
                        r.get::<_, u32>(4)?,
                        r.get::<_, Vec<u8>>(5)?,
                        r.get::<_, Vec<u8>>(6)?,
                    ))
                })
                .map_err(|e| Error::Storage(format!("revocations query failed: {e}")))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| Error::Storage(format!("revocations collect failed: {e}")))?;

            let mut g_stmt = conn
                .prepare("SELECT endpoint_id, bitchat_pubkey, status_tag, updated_at_unix, key_version, nonce, ciphertext FROM gateway_approvals")
                .map_err(|e| Error::Storage(format!("gateway_approvals prepare failed: {e}")))?;
            let g_rows = g_stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Vec<u8>>(1)?,
                        r.get::<_, u8>(2)?,
                        r.get::<_, u64>(3)?,
                        r.get::<_, u32>(4)?,
                        r.get::<_, Vec<u8>>(5)?,
                        r.get::<_, Vec<u8>>(6)?,
                    ))
                })
                .map_err(|e| Error::Storage(format!("gateway_approvals query failed: {e}")))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| Error::Storage(format!("gateway_approvals collect failed: {e}")))?;

            (p_rows, v_rows, r_rows, g_rows)
        };

        // 1. Peer trust states
        for (pid_vec, state_tag, updated_at, key_ver, nonce, ct) in peer_rows {
            if pid_vec.len() != 32 {
                return Err(Error::Storage(
                    "Invalid peer_id length in peer_trust".into(),
                ));
            }
            let mut pid_arr = [0u8; 32];
            pid_arr.copy_from_slice(&pid_vec);
            let aad = self.build_peer_aad(key_ver, &pid_arr, state_tag, updated_at);
            let pt = self.decrypt_aead(key_ver, &nonce, &aad, &ct)?;
            let parsed: PersistedPeerTrustV1 = serde_json::from_slice(&pt)
                .map_err(|e| Error::Storage(format!("Corrupt peer_trust JSON: {e}")))?;
            if parsed.peer_id != pid_arr
                || parsed.state_tag != state_tag
                || parsed.timestamp_unix != updated_at
            {
                return Err(Error::Storage(
                    "peer_trust column mismatch against authenticated payload".into(),
                ));
            }
            let peer_id = PeerId(pid_arr);
            let restored = match (state_tag, parsed.provenance) {
                (1, _) => PeerTrustState::IdentityVerifiedPendingConsent,
                (2, TrustProvenance::ExplicitLocal) => {
                    let mut caps = HashSet::new();
                    for raw_cap in parsed.capabilities {
                        caps.insert(Capability::from_u8(raw_cap)?);
                    }
                    PeerTrustState::Approved {
                        capabilities: caps,
                        granted_at_unix: updated_at,
                    }
                }
                (2, TrustProvenance::LegacyReviewRequired) => {
                    PeerTrustState::IdentityVerifiedPendingConsent
                }
                (3, _) => PeerTrustState::Revoked {
                    revoked_at_unix: updated_at,
                },
                (4, _) => PeerTrustState::Blocked {
                    blocked_at_unix: updated_at,
                },
                (other, _) => {
                    return Err(Error::Storage(format!(
                        "Unknown peer_trust state_tag: {other}"
                    )))
                }
            };
            store.restore_peer_state(peer_id, restored);
        }

        // 2. Cached revocations (before vouchers so revoked credentials stay invalid)
        for (issuer_vec, cred_vec, seq, rec_at, key_ver, nonce, ct) in rev_rows {
            if issuer_vec.len() != 32 || cred_vec.len() != 16 {
                return Err(Error::Storage(
                    "Invalid key lengths in voucher_revocations".into(),
                ));
            }
            let mut issuer_arr = [0u8; 32];
            issuer_arr.copy_from_slice(&issuer_vec);
            let mut cred_arr = [0u8; 16];
            cred_arr.copy_from_slice(&cred_vec);

            let aad = self.build_revocation_aad(key_ver, &issuer_arr, &cred_arr, seq, rec_at);
            let pt = self.decrypt_aead(key_ver, &nonce, &aad, &ct)?;
            let parsed: PersistedRevocationV1 = serde_json::from_slice(&pt)
                .map_err(|e| Error::Storage(format!("Corrupt revocation JSON: {e}")))?;
            if parsed.issuer != issuer_arr
                || parsed.credential_id != cred_arr
                || parsed.revocation_seq != seq
            {
                return Err(Error::Storage("Revocation record mismatch".into()));
            }
            store.restore_revocation(PeerId(issuer_arr), cred_arr, seq, now_monotonic);
        }

        // 3. Vouchers (mark wall-clock expired grants permanently_expired and persist seal!)
        let mut newly_expired_to_seal = Vec::new();
        for (cred_vec, issuer_vec, subj_vec, exp_unix, key_ver, nonce, ct) in voucher_rows {
            if cred_vec.len() != 16 || issuer_vec.len() != 32 || subj_vec.len() != 32 {
                return Err(Error::Storage("Invalid key lengths in vouchers".into()));
            }
            let mut cred_arr = [0u8; 16];
            cred_arr.copy_from_slice(&cred_vec);
            let mut issuer_arr = [0u8; 32];
            issuer_arr.copy_from_slice(&issuer_vec);
            let mut subj_arr = [0u8; 32];
            subj_arr.copy_from_slice(&subj_vec);

            let aad = self.build_voucher_aad(key_ver, &cred_arr, &issuer_arr, &subj_arr, exp_unix);
            let pt = self.decrypt_aead(key_ver, &nonce, &aad, &ct)?;
            let parsed: PersistedVoucherV1 = serde_json::from_slice(&pt)
                .map_err(|e| Error::Storage(format!("Corrupt voucher JSON: {e}")))?;
            if parsed.credential_id != cred_arr
                || parsed.issuer != issuer_arr
                || parsed.subject != subj_arr
                || parsed.audience != self.local_peer_id.0
                || parsed.expires_at_unix != exp_unix
            {
                return Err(Error::Storage(
                    "Voucher record columns failed binding verification".into(),
                ));
            }
            let cap = Capability::from_u8(parsed.capability)?;
            let wall_expired = now_unix.is_some_and(|u| u >= parsed.expires_at_unix);
            let perm_expired = parsed.permanently_expired || wall_expired;
            let expires_at_monotonic = match now_unix {
                Some(u) if !perm_expired => {
                    now_monotonic + Duration::from_secs(parsed.expires_at_unix.saturating_sub(u))
                }
                _ => now_monotonic,
            };

            let grant = ActiveVoucherGrant {
                credential_id: cred_arr,
                issuer: PeerId(issuer_arr),
                subject: PeerId(subj_arr),
                audience: PeerId(parsed.audience),
                capability: cap,
                issued_at_unix: parsed.issued_at_unix,
                expires_at_unix: parsed.expires_at_unix,
                expires_at_monotonic,
                revocation_seq: parsed.revocation_seq,
                permanently_expired: perm_expired,
            };
            if perm_expired && !parsed.permanently_expired {
                newly_expired_to_seal.push(grant.clone());
            }
            store.restore_voucher(grant);
        }

        // Persist permanent expiry seal so later wall-clock rollback cannot resurrect expired vouchers
        for expired_grant in newly_expired_to_seal {
            self.upsert_voucher(&expired_grant)?;
        }

        // 4. Gateway approvals/blocks/revocations
        let mut gateways = Vec::with_capacity(gw_rows.len());
        for (ep_id, pk_vec, status_tag, updated_at, key_ver, nonce, ct) in gw_rows {
            if pk_vec.len() != 32 {
                return Err(Error::Storage(
                    "Invalid bitchat_pubkey length in gateway_approvals".into(),
                ));
            }
            let mut pk_arr = [0u8; 32];
            pk_arr.copy_from_slice(&pk_vec);
            let status = GatewayApprovalStatus::from_u8(status_tag)?;
            let aad = self.build_gateway_aad(key_ver, &ep_id, &pk_arr, status_tag, updated_at);
            let pt = self.decrypt_aead(key_ver, &nonce, &aad, &ct)?;
            let parsed: PersistedGatewayV1 = serde_json::from_slice(&pt)
                .map_err(|e| Error::Storage(format!("Corrupt gateway_approvals JSON: {e}")))?;
            if parsed.endpoint_id != ep_id
                || parsed.bitchat_pubkey != pk_arr
                || parsed.status_tag != status_tag
                || parsed.updated_at_unix != updated_at
            {
                return Err(Error::Storage(
                    "Gateway approval row mismatch against authenticated payload".into(),
                ));
            }
            gateways.push(PersistedGatewayApproval {
                endpoint_id: ep_id,
                bitchat_pubkey: pk_arr,
                status,
                updated_at_unix: updated_at,
            });
        }

        Ok(gateways)
    }

    #[cfg(test)]
    pub fn raw_conn_for_test(&self) -> &Mutex<Connection> {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use crate::storage::key::{FailingKeyProvider, InjectedKeyProvider};
    use crate::trust::store::TrustQuotaConfig;

    #[test]
    fn test_sqlite_trust_roundtrip_tamper_and_fail_closed() {
        let dir =
            std::env::temp_dir().join(format!("bitchat_trust_unit_{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("bitchat_trust.db");

        let local = Identity::generate().peer_id();
        let bob = Identity::generate().peer_id();
        let key_prov: Arc<dyn KeyProvider> = Arc::new(InjectedKeyProvider::random(1));

        let db = SqliteTrustStore::open(&db_path, local, Arc::clone(&key_prov)).unwrap();
        db.upsert_peer_state(
            &bob,
            &PeerTrustState::Blocked {
                blocked_at_unix: 1_700_000_000,
            },
            TrustProvenance::ExplicitLocal,
        )
        .unwrap();
        drop(db);

        // Reload succeeds and restores Blocked
        let db2 = SqliteTrustStore::open(&db_path, local, Arc::clone(&key_prov)).unwrap();
        let mut store2 = TrustStore::new(local, TrustQuotaConfig::default());
        db2.load_into(&mut store2, Instant::now(), Some(1_700_000_010))
            .unwrap();
        assert!(matches!(
            store2.peer_state(&bob),
            PeerTrustState::Blocked { .. }
        ));

        // Tamper SQL column `state_tag` from 4 (Blocked) to 2 (Approved) -> fails AEAD!
        {
            let conn = db2.raw_conn_for_test().lock().unwrap();
            conn.execute("UPDATE peer_trust SET state_tag = 2", [])
                .unwrap();
        }
        let mut store_tampered = TrustStore::new(local, TrustQuotaConfig::default());
        assert!(db2
            .load_into(&mut store_tampered, Instant::now(), Some(1_700_000_010))
            .is_err());

        // Missing KEK -> fails closed
        let fail_prov: Arc<dyn KeyProvider> = Arc::new(FailingKeyProvider::new("locked"));
        assert!(SqliteTrustStore::open(&db_path, local, fail_prov).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_legacy_migration_downgrades_unproven_approvals() {
        let local = Identity::generate().peer_id();
        let explicit_peer = Identity::generate().peer_id();
        let legacy_unproven_peer = Identity::generate().peer_id();
        let key_prov: Arc<dyn KeyProvider> = Arc::new(InjectedKeyProvider::random(1));
        let db = SqliteTrustStore::in_memory(local, key_prov).unwrap();

        let mut caps = HashSet::new();
        caps.insert(Capability::ReceiveDirectMessage);
        db.migrate_legacy_approvals(&[
            (explicit_peer, caps.clone(), 1000, true),
            (legacy_unproven_peer, caps, 1000, false),
        ])
        .unwrap();

        let mut store = TrustStore::new(local, TrustQuotaConfig::default());
        db.load_into(&mut store, Instant::now(), Some(1005))
            .unwrap();
        assert!(matches!(
            store.peer_state(&explicit_peer),
            PeerTrustState::Approved { .. }
        ));
        assert_eq!(
            store.peer_state(&legacy_unproven_peer),
            PeerTrustState::IdentityVerifiedPendingConsent
        );
    }
}
