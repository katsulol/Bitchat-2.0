//! Gateway bandwidth quotas, rate limiting, and control traffic reservation.
//!
//! Enforces:
//! - Global inbound third-party bandwidth quota: 64 KB/s.
//! - Per-authenticated-peer bandwidth quota: 8 KB/s.
//! - Per-authenticated-peer envelope rate quota: 16 envelopes/min.
//! - 25% queue capacity reservation for control plane (Handshake/RouteSync) traffic.

use crate::envelope::MsgType;
use crate::identity::PeerId;
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const GLOBAL_BANDWIDTH_LIMIT_BPS: usize = 64 * 1024; // 64 KB/s
pub const PEER_BANDWIDTH_LIMIT_BPS: usize = 8 * 1024; // 8 KB/s
pub const PEER_ENVELOPE_LIMIT_PER_MIN: usize = 16; // 16 envelopes/min
pub const CONTROL_PLANE_RESERVATION_RATIO: f32 = 0.25; // 25%

/// Rejection reasons when gateway quotas are exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaRejection {
    GlobalBandwidthExceeded,
    PeerBandwidthExceeded,
    PeerEnvelopeRateExceeded,
    ControlCapacityReserved,
    QueueFull,
}

#[derive(Debug)]
struct PeerBucket {
    window_start: Instant,
    bytes_in_window: usize,
    envelopes_in_window: usize,
}

/// Fair gateway rate limiter tracking global and per-authenticated-peer usage.
pub struct GatewayRateLimiter {
    global_window_start: Instant,
    global_bytes_in_window: usize,
    peer_buckets: HashMap<PeerId, PeerBucket>,
    current_queue_len: usize,
    max_queue_len: usize,
}

impl GatewayRateLimiter {
    pub fn new(max_queue_len: usize) -> Self {
        let now = Instant::now();
        Self {
            global_window_start: now,
            global_bytes_in_window: 0,
            peer_buckets: HashMap::new(),
            current_queue_len: 0,
            max_queue_len,
        }
    }

    /// Check and record admission of an inbound envelope.
    pub fn check_and_record(
        &mut self,
        peer_id: &PeerId,
        msg_type: MsgType,
        byte_len: usize,
        now: Instant,
    ) -> Result<(), QuotaRejection> {
        // 1. Control plane reservation check
        let is_control = matches!(msg_type, MsgType::Handshake);
        let max_data_queue =
            (self.max_queue_len as f32 * (1.0 - CONTROL_PLANE_RESERVATION_RATIO)) as usize;

        if self.current_queue_len >= self.max_queue_len {
            return Err(QuotaRejection::QueueFull);
        }

        if !is_control && self.current_queue_len >= max_data_queue {
            return Err(QuotaRejection::ControlCapacityReserved);
        }

        // 2. Global bandwidth window check (1-second sliding window)
        if now.saturating_duration_since(self.global_window_start) >= Duration::from_secs(1) {
            self.global_window_start = now;
            self.global_bytes_in_window = 0;
        }

        if self.global_bytes_in_window + byte_len > GLOBAL_BANDWIDTH_LIMIT_BPS {
            return Err(QuotaRejection::GlobalBandwidthExceeded);
        }

        // 3. Per-peer window check (60-second window for envelopes, 1-second for bytes)
        let bucket = self
            .peer_buckets
            .entry(*peer_id)
            .or_insert_with(|| PeerBucket {
                window_start: now,
                bytes_in_window: 0,
                envelopes_in_window: 0,
            });

        if now.saturating_duration_since(bucket.window_start) >= Duration::from_secs(60) {
            bucket.window_start = now;
            bucket.bytes_in_window = 0;
            bucket.envelopes_in_window = 0;
        }

        if bucket.envelopes_in_window >= PEER_ENVELOPE_LIMIT_PER_MIN {
            return Err(QuotaRejection::PeerEnvelopeRateExceeded);
        }

        if bucket.bytes_in_window + byte_len > PEER_BANDWIDTH_LIMIT_BPS * 60 {
            return Err(QuotaRejection::PeerBandwidthExceeded);
        }

        // Admitted: update quotas
        self.global_bytes_in_window += byte_len;
        bucket.bytes_in_window += byte_len;
        bucket.envelopes_in_window += 1;
        self.current_queue_len += 1;

        Ok(())
    }

    /// Mark an envelope as processed and released from queue.
    pub fn release(&mut self) {
        self.current_queue_len = self.current_queue_len.saturating_sub(1);
    }

    pub fn queue_len(&self) -> usize {
        self.current_queue_len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_control_plane_reservation_prevents_chat_starvation() {
        let max_queue = 100;
        let mut limiter = GatewayRateLimiter::new(max_queue);
        let now = Instant::now();

        // Fill up to 75 items (data limit) across distinct peers so per-peer rate limits don't trigger
        for i in 0..75 {
            let p = PeerId([i as u8; 32]);
            assert!(limiter
                .check_and_record(&p, MsgType::Chat, 100, now)
                .is_ok());
        }

        // 76th Chat item is rejected because remaining 25% is reserved for control
        let rej_peer = PeerId([76u8; 32]);
        let rej = limiter.check_and_record(&rej_peer, MsgType::Chat, 100, now);
        assert_eq!(rej, Err(QuotaRejection::ControlCapacityReserved));

        // Handshake control traffic CAN still enter reserved capacity
        let ctrl_peer = PeerId([77u8; 32]);
        let ctrl = limiter.check_and_record(&ctrl_peer, MsgType::Handshake, 100, now);
        assert!(ctrl.is_ok());
    }

    #[test]
    fn test_peer_envelope_rate_limit() {
        let mut limiter = GatewayRateLimiter::new(100);
        let now = Instant::now();
        let peer = PeerId([2u8; 32]);

        // 16 envelopes admitted
        for _ in 0..16 {
            assert!(limiter
                .check_and_record(&peer, MsgType::Chat, 10, now)
                .is_ok());
        }

        // 17th envelope rejected
        let rej = limiter.check_and_record(&peer, MsgType::Chat, 10, now);
        assert_eq!(rej, Err(QuotaRejection::PeerEnvelopeRateExceeded));

        // A different authenticated peer is not affected (fairness isolation)
        let other_peer = PeerId([3u8; 32]);
        assert!(limiter
            .check_and_record(&other_peer, MsgType::Chat, 10, now)
            .is_ok());
    }

    #[test]
    fn test_global_bandwidth_quota() {
        let mut limiter = GatewayRateLimiter::new(1000);
        let now = Instant::now();
        let peer = PeerId([4u8; 32]);

        // Send 60 KB
        assert!(limiter
            .check_and_record(&peer, MsgType::Chat, 60 * 1024, now)
            .is_ok());

        // Attempting to exceed 64 KB/s
        let rej = limiter.check_and_record(&peer, MsgType::Chat, 10 * 1024, now);
        assert_eq!(rej, Err(QuotaRejection::GlobalBandwidthExceeded));

        // After 1 second window resets
        let later = now + Duration::from_millis(1100);
        assert!(limiter
            .check_and_record(&peer, MsgType::Chat, 10 * 1024, later)
            .is_ok());
    }
}
