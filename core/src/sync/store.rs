// CRYPTO-REVIEW:
// What it does: Encrypted SQLite storage (`bitchat_sync.db`) for opt-in Normal-mode
// conversation history and deletion tombstones (Default OFF).
// Primitives / crates: `chacha20poly1305`, `blake3`, `rand`, `zeroize`, `rusqlite`.
// Reviewer checklist:
// 1. Subkey derived strictly from DEK via `blake3::derive_key("bitchat2.sync.sqlite.v1", &dek.key)`
//    and wrapped in `ZeroizeOnDrop`.
// 2. Per-row ChaCha20-Poly1305 AEAD with 96-bit `OsRng` nonces and AAD binding
//    `(version, conv_id, logical_id, created_at_unix, expires_at_unix, kind)`.
// 3. `sync_meta` verifies schema version and local `PeerId`; missing/corrupt meta fails closed.
// 4. Atomic `BEGIN IMMEDIATE ... COMMIT` writes with strict `1024`-record cap per conversation.
// 5. Tombstone dominance (`Tombstone` overwrites `Message`; `Message` never overwrites `Tombstone`)
//    and `horizon_unix` anti-resurrection floor upon compaction/eviction.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::rngs::OsRng;
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{Error, Result};
use crate::identity::PeerId;
use crate::storage::KeyProvider;
use crate::sync::merkle::SyncSnapshot;
use crate::sync::record::{
    SyncRecord, SyncRecordKind, MAX_SYNC_RECORDS_PER_CONVERSATION, SYNC_RECORD_VERSION,
    TOMBSTONE_RETENTION_SECS,
};

const SYNC_SCHEMA_VERSION: i64 = 1;
const SYNC_KEY_CONTEXT: &str = "bitchat2.sync.sqlite.v1";

#[derive(Zeroize, ZeroizeOnDrop)]
struct SyncKey([u8; 32]);

fn derive_sync_key(dek: &[u8; 32]) -> SyncKey {
    SyncKey(blake3::derive_key(SYNC_KEY_CONTEXT, dek))
}

fn build_row_aad(
    conv_id: &[u8; 32],
    logical_id: &[u8; 16],
    created_at_unix: u64,
    expires_at_unix: u64,
    kind: SyncRecordKind,
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(1 + 32 + 16 + 8 + 8 + 1);
    aad.push(SYNC_RECORD_VERSION);
    aad.extend_from_slice(conv_id);
    aad.extend_from_slice(logical_id);
    aad.extend_from_slice(&created_at_unix.to_be_bytes());
    aad.extend_from_slice(&expires_at_unix.to_be_bytes());
    aad.push(kind as u8);
    aad
}

/// Encrypted SQLite persistence for bounded Normal-mode conversation archives (`bitchat_sync.db`).
pub struct SqliteSyncStore {
    conn: Mutex<Connection>,
    key: SyncKey,
    local_peer_id: PeerId,
    mutation_epoch: AtomicU64,
}

impl SqliteSyncStore {
    pub fn open(
        path: &Path,
        local_peer_id: PeerId,
        key_provider: Arc<dyn KeyProvider>,
    ) -> Result<Self> {
        let dek = key_provider
            .get_or_create_dek()
            .map_err(|e| Error::Storage(format!("SYNC_KEY_UNAVAILABLE: {e}")))?;
        let key = derive_sync_key(&dek.key);

        let conn = Connection::open(path)
            .map_err(|e| Error::Storage(format!("SYNC_DB_OPEN_FAILED: {e}")))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| Error::Storage(format!("SYNC_DB_PRAGMA_FAILED: {e}")))?;
        conn.pragma_update(None, "synchronous", "FULL")
            .map_err(|e| Error::Storage(format!("SYNC_DB_PRAGMA_FAILED: {e}")))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sync_meta (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                schema_version INTEGER NOT NULL,
                local_peer_id BLOB NOT NULL
            );
            CREATE TABLE IF NOT EXISTS sync_conversations (
                conv_id BLOB PRIMARY KEY,
                horizon_unix INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS sync_records (
                conv_id BLOB NOT NULL,
                logical_id BLOB NOT NULL,
                created_at_unix INTEGER NOT NULL,
                expires_at_unix INTEGER NOT NULL,
                kind INTEGER NOT NULL,
                ciphertext BLOB NOT NULL,
                nonce BLOB NOT NULL,
                PRIMARY KEY (conv_id, logical_id)
            );
            CREATE INDEX IF NOT EXISTS idx_sync_conv_order
                ON sync_records(conv_id, created_at_unix ASC, logical_id ASC);",
        )
        .map_err(|e| Error::Storage(format!("SYNC_DB_SCHEMA_FAILED: {e}")))?;

        let meta_row: Option<(i64, Vec<u8>)> = conn
            .query_row(
                "SELECT schema_version, local_peer_id FROM sync_meta WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| Error::Storage(format!("SYNC_DB_META_READ_FAILED: {e}")))?;

        match meta_row {
            None => {
                let count: i64 = conn
                    .query_row("SELECT COUNT(*) FROM sync_records", [], |r| r.get(0))
                    .map_err(|e| Error::Storage(format!("SYNC_DB_COUNT_FAILED: {e}")))?;
                if count > 0 {
                    return Err(Error::Storage(
                        "SYNC_DB_CORRUPT: sync_records populated without sync_meta".into(),
                    ));
                }
                conn.execute(
                    "INSERT INTO sync_meta (id, schema_version, local_peer_id) VALUES (1, ?1, ?2)",
                    params![SYNC_SCHEMA_VERSION, local_peer_id.as_bytes().as_slice()],
                )
                .map_err(|e| Error::Storage(format!("SYNC_DB_META_WRITE_FAILED: {e}")))?;
            }
            Some((ver, owner)) => {
                if ver != SYNC_SCHEMA_VERSION {
                    return Err(Error::Storage(format!(
                        "SYNC_DB_VERSION_MISMATCH: unsupported schema version {ver}"
                    )));
                }
                if owner.as_slice() != local_peer_id.as_bytes() {
                    return Err(Error::Storage(
                        "SYNC_DB_OWNER_MISMATCH: sync store belongs to a different PeerId".into(),
                    ));
                }
            }
        }

        Ok(Self {
            conn: Mutex::new(conn),
            key,
            local_peer_id,
            mutation_epoch: AtomicU64::new(0),
        })
    }

    pub fn local_peer_id(&self) -> PeerId {
        self.local_peer_id
    }

    /// O(1) mutation stamp detecting both in-process and external SQLite changes without decrypting rows.
    pub fn change_stamp(&self) -> Result<(u64, i64)> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Storage("Sync DB mutex poisoned".into()))?;
        let dv: i64 = conn
            .pragma_query_value(None, "data_version", |r| r.get(0))
            .map_err(|e| Error::Storage(format!("SYNC_DATA_VERSION_FAILED: {e}")))?;
        Ok((self.mutation_epoch.load(Ordering::Acquire), dv))
    }

    fn encrypt_record(&self, rec: &SyncRecord) -> Result<(Vec<u8>, [u8; 12])> {
        let plain = rec.encode_wire()?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.key.0));
        let mut nonce = [0u8; 12];
        OsRng.fill_bytes(&mut nonce);
        let aad = build_row_aad(
            &rec.conv_id,
            &rec.logical_id,
            rec.created_at_unix,
            rec.expires_at_unix,
            rec.kind,
        );
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &plain,
                    aad: &aad,
                },
            )
            .map_err(|e| Error::Crypto(format!("Sync AEAD encrypt failed: {e}")))?;
        Ok((ciphertext, nonce))
    }

    fn decrypt_record(
        &self,
        conv_id: &[u8; 32],
        logical_id: &[u8; 16],
        timestamps: (u64, u64),
        kind_u8: i64,
        ciphertext: &[u8],
        nonce: &[u8],
    ) -> Result<SyncRecord> {
        let (created_at_unix, expires_at_unix) = timestamps;
        if nonce.len() != 12 {
            return Err(Error::Crypto("Invalid sync AEAD nonce length".into()));
        }
        let kind = SyncRecordKind::from_u8(
            u8::try_from(kind_u8).map_err(|_| Error::Crypto("Invalid sync row kind".into()))?,
        )?;
        let aad = build_row_aad(conv_id, logical_id, created_at_unix, expires_at_unix, kind);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.key.0));
        let plain = cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|e| Error::Storage(format!("SYNC_AEAD_CORRUPT: {e}")))?;
        let (rec, consumed) = SyncRecord::decode_wire(&plain)?;
        if consumed != plain.len()
            || rec.conv_id != *conv_id
            || rec.logical_id != *logical_id
            || rec.created_at_unix != created_at_unix
            || rec.expires_at_unix != expires_at_unix
            || rec.kind != kind
        {
            return Err(Error::Storage(
                "SYNC_ROW_TAMPERED: metadata mismatch between columns and ciphertext".into(),
            ));
        }
        Ok(rec)
    }

    fn read_horizon_conn(conn: &Connection, conv_id: &[u8; 32]) -> Result<u64> {
        let val: Option<i64> = conn
            .query_row(
                "SELECT horizon_unix FROM sync_conversations WHERE conv_id = ?1",
                params![conv_id.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| Error::Storage(format!("SYNC_HORIZON_READ_FAILED: {e}")))?;
        Ok(val.map(|v| v.max(0) as u64).unwrap_or(0))
    }

    fn set_horizon_conn(conn: &Connection, conv_id: &[u8; 32], horizon_unix: u64) -> Result<()> {
        let h_i64 = i64::try_from(horizon_unix).unwrap_or(i64::MAX);
        conn.execute(
            "INSERT INTO sync_conversations (conv_id, horizon_unix) VALUES (?1, ?2)
             ON CONFLICT(conv_id) DO UPDATE SET horizon_unix = MAX(sync_conversations.horizon_unix, excluded.horizon_unix)",
            params![conv_id.as_slice(), h_i64],
        )
        .map_err(|e| Error::Storage(format!("SYNC_HORIZON_WRITE_FAILED: {e}")))?;
        Ok(())
    }

    /// Query the monotonic deletion/compaction anti-resurrection horizon for `conv_id`.
    pub fn horizon_for(&self, conv_id: &[u8; 32]) -> Result<u64> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Storage("Sync DB mutex poisoned".into()))?;
        Self::read_horizon_conn(&conn, conv_id)
    }

    /// Prune expired rows and age-out tombstones older than `TOMBSTONE_RETENTION_SECS`,
    /// advancing `horizon_unix` whenever a tombstone is evicted early so deleted records cannot resurrect.
    pub fn prune_and_compact(&self, conv_id: &[u8; 32], now_unix: u64) -> Result<(usize, u64)> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Storage("Sync DB mutex poisoned".into()))?;
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| Error::Storage(format!("SYNC_TX_BEGIN_FAILED: {e}")))?;

        let res = Self::prune_and_compact_in_tx(&conn, conv_id, now_unix);
        match res {
            Ok(out) => {
                conn.execute_batch("COMMIT")
                    .map_err(|e| Error::Storage(format!("SYNC_TX_COMMIT_FAILED: {e}")))?;
                if out.0 > 0 {
                    self.mutation_epoch.fetch_add(1, Ordering::Release);
                }
                Ok(out)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    fn prune_and_compact_in_tx(
        conn: &Connection,
        conv_id: &[u8; 32],
        now_unix: u64,
    ) -> Result<(usize, u64)> {
        let now_i64 = i64::try_from(now_unix).unwrap_or(i64::MAX);
        let mut horizon = Self::read_horizon_conn(conn, conv_id)?;

        // 1. Delete naturally expired records (expiry applies symmetrically across peers)
        let expired = conn
            .execute(
                "DELETE FROM sync_records WHERE conv_id = ?1 AND expires_at_unix <= ?2",
                params![conv_id.as_slice(), now_i64],
            )
            .map_err(|e| Error::Storage(format!("SYNC_EXPIRE_FAILED: {e}")))?;

        // 2. Evict tombstones older than TOMBSTONE_RETENTION_SECS and advance anti-resurrection horizon
        let age_cutoff = now_unix.saturating_sub(TOMBSTONE_RETENTION_SECS);
        let max_aged_tomb: Option<i64> = conn
            .query_row(
                "SELECT MAX(created_at_unix) FROM sync_records
                 WHERE conv_id = ?1 AND kind = 2 AND created_at_unix <= ?2",
                params![conv_id.as_slice(), age_cutoff as i64],
                |r| r.get(0),
            )
            .map_err(|e| Error::Storage(format!("SYNC_AGED_TOMB_QUERY_FAILED: {e}")))?;

        let mut aged_evicted = 0usize;
        if let Some(max_t) = max_aged_tomb {
            let new_h = (max_t.max(0) as u64).saturating_add(1);
            if new_h > horizon {
                horizon = new_h;
                Self::set_horizon_conn(conn, conv_id, horizon)?;
            }
            aged_evicted = conn
                .execute(
                    "DELETE FROM sync_records WHERE conv_id = ?1 AND created_at_unix < ?2",
                    params![conv_id.as_slice(), horizon as i64],
                )
                .map_err(|e| Error::Storage(format!("SYNC_AGED_EVICT_FAILED: {e}")))?;
        }

        // 3. Enforce hard capacity ceiling MAX_SYNC_RECORDS_PER_CONVERSATION (1024)
        let total: usize = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_records WHERE conv_id = ?1",
                params![conv_id.as_slice()],
                |r| r.get::<_, i64>(0),
            )
            .map(|c| c.max(0) as usize)
            .map_err(|e| Error::Storage(format!("SYNC_COUNT_FAILED: {e}")))?;

        let mut cap_evicted = 0usize;
        if total > MAX_SYNC_RECORDS_PER_CONVERSATION {
            let overflow = total - MAX_SYNC_RECORDS_PER_CONVERSATION;
            let mut stmt = conn
                .prepare(
                    "SELECT logical_id, created_at_unix, kind FROM sync_records
                     WHERE conv_id = ?1
                     ORDER BY created_at_unix ASC, logical_id ASC
                     LIMIT ?2",
                )
                .map_err(|e| Error::Storage(format!("SYNC_EVICT_PREP_FAILED: {e}")))?;
            let rows: Vec<(Vec<u8>, i64, i64)> = stmt
                .query_map(params![conv_id.as_slice(), overflow as i64], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .map_err(|e| Error::Storage(format!("SYNC_EVICT_QUERY_FAILED: {e}")))?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| Error::Storage(format!("SYNC_EVICT_ROW_FAILED: {e}")))?;
            drop(stmt);

            for (lid, created_i64, kind_i64) in rows {
                if kind_i64 == SyncRecordKind::Tombstone as i64 {
                    horizon = horizon.max((created_i64.max(0) as u64).saturating_add(1));
                }
                conn.execute(
                    "DELETE FROM sync_records WHERE conv_id = ?1 AND logical_id = ?2",
                    params![conv_id.as_slice(), lid],
                )
                .map_err(|e| Error::Storage(format!("SYNC_EVICT_DEL_FAILED: {e}")))?;
                cap_evicted += 1;
            }
            if horizon > 0 {
                Self::set_horizon_conn(conn, conv_id, horizon)?;
            }
        }

        Ok((expired + aged_evicted + cap_evicted, horizon))
    }

    /// Atomically upsert verified records into `bitchat_sync.db` enforcing Tombstone dominance,
    /// `horizon_unix` non-resurrection, expiry rejection, and the 1024-record capacity ceiling.
    pub fn upsert_records_atomic(
        &self,
        conv_id: &[u8; 32],
        records: &[SyncRecord],
        now_unix: u64,
    ) -> Result<usize> {
        let mut prepared = Vec::with_capacity(records.len());
        for rec in records {
            if rec.conv_id != *conv_id {
                return Err(Error::Storage(
                    "SYNC_CONV_MISMATCH: record conv_id does not match target conversation".into(),
                ));
            }
            if now_unix >= rec.expires_at_unix || rec.expires_at_unix <= rec.created_at_unix {
                continue;
            }
            let (ct, nonce) = self.encrypt_record(rec)?;
            prepared.push((rec, ct, nonce));
        }

        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Storage("Sync DB mutex poisoned".into()))?;
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| Error::Storage(format!("SYNC_TX_BEGIN_FAILED: {e}")))?;

        let tx_res: Result<(usize, usize)> = (|| {
            let mut horizon = Self::read_horizon_conn(&conn, conv_id)?;
            let mut applied = 0usize;

            for (rec, ct, nonce) in prepared {
                if rec.created_at_unix < horizon {
                    continue;
                }
                let existing_kind: Option<i64> = conn
                    .query_row(
                        "SELECT kind FROM sync_records WHERE conv_id = ?1 AND logical_id = ?2",
                        params![conv_id.as_slice(), rec.logical_id.as_slice()],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(|e| Error::Storage(format!("SYNC_LOOKUP_FAILED: {e}")))?;

                match existing_kind {
                    Some(k) if k == SyncRecordKind::Tombstone as i64 => {
                        // Tombstone always dominates; Message or duplicate Tombstone cannot overwrite
                        continue;
                    }
                    Some(_) if rec.kind == SyncRecordKind::Message => {
                        // Identical message already present; immutable logical ID
                        continue;
                    }
                    Some(_) => {
                        // Incoming Tombstone upgrades existing Message
                        conn.execute(
                            "UPDATE sync_records
                             SET created_at_unix = ?3, expires_at_unix = ?4, kind = ?5, ciphertext = ?6, nonce = ?7
                             WHERE conv_id = ?1 AND logical_id = ?2",
                            params![
                                conv_id.as_slice(),
                                rec.logical_id.as_slice(),
                                rec.created_at_unix as i64,
                                rec.expires_at_unix as i64,
                                rec.kind as i64,
                                ct,
                                nonce.as_slice(),
                            ],
                        )
                        .map_err(|e| Error::Storage(format!("SYNC_UPDATE_FAILED: {e}")))?;
                        applied += 1;
                    }
                    None => {
                        conn.execute(
                            "INSERT INTO sync_records
                             (conv_id, logical_id, created_at_unix, expires_at_unix, kind, ciphertext, nonce)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            params![
                                conv_id.as_slice(),
                                rec.logical_id.as_slice(),
                                rec.created_at_unix as i64,
                                rec.expires_at_unix as i64,
                                rec.kind as i64,
                                ct,
                                nonce.as_slice(),
                            ],
                        )
                        .map_err(|e| Error::Storage(format!("SYNC_INSERT_FAILED: {e}")))?;
                        applied += 1;
                    }
                }
            }

            let (pruned, new_h) = Self::prune_and_compact_in_tx(&conn, conv_id, now_unix)?;
            horizon = horizon.max(new_h);
            let _ = horizon;
            Ok((applied, pruned))
        })();

        match tx_res {
            Ok((count, pruned)) => {
                conn.execute_batch("COMMIT")
                    .map_err(|e| Error::Storage(format!("SYNC_TX_COMMIT_FAILED: {e}")))?;
                if count > 0 || pruned > 0 {
                    self.mutation_epoch.fetch_add(1, Ordering::Release);
                }
                Ok(count)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Force-advance `horizon_unix` during explicit local tombstone eviction/compaction test or maintenance.
    pub fn evict_tombstones_before(
        &self,
        conv_id: &[u8; 32],
        cutoff_created_at: u64,
    ) -> Result<(usize, u64)> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Storage("Sync DB mutex poisoned".into()))?;
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| Error::Storage(format!("SYNC_TX_BEGIN_FAILED: {e}")))?;

        let tx_res: Result<(usize, u64)> = (|| {
            let mut horizon = Self::read_horizon_conn(&conn, conv_id)?;
            horizon = horizon.max(cutoff_created_at.saturating_add(1));
            Self::set_horizon_conn(&conn, conv_id, horizon)?;
            let deleted = conn
                .execute(
                    "DELETE FROM sync_records WHERE conv_id = ?1 AND created_at_unix < ?2",
                    params![conv_id.as_slice(), horizon as i64],
                )
                .map_err(|e| Error::Storage(format!("SYNC_COMPACT_DEL_FAILED: {e}")))?;
            Ok((deleted, horizon))
        })();

        match tx_res {
            Ok(out) => {
                conn.execute_batch("COMMIT")
                    .map_err(|e| Error::Storage(format!("SYNC_TX_COMMIT_FAILED: {e}")))?;
                self.mutation_epoch.fetch_add(1, Ordering::Release);
                Ok(out)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// List all non-expired records in `conv_id`, sorted deterministically.
    pub fn list_records(&self, conv_id: &[u8; 32], now_unix: u64) -> Result<Vec<SyncRecord>> {
        let _ = self.prune_and_compact(conv_id, now_unix)?;
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Storage("Sync DB mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare(
                "SELECT logical_id, created_at_unix, expires_at_unix, kind, ciphertext, nonce
                 FROM sync_records
                 WHERE conv_id = ?1
                 ORDER BY created_at_unix ASC, logical_id ASC",
            )
            .map_err(|e| Error::Storage(format!("SYNC_LIST_PREP_FAILED: {e}")))?;
        let rows = stmt
            .query_map(params![conv_id.as_slice()], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                ))
            })
            .map_err(|e| Error::Storage(format!("SYNC_LIST_QUERY_FAILED: {e}")))?;

        let mut records = Vec::new();
        for row_res in rows {
            let (lid_vec, c_at, e_at, kind_i64, ct, nonce) =
                row_res.map_err(|e| Error::Storage(format!("SYNC_LIST_ROW_FAILED: {e}")))?;
            if lid_vec.len() != 16 || c_at < 0 || e_at < 0 {
                return Err(Error::Storage(
                    "SYNC_DB_CORRUPT: invalid column dimensions".into(),
                ));
            }
            let mut lid = [0u8; 16];
            lid.copy_from_slice(&lid_vec);
            let rec = self.decrypt_record(
                conv_id,
                &lid,
                (c_at as u64, e_at as u64),
                kind_i64,
                &ct,
                &nonce,
            )?;
            if now_unix < rec.expires_at_unix {
                records.push(rec);
            }
        }
        records.sort_by_key(SyncRecord::sort_key);
        Ok(records)
    }

    pub fn snapshot_for_conversation(
        &self,
        conv_id: &[u8; 32],
        now_unix: u64,
    ) -> Result<SyncSnapshot> {
        let records = self.list_records(conv_id, now_unix)?;
        let horizon = self.horizon_for(conv_id)?;
        SyncSnapshot::build(*conv_id, horizon, records)
    }

    pub fn get_records_by_ids(
        &self,
        conv_id: &[u8; 32],
        ids: &[[u8; 16]],
        now_unix: u64,
    ) -> Result<Vec<SyncRecord>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Storage("Sync DB mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare(
                "SELECT created_at_unix, expires_at_unix, kind, ciphertext, nonce
                 FROM sync_records
                 WHERE conv_id = ?1 AND logical_id = ?2",
            )
            .map_err(|e| Error::Storage(format!("SYNC_GET_IDS_PREP_FAILED: {e}")))?;

        let mut unique: Vec<[u8; 16]> = ids.to_vec();
        unique.sort();
        unique.dedup();

        type SyncRowTuple = (i64, i64, i64, Vec<u8>, Vec<u8>);
        let mut records = Vec::with_capacity(unique.len());
        for lid in unique {
            let row_opt: Option<SyncRowTuple> = stmt
                .query_row(params![conv_id.as_slice(), lid.as_slice()], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })
                .optional()
                .map_err(|e| Error::Storage(format!("SYNC_GET_ID_QUERY_FAILED: {e}")))?;

            if let Some((c_at, e_at, kind_i64, ct, nonce)) = row_opt {
                if c_at < 0 || e_at < 0 {
                    return Err(Error::Storage(
                        "SYNC_DB_CORRUPT: invalid timestamp columns".into(),
                    ));
                }
                let rec = self.decrypt_record(
                    conv_id,
                    &lid,
                    (c_at as u64, e_at as u64),
                    kind_i64,
                    &ct,
                    &nonce,
                )?;
                if now_unix < rec.expires_at_unix {
                    records.push(rec);
                }
            }
        }
        records.sort_by_key(SyncRecord::sort_key);
        Ok(records)
    }
}
