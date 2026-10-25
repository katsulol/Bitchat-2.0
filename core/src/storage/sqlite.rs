//! Bounded encrypted SQLite outbox store.
//!
//! Provides durable persistence for Normal Mode with AEAD record encryption.
//! Plaintext message bodies NEVER touch SQLite disk storage, indexes, or WAL.

use crate::identity::PeerId;
use crate::mesh::delivery::DeliveryState;
use crate::storage::key::KeyProvider;
use crate::storage::outbox::{MessageMode, OutboxLimits, OutboxRecord, OutboxStore, SendRejection};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Nonce,
};
use rand::RngCore;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// SQLite-backed encrypted outbox store.
///
/// Metadata visible in SQLite tables and indexes:
/// - `logical_id`: 16-byte random message identifier.
/// - `sender`: 32-byte Ed25519 identity key.
/// - `recipient`: 32-byte Ed25519 identity key.
/// - `mode`: message mode (Normal = 0).
/// - `state`: integer delivery state.
/// - `created_at`, `expires_at`, `attempt_count`, `last_attempt_at`: delivery timestamps.
/// - `key_version`: DEK version identifier.
///
/// Protected data:
/// - `body`: Always encrypted with ChaCha20-Poly1305 before SQL INSERT.
/// - AEAD Associated Data (AAD) binds record type, schema version, identities, logical ID, and mode.
pub struct SqliteOutboxStore {
    conn: Mutex<Connection>,
    key_provider: Arc<dyn KeyProvider>,
    limits: OutboxLimits,
}

struct DecryptParams<'a> {
    key_version: u32,
    nonce_bytes: &'a [u8],
    ciphertext: &'a [u8],
    logical_id: &'a [u8; 16],
    sender: &'a PeerId,
    recipient: &'a PeerId,
    mode: MessageMode,
}

impl SqliteOutboxStore {
    /// Open or create an encrypted SQLite store at the given path.
    pub fn open(
        path: impl AsRef<Path>,
        key_provider: Arc<dyn KeyProvider>,
        limits: OutboxLimits,
    ) -> Result<Self, SendRejection> {
        let conn = Connection::open(path)
            .map_err(|e| SendRejection::StorageError(format!("SQLite open failed: {e}")))?;

        // Ensure WAL mode for durability and concurrent reads
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")
            .map_err(|e| SendRejection::StorageError(format!("PRAGMA failed: {e}")))?;

        Self::init_schema(&conn)?;

        Ok(Self {
            conn: Mutex::new(conn),
            key_provider,
            limits,
        })
    }

    /// Access raw mutex connection for testing and tamper tests.
    #[cfg(test)]
    pub fn raw_conn_for_test(&self) -> &Mutex<Connection> {
        &self.conn
    }
    pub fn in_memory(
        key_provider: Arc<dyn KeyProvider>,
        limits: OutboxLimits,
    ) -> Result<Self, SendRejection> {
        let conn = Connection::open_in_memory()
            .map_err(|e| SendRejection::StorageError(format!("SQLite in-memory failed: {e}")))?;

        Self::init_schema(&conn)?;

        Ok(Self {
            conn: Mutex::new(conn),
            key_provider,
            limits,
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), SendRejection> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS outbox (
                logical_id BLOB PRIMARY KEY,
                sender BLOB NOT NULL,
                recipient BLOB NOT NULL,
                mode INTEGER NOT NULL,
                state INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                attempt_count INTEGER NOT NULL,
                last_attempt_at INTEGER,
                key_version INTEGER NOT NULL,
                nonce BLOB NOT NULL,
                ciphertext BLOB NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_outbox_state ON outbox(state);
            CREATE INDEX IF NOT EXISTS idx_outbox_recipient ON outbox(recipient, state);
            CREATE INDEX IF NOT EXISTS idx_outbox_expires ON outbox(expires_at);",
        )
        .map_err(|e| SendRejection::StorageError(format!("Init schema failed: {e}")))?;
        Ok(())
    }

    fn build_aad(
        key_version: u32,
        logical_id: &[u8; 16],
        sender: &PeerId,
        recipient: &PeerId,
        mode: MessageMode,
    ) -> Vec<u8> {
        let mut aad = Vec::with_capacity(4 + 16 + 32 + 32 + 1 + 19);
        aad.extend_from_slice(b"bitchat2-outbox-v1:");
        aad.extend_from_slice(&key_version.to_le_bytes());
        aad.extend_from_slice(logical_id);
        aad.extend_from_slice(sender.as_bytes());
        aad.extend_from_slice(recipient.as_bytes());
        aad.push(mode as u8);
        aad
    }

    fn encrypt_body(
        &self,
        record: &OutboxRecord,
    ) -> Result<(u32, [u8; 12], Vec<u8>), SendRejection> {
        let dek = self.key_provider.get_or_create_dek().map_err(|e| {
            if matches!(e, crate::error::Error::KeyStore(_)) {
                SendRejection::StorageLocked
            } else {
                SendRejection::StorageError(format!("Key provider failed: {e}"))
            }
        })?;

        let cipher = ChaCha20Poly1305::new_from_slice(&dek.key)
            .map_err(|e| SendRejection::StorageError(format!("Cipher init failed: {e}")))?;

        let mut nonce = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce);

        let aad = Self::build_aad(
            dek.version,
            &record.logical_id,
            &record.sender,
            &record.recipient,
            record.mode,
        );

        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &record.body,
                    aad: &aad,
                },
            )
            .map_err(|e| SendRejection::StorageError(format!("AEAD encryption failed: {e}")))?;

        Ok((dek.version, nonce, ciphertext))
    }

    fn decrypt_body(&self, params: DecryptParams<'_>) -> Result<Vec<u8>, SendRejection> {
        let dek = self.key_provider.get_or_create_dek().map_err(|e| {
            if matches!(e, crate::error::Error::KeyStore(_)) {
                SendRejection::StorageLocked
            } else {
                SendRejection::StorageError(format!("Key provider failed: {e}"))
            }
        })?;

        if dek.version != params.key_version {
            return Err(SendRejection::StorageError(format!(
                "DEK version mismatch: expected {}, got {}",
                dek.version, params.key_version
            )));
        }

        let cipher = ChaCha20Poly1305::new_from_slice(&dek.key)
            .map_err(|e| SendRejection::StorageError(format!("Cipher init failed: {e}")))?;

        let aad = Self::build_aad(
            params.key_version,
            params.logical_id,
            params.sender,
            params.recipient,
            params.mode,
        );

        cipher
            .decrypt(
                Nonce::from_slice(params.nonce_bytes),
                Payload {
                    msg: params.ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|e| {
                SendRejection::StorageError(format!("AEAD authentication/decryption failed: {e}"))
            })
    }
}

impl OutboxStore for SqliteOutboxStore {
    fn enqueue(&mut self, record: OutboxRecord) -> Result<(), SendRejection> {
        // Enforce zero-disk guarantee for Privacy Mode
        if record.mode == MessageMode::Privacy {
            return Err(SendRejection::StorageError(
                "Privacy mode messages must never be written to SQLite disk storage".into(),
            ));
        }

        if record.body.len() > self.limits.max_record_bytes {
            return Err(SendRejection::PayloadTooLarge {
                max: self.limits.max_record_bytes,
                actual: record.body.len(),
            });
        }

        let conn = self
            .conn
            .lock()
            .map_err(|_| SendRejection::StorageError("Mutex lock failed".into()))?;

        // Check if already present (idempotent enqueue)
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM outbox WHERE logical_id = ?1)",
                params![record.logical_id.as_slice()],
                |row| row.get(0),
            )
            .map_err(|e| SendRejection::StorageError(format!("Query failed: {e}")))?;

        if exists {
            return Ok(());
        }

        // Check queue bounds
        let count: usize = conn
            .query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))
            .map_err(|e| SendRejection::StorageError(format!("Count query failed: {e}")))?;

        if count >= self.limits.max_records {
            return Err(SendRejection::QueueFull {
                current: count,
                max: self.limits.max_records,
            });
        }

        // Encrypt body BEFORE inserting into database
        let (key_version, nonce, ciphertext) = self.encrypt_body(&record)?;

        conn.execute(
            "INSERT INTO outbox (
                logical_id, sender, recipient, mode, state, created_at, expires_at, attempt_count, last_attempt_at, key_version, nonce, ciphertext
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                record.logical_id.as_slice(),
                record.sender.as_bytes().as_slice(),
                record.recipient.as_bytes().as_slice(),
                record.mode as u8,
                record.state as u8,
                record.created_at,
                record.expires_at,
                record.attempt_count,
                record.last_attempt_at,
                key_version,
                nonce.as_slice(),
                ciphertext,
            ],
        )
        .map_err(|e| SendRejection::StorageError(format!("Insert failed: {e}")))?;

        Ok(())
    }

    fn mark_in_flight(&mut self, logical_id: &[u8; 16], now: u64) -> Result<(), SendRejection> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| SendRejection::StorageError("Mutex lock failed".into()))?;

        // Monotonic check: terminal states cannot be changed
        let mut stmt = conn
            .prepare("SELECT state, attempt_count FROM outbox WHERE logical_id = ?1")
            .map_err(|e| SendRejection::StorageError(format!("Prepare failed: {e}")))?;

        let row = stmt.query_row(params![logical_id.as_slice()], |row| {
            let state: u8 = row.get(0)?;
            let attempts: u32 = row.get(1)?;
            Ok((state, attempts))
        });

        match row {
            Ok((state_raw, attempts)) => {
                let state = match state_raw {
                    0 => DeliveryState::Queued,
                    1 => DeliveryState::InFlight,
                    2 => DeliveryState::Delivered,
                    3 => DeliveryState::Failed,
                    4 => DeliveryState::Expired,
                    5 => DeliveryState::Cancelled,
                    _ => DeliveryState::Failed,
                };
                if state.is_terminal() {
                    return Ok(());
                }
                conn.execute(
                    "UPDATE outbox SET state = ?1, attempt_count = ?2, last_attempt_at = ?3 WHERE logical_id = ?4",
                    params![
                        DeliveryState::InFlight as u8,
                        attempts.saturating_add(1),
                        now,
                        logical_id.as_slice()
                    ],
                )
                .map_err(|e| SendRejection::StorageError(format!("Update failed: {e}")))?;
                Ok(())
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                Err(SendRejection::StorageError("Record not found".into()))
            }
            Err(e) => Err(SendRejection::StorageError(format!("Query failed: {e}"))),
        }
    }

    fn mark_delivered(&mut self, logical_id: &[u8; 16]) -> Result<(), SendRejection> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| SendRejection::StorageError("Mutex lock failed".into()))?;

        conn.execute(
            "UPDATE outbox SET state = ?1 WHERE logical_id = ?2",
            params![DeliveryState::Delivered as u8, logical_id.as_slice()],
        )
        .map_err(|e| SendRejection::StorageError(format!("Update failed: {e}")))?;
        Ok(())
    }

    fn mark_failed(&mut self, logical_id: &[u8; 16]) -> Result<(), SendRejection> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| SendRejection::StorageError("Mutex lock failed".into()))?;

        conn.execute(
            "UPDATE outbox SET state = ?1 WHERE logical_id = ?2",
            params![DeliveryState::Failed as u8, logical_id.as_slice()],
        )
        .map_err(|e| SendRejection::StorageError(format!("Update failed: {e}")))?;
        Ok(())
    }

    fn mark_cancelled(&mut self, logical_id: &[u8; 16]) -> Result<(), SendRejection> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| SendRejection::StorageError("Mutex lock failed".into()))?;

        conn.execute(
            "UPDATE outbox SET state = ?1 WHERE logical_id = ?2",
            params![DeliveryState::Cancelled as u8, logical_id.as_slice()],
        )
        .map_err(|e| SendRejection::StorageError(format!("Update failed: {e}")))?;
        Ok(())
    }

    fn get(&self, logical_id: &[u8; 16]) -> Result<Option<OutboxRecord>, SendRejection> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| SendRejection::StorageError("Mutex lock failed".into()))?;

        let mut stmt = conn
            .prepare(
                "SELECT sender, recipient, mode, state, created_at, expires_at, attempt_count, last_attempt_at, key_version, nonce, ciphertext
                 FROM outbox WHERE logical_id = ?1",
            )
            .map_err(|e| SendRejection::StorageError(format!("Prepare failed: {e}")))?;

        let mut rows = stmt
            .query(params![logical_id.as_slice()])
            .map_err(|e| SendRejection::StorageError(format!("Query failed: {e}")))?;

        if let Some(row) = rows
            .next()
            .map_err(|e| SendRejection::StorageError(format!("Row fetch failed: {e}")))?
        {
            let sender_bytes: Vec<u8> = row
                .get(0)
                .map_err(|e| SendRejection::StorageError(format!("Column 0 failed: {e}")))?;
            let recipient_bytes: Vec<u8> = row
                .get(1)
                .map_err(|e| SendRejection::StorageError(format!("Column 1 failed: {e}")))?;
            let mode_val: u8 = row
                .get(2)
                .map_err(|e| SendRejection::StorageError(format!("Column 2 failed: {e}")))?;
            let state_val: u8 = row
                .get(3)
                .map_err(|e| SendRejection::StorageError(format!("Column 3 failed: {e}")))?;
            let created_at: u64 = row
                .get(4)
                .map_err(|e| SendRejection::StorageError(format!("Column 4 failed: {e}")))?;
            let expires_at: u64 = row
                .get(5)
                .map_err(|e| SendRejection::StorageError(format!("Column 5 failed: {e}")))?;
            let attempt_count: u32 = row
                .get(6)
                .map_err(|e| SendRejection::StorageError(format!("Column 6 failed: {e}")))?;
            let last_attempt_at: Option<u64> = row
                .get(7)
                .map_err(|e| SendRejection::StorageError(format!("Column 7 failed: {e}")))?;
            let key_version: u32 = row
                .get(8)
                .map_err(|e| SendRejection::StorageError(format!("Column 8 failed: {e}")))?;
            let nonce: Vec<u8> = row
                .get(9)
                .map_err(|e| SendRejection::StorageError(format!("Column 9 failed: {e}")))?;
            let ciphertext: Vec<u8> = row
                .get(10)
                .map_err(|e| SendRejection::StorageError(format!("Column 10 failed: {e}")))?;

            if sender_bytes.len() != 32 || recipient_bytes.len() != 32 {
                return Err(SendRejection::StorageError("Corrupted PeerId in DB".into()));
            }

            let mut s_arr = [0u8; 32];
            s_arr.copy_from_slice(&sender_bytes);
            let sender = PeerId::from_bytes(s_arr);

            let mut r_arr = [0u8; 32];
            r_arr.copy_from_slice(&recipient_bytes);
            let recipient = PeerId::from_bytes(r_arr);

            let mode = if mode_val == 0 {
                MessageMode::Normal
            } else {
                MessageMode::Privacy
            };

            let state = match state_val {
                0 => DeliveryState::Queued,
                1 => DeliveryState::InFlight,
                2 => DeliveryState::Delivered,
                3 => DeliveryState::Failed,
                4 => DeliveryState::Expired,
                5 => DeliveryState::Cancelled,
                _ => DeliveryState::Failed,
            };

            let body = self.decrypt_body(DecryptParams {
                key_version,
                nonce_bytes: &nonce,
                ciphertext: &ciphertext,
                logical_id,
                sender: &sender,
                recipient: &recipient,
                mode,
            })?;

            Ok(Some(OutboxRecord {
                logical_id: *logical_id,
                sender,
                recipient,
                body,
                mode,
                created_at,
                expires_at,
                state,
                attempt_count,
                last_attempt_at,
            }))
        } else {
            Ok(None)
        }
    }

    fn get_pending(&self, recipient: Option<&PeerId>) -> Result<Vec<OutboxRecord>, SendRejection> {
        let ids: Vec<Vec<u8>> = {
            let conn = self
                .conn
                .lock()
                .map_err(|_| SendRejection::StorageError("Mutex lock failed".into()))?;

            let mut stmt = conn
                .prepare(
                    "SELECT logical_id FROM outbox WHERE state IN (0, 1) ORDER BY created_at ASC",
                )
                .map_err(|e| SendRejection::StorageError(format!("Prepare failed: {e}")))?;

            let mapped: Vec<Vec<u8>> = stmt
                .query_map([], |row| row.get(0))
                .map_err(|e| SendRejection::StorageError(format!("QueryMap failed: {e}")))?
                .filter_map(std::result::Result::ok)
                .collect();
            mapped
        };

        let mut records = Vec::new();
        for id_bytes in ids {
            if id_bytes.len() == 16 {
                let mut id = [0u8; 16];
                id.copy_from_slice(&id_bytes);
                if let Some(record) = self.get(&id)? {
                    if recipient.is_none_or(|p| record.recipient == *p) {
                        records.push(record);
                    }
                }
            }
        }
        Ok(records)
    }

    fn prune_expired(&mut self, now: u64) -> Result<usize, SendRejection> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| SendRejection::StorageError("Mutex lock failed".into()))?;

        let count = conn
            .execute(
                "UPDATE outbox SET state = ?1 WHERE expires_at <= ?2 AND state NOT IN (2, 3, 4, 5)",
                params![DeliveryState::Expired as u8, now],
            )
            .map_err(|e| SendRejection::StorageError(format!("Update expired failed: {e}")))?;

        Ok(count)
    }

    fn delete(&mut self, logical_id: &[u8; 16]) -> Result<bool, SendRejection> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| SendRejection::StorageError("Mutex lock failed".into()))?;

        let count = conn
            .execute(
                "DELETE FROM outbox WHERE logical_id = ?1",
                params![logical_id.as_slice()],
            )
            .map_err(|e| SendRejection::StorageError(format!("Delete failed: {e}")))?;

        Ok(count > 0)
    }

    fn record_count(&self) -> usize {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return 0,
        };
        conn.query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))
            .unwrap_or(0)
    }

    fn total_bytes(&self) -> usize {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return 0,
        };
        conn.query_row(
            "SELECT COALESCE(SUM(LENGTH(ciphertext)), 0) FROM outbox",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0)
    }
}
