//! Outbox and persistence subsystem.
//!
//! Enforces bounded storage, Normal-mode AEAD encrypted SQLite persistence,
//! and Privacy-mode zero-disk heap retention.

pub mod key;
pub mod memory;
pub mod outbox;
pub mod sqlite;

pub use key::{DataEncryptionKey, FailingKeyProvider, InjectedKeyProvider, KeyProvider};
pub use memory::MemoryOutboxStore;
pub use outbox::{MessageMode, OutboxLimits, OutboxRecord, OutboxStore, SendRejection, SendResult};
pub use sqlite::SqliteOutboxStore;

use crate::identity::PeerId;
use crate::mesh::delivery::DeliveryState;
use rand::RngCore;
use std::time::Duration;

/// High-level coordinator managing message admission, mode dispatch, and persistence.
pub struct OutboxManager {
    local_peer_id: PeerId,
    sqlite_store: SqliteOutboxStore,
    memory_store: MemoryOutboxStore,
    limits: OutboxLimits,
}

impl OutboxManager {
    pub fn new(
        local_peer_id: PeerId,
        sqlite_store: SqliteOutboxStore,
        memory_store: MemoryOutboxStore,
        limits: OutboxLimits,
    ) -> Self {
        Self {
            local_peer_id,
            sqlite_store,
            memory_store,
            limits,
        }
    }

    /// Submit an application message for transmission.
    ///
    /// Validates inputs and returns `Accepted(logical_id, Queued)` only after
    /// durable commit (Normal mode) or volatile memory admission (Privacy mode).
    pub fn send_to(
        &mut self,
        recipient: PeerId,
        body: Vec<u8>,
        mode: MessageMode,
        ttl_override: Option<Duration>,
        now: u64,
    ) -> SendResult {
        // Validation: non-zero recipient and not self
        if recipient.as_bytes() == &[0u8; 32] {
            return SendResult::Rejected(SendRejection::InvalidRecipient(
                "Recipient cannot be zero".into(),
            ));
        }
        if recipient == self.local_peer_id {
            return SendResult::Rejected(SendRejection::InvalidRecipient(
                "Self-addressed messages are unsupported".into(),
            ));
        }

        // Validation: body length bound
        if body.is_empty() {
            return SendResult::Rejected(SendRejection::PayloadTooLarge {
                max: self.limits.max_record_bytes,
                actual: 0,
            });
        }
        if body.len() > self.limits.max_record_bytes {
            return SendResult::Rejected(SendRejection::PayloadTooLarge {
                max: self.limits.max_record_bytes,
                actual: body.len(),
            });
        }

        // Determine TTL bounded by mode limits
        let ttl = ttl_override.unwrap_or_else(|| mode.default_ttl());
        let bounded_ttl = ttl.min(mode.max_ttl());
        let expires_at = now.saturating_add(bounded_ttl.as_secs());

        // Generate logical message ID once
        let mut logical_id = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut logical_id);

        let record = OutboxRecord {
            logical_id,
            sender: self.local_peer_id,
            recipient,
            body,
            mode,
            created_at: now,
            expires_at,
            state: DeliveryState::Queued,
            attempt_count: 0,
            last_attempt_at: None,
        };

        match mode {
            MessageMode::Normal => {
                // Durable commit in SQLite
                if let Err(e) = self.sqlite_store.enqueue(record) {
                    return SendResult::Rejected(e);
                }
            }
            MessageMode::Privacy => {
                // Volatile heap storage only (zero-disk guarantee)
                if let Err(e) = self.memory_store.enqueue(record) {
                    return SendResult::Rejected(e);
                }
            }
        }

        SendResult::Accepted {
            logical_id,
            state: DeliveryState::Queued,
        }
    }

    /// Mark a message attempt as InFlight.
    pub fn mark_in_flight(&mut self, logical_id: &[u8; 16], now: u64) -> Result<(), SendRejection> {
        if self.memory_store.get(logical_id)?.is_some() {
            self.memory_store.mark_in_flight(logical_id, now)
        } else {
            self.sqlite_store.mark_in_flight(logical_id, now)
        }
    }

    /// Mark message delivered.
    pub fn mark_delivered(&mut self, logical_id: &[u8; 16]) -> Result<(), SendRejection> {
        let _ = self.memory_store.mark_delivered(logical_id);
        let _ = self.sqlite_store.mark_delivered(logical_id);
        Ok(())
    }

    /// Mark message failed.
    pub fn mark_failed(&mut self, logical_id: &[u8; 16]) -> Result<(), SendRejection> {
        let _ = self.memory_store.mark_failed(logical_id);
        let _ = self.sqlite_store.mark_failed(logical_id);
        Ok(())
    }

    /// Mark message cancelled.
    pub fn mark_cancelled(&mut self, logical_id: &[u8; 16]) -> Result<(), SendRejection> {
        let _ = self.memory_store.mark_cancelled(logical_id);
        let _ = self.sqlite_store.mark_cancelled(logical_id);
        Ok(())
    }

    /// Retrieve a record by logical ID.
    pub fn get(&self, logical_id: &[u8; 16]) -> Result<Option<OutboxRecord>, SendRejection> {
        if let Some(r) = self.memory_store.get(logical_id)? {
            return Ok(Some(r));
        }
        self.sqlite_store.get(logical_id)
    }

    /// Retrieve all pending records across storage engines.
    pub fn get_pending(
        &self,
        recipient: Option<&PeerId>,
    ) -> Result<Vec<OutboxRecord>, SendRejection> {
        let mut list = self.memory_store.get_pending(recipient)?;
        let mut sqlite_list = self.sqlite_store.get_pending(recipient)?;
        list.append(&mut sqlite_list);
        list.sort_by_key(|r| r.created_at);
        Ok(list)
    }

    /// Prune expired records across storage engines.
    pub fn prune_expired(&mut self, now: u64) -> Result<usize, SendRejection> {
        let m = self.memory_store.prune_expired(now)?;
        let s = self.sqlite_store.prune_expired(now)?;
        Ok(m + s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use std::sync::Arc;

    #[test]
    fn test_send_to_normal_mode_durable_commit_and_aead_privacy() {
        let id_a = Identity::generate();
        let id_b = Identity::generate();
        let key_provider = Arc::new(InjectedKeyProvider::random(1));
        let limits = OutboxLimits::default();

        let sqlite = SqliteOutboxStore::in_memory(key_provider.clone(), limits).unwrap();
        let memory = MemoryOutboxStore::new(limits);
        let mut manager = OutboxManager::new(id_a.peer_id(), sqlite, memory, limits);

        let body = b"Super secret message for Bob".to_vec();
        let res = manager.send_to(id_b.peer_id(), body.clone(), MessageMode::Normal, None, 100);

        let logical_id = match res {
            SendResult::Accepted { logical_id, state } => {
                assert_eq!(state, DeliveryState::Queued);
                logical_id
            }
            SendResult::Rejected(r) => panic!("Expected acceptance, got rejection: {r:?}"),
        };

        // Retrieve through manager and verify decrypted content
        let rec = manager
            .get(&logical_id)
            .unwrap()
            .expect("Record must exist");
        assert_eq!(rec.body, body);
        assert_eq!(rec.mode, MessageMode::Normal);
        assert_eq!(rec.state, DeliveryState::Queued);

        // Verify SQLite storage directly: plaintext body must NOT appear anywhere in DB
        let conn = manager.sqlite_store.raw_conn_for_test().lock().unwrap();
        let ciphertext: Vec<u8> = conn
            .query_row(
                "SELECT ciphertext FROM outbox WHERE logical_id = ?1",
                rusqlite::params![logical_id.as_slice()],
                |row| row.get(0),
            )
            .unwrap();

        assert_ne!(ciphertext, body);
        assert!(!ciphertext.windows(body.len()).any(|w| w == body.as_slice()));
    }

    #[test]
    fn test_send_to_privacy_mode_zero_disk_guarantee() {
        let id_a = Identity::generate();
        let id_b = Identity::generate();
        let key_provider = Arc::new(InjectedKeyProvider::random(1));
        let limits = OutboxLimits::default();

        let sqlite = SqliteOutboxStore::in_memory(key_provider.clone(), limits).unwrap();
        let memory = MemoryOutboxStore::new(limits);
        let mut manager = OutboxManager::new(id_a.peer_id(), sqlite, memory, limits);

        let body = b"Volatile ephemeral message".to_vec();
        let res = manager.send_to(
            id_b.peer_id(),
            body.clone(),
            MessageMode::Privacy,
            None,
            100,
        );

        let logical_id = match res {
            SendResult::Accepted { logical_id, state } => {
                assert_eq!(state, DeliveryState::Queued);
                logical_id
            }
            SendResult::Rejected(r) => panic!("Expected acceptance: {r:?}"),
        };

        // Record must be retrieved from memory
        let rec = manager.get(&logical_id).unwrap().unwrap();
        assert_eq!(rec.body, body);
        assert_eq!(rec.mode, MessageMode::Privacy);

        // ZERO-DISK CHECK: raw SQLite DB must have 0 rows
        let conn = manager.sqlite_store.raw_conn_for_test().lock().unwrap();
        let count: usize = conn
            .query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0, "Privacy messages must NEVER touch SQLite");
    }

    #[test]
    fn test_tampered_ciphertext_fails_aead_authentication() {
        let id_a = Identity::generate();
        let id_b = Identity::generate();
        let key_provider = Arc::new(InjectedKeyProvider::random(1));
        let limits = OutboxLimits::default();

        let sqlite = SqliteOutboxStore::in_memory(key_provider.clone(), limits).unwrap();
        let memory = MemoryOutboxStore::new(limits);
        let mut manager = OutboxManager::new(id_a.peer_id(), sqlite, memory, limits);

        let body = b"Message to be tampered with".to_vec();
        let logical_id = match manager.send_to(id_b.peer_id(), body, MessageMode::Normal, None, 100)
        {
            SendResult::Accepted { logical_id, .. } => logical_id,
            SendResult::Rejected(e) => panic!("Rejected: {e:?}"),
        };

        // Tamper with the raw ciphertext in SQLite
        {
            let conn = manager.sqlite_store.raw_conn_for_test().lock().unwrap();
            let mut ct: Vec<u8> = conn
                .query_row(
                    "SELECT ciphertext FROM outbox WHERE logical_id = ?1",
                    rusqlite::params![logical_id.as_slice()],
                    |row| row.get(0),
                )
                .unwrap();
            // Flip last byte
            let last = ct.len() - 1;
            ct[last] ^= 0x55;
            conn.execute(
                "UPDATE outbox SET ciphertext = ?1 WHERE logical_id = ?2",
                rusqlite::params![ct, logical_id.as_slice()],
            )
            .unwrap();
        }

        // Retrieving the record must fail with StorageError (AEAD auth error), never corrupted text
        let err = manager.get(&logical_id);
        assert!(err.is_err());
        assert!(matches!(err.unwrap_err(), SendRejection::StorageError(_)));
    }

    #[test]
    fn test_keystore_locked_rejection() {
        let id_a = Identity::generate();
        let id_b = Identity::generate();
        let failing_provider = Arc::new(FailingKeyProvider::new("Keystore is locked"));
        let limits = OutboxLimits::default();

        let sqlite = SqliteOutboxStore::in_memory(failing_provider, limits).unwrap();
        let memory = MemoryOutboxStore::new(limits);
        let mut manager = OutboxManager::new(id_a.peer_id(), sqlite, memory, limits);

        let res = manager.send_to(
            id_b.peer_id(),
            b"Hello".to_vec(),
            MessageMode::Normal,
            None,
            100,
        );
        assert_eq!(res, SendResult::Rejected(SendRejection::StorageLocked));
    }

    #[test]
    fn test_queue_limits_and_payload_too_large() {
        let id_a = Identity::generate();
        let id_b = Identity::generate();
        let key_provider = Arc::new(InjectedKeyProvider::random(1));
        let limits = OutboxLimits {
            max_records: 2,
            max_record_bytes: 32,
            max_total_bytes: 1024,
        };

        let sqlite = SqliteOutboxStore::in_memory(key_provider.clone(), limits).unwrap();
        let memory = MemoryOutboxStore::new(limits);
        let mut manager = OutboxManager::new(id_a.peer_id(), sqlite, memory, limits);

        // Oversized payload
        let large_body = vec![0x42; 64];
        let res = manager.send_to(id_b.peer_id(), large_body, MessageMode::Normal, None, 100);
        assert_eq!(
            res,
            SendResult::Rejected(SendRejection::PayloadTooLarge {
                max: 32,
                actual: 64
            })
        );

        // Fill queue to capacity (2 items)
        let r1 = manager.send_to(
            id_b.peer_id(),
            b"item 1".to_vec(),
            MessageMode::Normal,
            None,
            100,
        );
        assert!(matches!(r1, SendResult::Accepted { .. }));
        let r2 = manager.send_to(
            id_b.peer_id(),
            b"item 2".to_vec(),
            MessageMode::Normal,
            None,
            100,
        );
        assert!(matches!(r2, SendResult::Accepted { .. }));

        // 3rd item rejected as queue full
        let r3 = manager.send_to(
            id_b.peer_id(),
            b"item 3".to_vec(),
            MessageMode::Normal,
            None,
            100,
        );
        assert_eq!(
            r3,
            SendResult::Rejected(SendRejection::QueueFull { current: 2, max: 2 })
        );
    }

    #[test]
    fn test_restart_and_crash_recovery_from_file() {
        let temp_dir = std::env::temp_dir().join(format!("bitchat_test_{}", rand::random::<u64>()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("outbox.db");

        let id_a = Identity::generate();
        let id_b = Identity::generate();
        let raw_key = [7u8; 32];
        let key_provider = Arc::new(InjectedKeyProvider::new(raw_key, 1));
        let limits = OutboxLimits::default();

        let logical_id;
        let payload = b"Crash recovery test payload".to_vec();

        // 1. First run: enqueue record and mark InFlight
        {
            let sqlite = SqliteOutboxStore::open(&db_path, key_provider.clone(), limits).unwrap();
            let memory = MemoryOutboxStore::new(limits);
            let mut manager = OutboxManager::new(id_a.peer_id(), sqlite, memory, limits);

            logical_id = match manager.send_to(
                id_b.peer_id(),
                payload.clone(),
                MessageMode::Normal,
                None,
                100,
            ) {
                SendResult::Accepted { logical_id, .. } => logical_id,
                SendResult::Rejected(e) => panic!("Rejected: {e:?}"),
            };

            manager.mark_in_flight(&logical_id, 105).unwrap();
        }

        // 2. Restart: simulate crash/restart by opening a new manager from the existing database file
        {
            let sqlite = SqliteOutboxStore::open(&db_path, key_provider.clone(), limits).unwrap();
            let memory = MemoryOutboxStore::new(limits);
            let manager = OutboxManager::new(id_a.peer_id(), sqlite, memory, limits);

            let pending = manager.get_pending(None).unwrap();
            assert_eq!(pending.len(), 1);
            let recovered = &pending[0];
            assert_eq!(recovered.logical_id, logical_id);
            assert_eq!(recovered.body, payload);
            assert_eq!(recovered.state, DeliveryState::InFlight);
            assert_eq!(recovered.attempt_count, 1);
            assert_eq!(recovered.last_attempt_at, Some(105));
        }

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_cancellation_and_terminal_immutability() {
        let id_a = Identity::generate();
        let id_b = Identity::generate();
        let key_provider = Arc::new(InjectedKeyProvider::random(1));
        let limits = OutboxLimits::default();

        let sqlite = SqliteOutboxStore::in_memory(key_provider.clone(), limits).unwrap();
        let memory = MemoryOutboxStore::new(limits);
        let mut manager = OutboxManager::new(id_a.peer_id(), sqlite, memory, limits);

        let logical_id = match manager.send_to(
            id_b.peer_id(),
            b"cancel me".to_vec(),
            MessageMode::Normal,
            None,
            100,
        ) {
            SendResult::Accepted { logical_id, .. } => logical_id,
            SendResult::Rejected(e) => panic!("Rejected: {e:?}"),
        };

        // Cancel
        manager.mark_cancelled(&logical_id).unwrap();
        let rec = manager.get(&logical_id).unwrap().unwrap();
        assert_eq!(rec.state, DeliveryState::Cancelled);

        // Attempting to transition to InFlight must be ignored (terminal immutability)
        manager.mark_in_flight(&logical_id, 110).unwrap();
        let rec_after = manager.get(&logical_id).unwrap().unwrap();
        assert_eq!(rec_after.state, DeliveryState::Cancelled);
    }

    #[test]
    fn test_expiry_pruning() {
        let id_a = Identity::generate();
        let id_b = Identity::generate();
        let key_provider = Arc::new(InjectedKeyProvider::random(1));
        let limits = OutboxLimits::default();

        let sqlite = SqliteOutboxStore::in_memory(key_provider.clone(), limits).unwrap();
        let memory = MemoryOutboxStore::new(limits);
        let mut manager = OutboxManager::new(id_a.peer_id(), sqlite, memory, limits);

        // Record 1: expires at t=200
        let r1 = manager.send_to(
            id_b.peer_id(),
            b"msg1".to_vec(),
            MessageMode::Normal,
            Some(Duration::from_secs(100)),
            100,
        );
        let id1 = match r1 {
            SendResult::Accepted { logical_id, .. } => logical_id,
            SendResult::Rejected(e) => panic!("{e:?}"),
        };

        // Record 2: expires at t=500
        let r2 = manager.send_to(
            id_b.peer_id(),
            b"msg2".to_vec(),
            MessageMode::Normal,
            Some(Duration::from_secs(400)),
            100,
        );
        let id2 = match r2 {
            SendResult::Accepted { logical_id, .. } => logical_id,
            SendResult::Rejected(e) => panic!("{e:?}"),
        };

        // At t=250: Record 1 expired, Record 2 still pending
        let pruned = manager.prune_expired(250).unwrap();
        assert_eq!(pruned, 1);

        assert_eq!(
            manager.get(&id1).unwrap().unwrap().state,
            DeliveryState::Expired
        );
        assert_eq!(
            manager.get(&id2).unwrap().unwrap().state,
            DeliveryState::Queued
        );
    }
}
