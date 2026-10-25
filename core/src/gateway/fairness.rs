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
    global_data_bytes_in_window: usize,
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
            global_data_bytes_in_window: 0,
            peer_buckets: HashMap::new(),
            current_queue_len: 0,
            max_queue_len,
        }
    }

    /// Check and record admission of an inbound envelope.
    ///
    /// Quota units:
    /// - Global bandwidth: bytes per second (window = 1s, limit = 64 KiB/s).
    /// - Per-peer bandwidth: bytes per minute (window = 60s, limit = 8 KiB/s * 60 = 480 KiB/min).
    /// - Per-peer envelope rate: envelopes per minute (window = 60s, limit = 16 env/min).
    /// - Control plane reservation: 25% of queue depth and 25% of bandwidth (16 KiB/s)
    ///   strictly reserved for Handshake and Ack traffic to prevent starvation.
    pub fn check_and_record(
        &mut self,
        peer_id: &PeerId,
        msg_type: MsgType,
        byte_len: usize,
        now: Instant,
    ) -> Result<(), QuotaRejection> {
        let is_control = matches!(msg_type, MsgType::Handshake | MsgType::Ack);

        // 1. Control plane queue reservation check
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
            self.global_data_bytes_in_window = 0;
        }

        // Bandwidth reservation: non-control traffic is capped at (1.0 - 0.25) * 64KB = 48KB/s
        let max_data_bytes =
            (GLOBAL_BANDWIDTH_LIMIT_BPS as f32 * (1.0 - CONTROL_PLANE_RESERVATION_RATIO)) as usize;
        if !is_control && self.global_data_bytes_in_window + byte_len > max_data_bytes {
            return Err(QuotaRejection::ControlCapacityReserved);
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

        if !is_control && bucket.envelopes_in_window >= PEER_ENVELOPE_LIMIT_PER_MIN {
            return Err(QuotaRejection::PeerEnvelopeRateExceeded);
        }

        if bucket.bytes_in_window + byte_len > PEER_BANDWIDTH_LIMIT_BPS * 60 {
            return Err(QuotaRejection::PeerBandwidthExceeded);
        }

        // Admitted: update quotas
        self.global_bytes_in_window += byte_len;
        if !is_control {
            self.global_data_bytes_in_window += byte_len;
            bucket.envelopes_in_window += 1;
        }
        bucket.bytes_in_window += byte_len;
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

        // Handshake and Ack control traffic CAN still enter reserved capacity
        let ctrl_peer1 = PeerId([77u8; 32]);
        let ctrl1 = limiter.check_and_record(&ctrl_peer1, MsgType::Handshake, 100, now);
        assert!(ctrl1.is_ok());

        let ctrl_peer2 = PeerId([78u8; 32]);
        let ctrl2 = limiter.check_and_record(&ctrl_peer2, MsgType::Ack, 100, now);
        assert!(ctrl2.is_ok());
    }

    #[test]
    fn test_control_plane_reservation_prevents_bandwidth_starvation() {
        let mut limiter = GatewayRateLimiter::new(1000);
        let now = Instant::now();
        let peer1 = PeerId([10u8; 32]);

        // Non-control (Chat) data can fill up to 75% of 64 KB/s = 48 KB/s
        assert!(limiter
            .check_and_record(&peer1, MsgType::Chat, 48 * 1024, now)
            .is_ok());

        // Any further Chat data in this 1s window is rejected to preserve 25% bandwidth for control
        let peer2 = PeerId([11u8; 32]);
        let rej_chat = limiter.check_and_record(&peer2, MsgType::Chat, 1024, now);
        assert_eq!(rej_chat, Err(QuotaRejection::ControlCapacityReserved));

        // Control and ACK traffic CAN still use the reserved 16 KB/s bandwidth
        let ack_peer = PeerId([12u8; 32]);
        let ack_res = limiter.check_and_record(&ack_peer, MsgType::Ack, 2 * 1024, now);
        assert!(ack_res.is_ok());

        let hs_peer = PeerId([13u8; 32]);
        let hs_res = limiter.check_and_record(&hs_peer, MsgType::Handshake, 14 * 1024, now);
        assert!(hs_res.is_ok());

        // When all 64 KB is exhausted (48K data + 16K control), global limit triggers
        let rej_global = limiter.check_and_record(&ack_peer, MsgType::Ack, 1024, now);
        assert_eq!(rej_global, Err(QuotaRejection::GlobalBandwidthExceeded));
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

        // Send 48 KB Chat + 16 KB Handshake = 64 KB total
        assert!(limiter
            .check_and_record(&peer, MsgType::Chat, 48 * 1024, now)
            .is_ok());
        assert!(limiter
            .check_and_record(&peer, MsgType::Handshake, 16 * 1024, now)
            .is_ok());

        // Attempting to exceed 64 KB/s total
        let rej = limiter.check_and_record(&peer, MsgType::Handshake, 10 * 1024, now);
        assert_eq!(rej, Err(QuotaRejection::GlobalBandwidthExceeded));

        // After 1 second window resets
        let later = now + Duration::from_millis(1100);
        assert!(limiter
            .check_and_record(&peer, MsgType::Chat, 10 * 1024, later)
            .is_ok());
    }
}
