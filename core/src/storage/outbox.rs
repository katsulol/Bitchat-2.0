//! Outbox domain model, traits, and admission control.

use crate::identity::PeerId;
use crate::mesh::delivery::DeliveryState;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Operating profile for message delivery and storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageMode {
    /// Durable encrypted storage in SQLite; default TTL 24h; up to 10 retry attempts.
    Normal = 0,
    /// Volatile heap storage only (zero-disk guarantee); strict TTL 15m; up to 5 retry attempts.
    Privacy = 1,
}

impl MessageMode {
    /// Default time-to-live for this mode.
    pub fn default_ttl(&self) -> Duration {
        match self {
            MessageMode::Normal => Duration::from_secs(24 * 3600),
            MessageMode::Privacy => Duration::from_secs(15 * 60),
        }
    }

    /// Hard upper bound on time-to-live.
    pub fn max_ttl(&self) -> Duration {
        match self {
            MessageMode::Normal => Duration::from_secs(7 * 24 * 3600), // 7 days
            MessageMode::Privacy => Duration::from_secs(60 * 60),      // 1 hour
        }
    }

    /// Maximum delivery retry attempts.
    pub fn max_attempts(&self) -> u32 {
        match self {
            MessageMode::Normal => 10,
            MessageMode::Privacy => 5,
        }
    }
}

/// Outbox record holding application message plaintext inside the protected boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxRecord {
    pub logical_id: [u8; 16],
    pub sender: PeerId,
    pub recipient: PeerId,
    pub body: Vec<u8>,
    pub mode: MessageMode,
    pub created_at: u64,
    pub expires_at: u64,
    pub state: DeliveryState,
    pub attempt_count: u32,
    pub last_attempt_at: Option<u64>,
}

/// Explicit typed rejections for send admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendRejection {
    QueueFull { current: usize, max: usize },
    TotalBytesExceeded { current: usize, max: usize },
    PayloadTooLarge { max: usize, actual: usize },
    InvalidRecipient(String),
    StorageLocked,
    DiskFull,
    StorageError(String),
}

impl std::fmt::Display for SendRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendRejection::QueueFull { current, max } => {
                write!(f, "Outbox queue full: {current}/{max}")
            }
            SendRejection::TotalBytesExceeded { current, max } => {
                write!(f, "Outbox byte capacity exceeded: {current}/{max}")
            }
            SendRejection::PayloadTooLarge { max, actual } => {
                write!(f, "Payload too large: {actual} > max {max}")
            }
            SendRejection::InvalidRecipient(reason) => write!(f, "Invalid recipient: {reason}"),
            SendRejection::StorageLocked => write!(f, "Storage keystore is locked"),
            SendRejection::DiskFull => write!(f, "Disk space full"),
            SendRejection::StorageError(err) => write!(f, "Storage error: {err}"),
        }
    }
}

impl std::error::Error for SendRejection {}

/// Result returned from `send_to` admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendResult {
    Accepted {
        logical_id: [u8; 16],
        state: DeliveryState,
    },
    Rejected(SendRejection),
}

/// Storage bounds configuration.
#[derive(Debug, Clone, Copy)]
pub struct OutboxLimits {
    pub max_records: usize,
    pub max_record_bytes: usize,
    pub max_total_bytes: usize,
}

impl Default for OutboxLimits {
    fn default() -> Self {
        Self {
            max_records: 1_000,
            max_record_bytes: 64 * 1024,       // 64 KB
            max_total_bytes: 10 * 1024 * 1024, // 10 MB
        }
    }
}

/// Abstract bounded storage contract for the outbox.
pub trait OutboxStore: Send + Sync {
    /// Enqueue a validated message record.
    fn enqueue(&mut self, record: OutboxRecord) -> Result<(), SendRejection>;

    /// Update record to InFlight upon transmission attempt.
    fn mark_in_flight(&mut self, logical_id: &[u8; 16], now: u64) -> Result<(), SendRejection>;

    /// Mark record delivered (terminal state).
    fn mark_delivered(&mut self, logical_id: &[u8; 16]) -> Result<(), SendRejection>;

    /// Mark record failed (terminal state).
    fn mark_failed(&mut self, logical_id: &[u8; 16]) -> Result<(), SendRejection>;

    /// Mark record cancelled (terminal state).
    fn mark_cancelled(&mut self, logical_id: &[u8; 16]) -> Result<(), SendRejection>;

    /// Retrieve a record by logical ID.
    fn get(&self, logical_id: &[u8; 16]) -> Result<Option<OutboxRecord>, SendRejection>;

    /// Get all pending (Queued or InFlight) records, optionally filtered by recipient.
    fn get_pending(&self, recipient: Option<&PeerId>) -> Result<Vec<OutboxRecord>, SendRejection>;

    /// Prune expired records. Returns number of pruned records.
    fn prune_expired(&mut self, now: u64) -> Result<usize, SendRejection>;

    /// Delete a record by logical ID.
    fn delete(&mut self, logical_id: &[u8; 16]) -> Result<bool, SendRejection>;

    /// Number of records currently stored.
    fn record_count(&self) -> usize;

    /// Total plaintext payload bytes currently buffered.
    fn total_bytes(&self) -> usize;
}
