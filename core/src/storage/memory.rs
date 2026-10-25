//! In-memory bounded outbox store.
//!
//! Provides volatile, zero-disk retention for Privacy Mode and unit testing.

use crate::identity::PeerId;
use crate::mesh::delivery::DeliveryState;
use crate::storage::outbox::{OutboxLimits, OutboxRecord, OutboxStore, SendRejection};
use std::collections::HashMap;

/// Bounded heap-only store for outbox records.
pub struct MemoryOutboxStore {
    records: HashMap<[u8; 16], OutboxRecord>,
    limits: OutboxLimits,
    total_bytes: usize,
}

impl MemoryOutboxStore {
    pub fn new(limits: OutboxLimits) -> Self {
        Self {
            records: HashMap::new(),
            limits,
            total_bytes: 0,
        }
    }
}

impl Default for MemoryOutboxStore {
    fn default() -> Self {
        Self::new(OutboxLimits::default())
    }
}

impl OutboxStore for MemoryOutboxStore {
    fn enqueue(&mut self, record: OutboxRecord) -> Result<(), SendRejection> {
        if record.body.len() > self.limits.max_record_bytes {
            return Err(SendRejection::PayloadTooLarge {
                max: self.limits.max_record_bytes,
                actual: record.body.len(),
            });
        }
        if self.records.contains_key(&record.logical_id) {
            return Ok(());
        }
        if self.records.len() >= self.limits.max_records {
            return Err(SendRejection::QueueFull {
                current: self.records.len(),
                max: self.limits.max_records,
            });
        }
        if self.total_bytes + record.body.len() > self.limits.max_total_bytes {
            return Err(SendRejection::TotalBytesExceeded {
                current: self.total_bytes,
                max: self.limits.max_total_bytes,
            });
        }

        self.total_bytes += record.body.len();
        self.records.insert(record.logical_id, record);
        Ok(())
    }

    fn mark_in_flight(&mut self, logical_id: &[u8; 16], now: u64) -> Result<(), SendRejection> {
        let rec = self
            .records
            .get_mut(logical_id)
            .ok_or_else(|| SendRejection::StorageError("Record not found".into()))?;

        if rec.state.is_terminal() {
            return Ok(());
        }

        rec.state = DeliveryState::InFlight;
        rec.attempt_count = rec.attempt_count.saturating_add(1);
        rec.last_attempt_at = Some(now);
        Ok(())
    }

    fn mark_delivered(&mut self, logical_id: &[u8; 16]) -> Result<(), SendRejection> {
        let rec = self
            .records
            .get_mut(logical_id)
            .ok_or_else(|| SendRejection::StorageError("Record not found".into()))?;

        rec.state = DeliveryState::Delivered;
        Ok(())
    }

    fn mark_failed(&mut self, logical_id: &[u8; 16]) -> Result<(), SendRejection> {
        let rec = self
            .records
            .get_mut(logical_id)
            .ok_or_else(|| SendRejection::StorageError("Record not found".into()))?;

        rec.state = DeliveryState::Failed;
        Ok(())
    }

    fn mark_cancelled(&mut self, logical_id: &[u8; 16]) -> Result<(), SendRejection> {
        let rec = self
            .records
            .get_mut(logical_id)
            .ok_or_else(|| SendRejection::StorageError("Record not found".into()))?;

        rec.state = DeliveryState::Cancelled;
        Ok(())
    }

    fn get(&self, logical_id: &[u8; 16]) -> Result<Option<OutboxRecord>, SendRejection> {
        Ok(self.records.get(logical_id).cloned())
    }

    fn get_pending(&self, recipient: Option<&PeerId>) -> Result<Vec<OutboxRecord>, SendRejection> {
        let mut list: Vec<_> = self
            .records
            .values()
            .filter(|r| !r.state.is_terminal())
            .filter(|r| recipient.is_none_or(|p| r.recipient == *p))
            .cloned()
            .collect();
        list.sort_by_key(|r| r.created_at);
        Ok(list)
    }

    fn prune_expired(&mut self, now: u64) -> Result<usize, SendRejection> {
        let mut expired_keys = Vec::new();
        for (id, rec) in &mut self.records {
            if now >= rec.expires_at && !rec.state.is_terminal() {
                rec.state = DeliveryState::Expired;
                expired_keys.push(*id);
            }
        }
        Ok(expired_keys.len())
    }

    fn delete(&mut self, logical_id: &[u8; 16]) -> Result<bool, SendRejection> {
        if let Some(rec) = self.records.remove(logical_id) {
            self.total_bytes = self.total_bytes.saturating_sub(rec.body.len());
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn record_count(&self) -> usize {
        self.records.len()
    }

    fn total_bytes(&self) -> usize {
        self.total_bytes
    }
}
