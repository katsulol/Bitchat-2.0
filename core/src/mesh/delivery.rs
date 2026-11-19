// CRYPTO-REVIEW:
// What it does: Logical Message Delivery Protocol, Application Framing, Authenticated ACKs,
// and Destination Display Deduplication.
// Primitives / crates: `blake3`, `ed25519-dalek`, `rand`.
// Reviewer checklist:
// 1. Logical message ID is 16 bytes generated once and bound inside authenticated application frames.
// 2. Application frames are decrypted and authenticated under Noise AEAD before parsing.
// 3. ApplicationFrame::Ack is authenticated by the intended recipient's session; gateways cannot forge ACKs.
// 4. Duplicate received messages trigger a fresh ACK under the active session without duplicate display.
// 5. Terminal states (Delivered, Failed, Expired, Cancelled) cannot be downgraded or resurrected by late ACKs.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::identity::{PeerId, ShortId};
use crate::mesh::router::{ObservedTransport, RouteReason};
use crate::storage::{MessageMode, OutboxRecord};

pub type LogicalMessageId = [u8; 16];

pub fn generate_logical_id() -> LogicalMessageId {
    rand::random()
}

pub const APP_FRAME_VERSION: u8 = 1;
pub const APP_FRAME_HEADER_LEN: usize = 102; // Fixed application frame header bytes
pub const DEFAULT_MAX_APP_BODY: usize = 4096;

/// Kind of authenticated application frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ApplicationFrameKind {
    Message = 1,
    Ack = 2,
}

impl ApplicationFrameKind {
    pub fn from_u8(v: u8) -> Result<Self> {
        match v {
            1 => Ok(ApplicationFrameKind::Message),
            2 => Ok(ApplicationFrameKind::Ack),
            other => Err(Error::Crypto(format!(
                "Invalid ApplicationFrameKind: {other}"
            ))),
        }
    }
}

/// Logical message delivery states with strict terminal transitions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeliveryState {
    Queued,
    InFlight,
    Delivered,
    Failed,
    Expired,
    Cancelled,
}

impl DeliveryState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            DeliveryState::Delivered
                | DeliveryState::Failed
                | DeliveryState::Expired
                | DeliveryState::Cancelled
        )
    }
}

/// End-to-end encrypted and authenticated application frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationFrame {
    pub version: u8,
    pub kind: ApplicationFrameKind,
    pub logical_id: LogicalMessageId,
    pub sender_peer_id: PeerId,
    pub recipient_peer_id: PeerId,
    pub timestamp_monotonic: u64,
    pub expiry_monotonic: u64,
    pub body: Vec<u8>,
}

impl ApplicationFrame {
    pub fn new_message(
        logical_id: LogicalMessageId,
        sender_peer_id: PeerId,
        recipient_peer_id: PeerId,
        body: Vec<u8>,
        ttl: Duration,
        now_secs: u64,
    ) -> Self {
        Self {
            version: APP_FRAME_VERSION,
            kind: ApplicationFrameKind::Message,
            logical_id,
            sender_peer_id,
            recipient_peer_id,
            timestamp_monotonic: now_secs,
            expiry_monotonic: now_secs.saturating_add(ttl.as_secs()),
            body,
        }
    }

    pub fn new_ack(
        logical_id: LogicalMessageId,
        sender_peer_id: PeerId,
        recipient_peer_id: PeerId,
        now_secs: u64,
    ) -> Self {
        Self {
            version: APP_FRAME_VERSION,
            kind: ApplicationFrameKind::Ack,
            logical_id,
            sender_peer_id,
            recipient_peer_id,
            timestamp_monotonic: now_secs,
            expiry_monotonic: now_secs.saturating_add(300),
            body: Vec::new(),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let total_len = APP_FRAME_HEADER_LEN + self.body.len();
        let mut buf = Vec::with_capacity(total_len);

        buf.push(self.version);
        buf.push(self.kind as u8);
        buf.extend_from_slice(&[0u8; 2]); // Reserved
        buf.extend_from_slice(&self.logical_id);
        buf.extend_from_slice(self.sender_peer_id.as_bytes());
        buf.extend_from_slice(self.recipient_peer_id.as_bytes());
        buf.extend_from_slice(&self.timestamp_monotonic.to_be_bytes());
        buf.extend_from_slice(&self.expiry_monotonic.to_be_bytes());
        buf.extend_from_slice(&(self.body.len() as u16).to_be_bytes());
        buf.extend_from_slice(&self.body);

        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < APP_FRAME_HEADER_LEN {
            return Err(Error::Crypto(format!(
                "ApplicationFrame buffer too short: {} < {}",
                buf.len(),
                APP_FRAME_HEADER_LEN
            )));
        }

        let version = buf[0];
        if version != APP_FRAME_VERSION {
            return Err(Error::Crypto(format!(
                "Unsupported ApplicationFrame version: {version}"
            )));
        }

        let kind = ApplicationFrameKind::from_u8(buf[1])?;

        let mut logical_id = [0u8; 16];
        logical_id.copy_from_slice(&buf[4..20]);

        let mut sender_bytes = [0u8; 32];
        sender_bytes.copy_from_slice(&buf[20..52]);
        let sender_peer_id = PeerId(sender_bytes);

        let mut recipient_bytes = [0u8; 32];
        recipient_bytes.copy_from_slice(&buf[52..84]);
        let recipient_peer_id = PeerId(recipient_bytes);

        let mut ts_bytes = [0u8; 8];
        ts_bytes.copy_from_slice(&buf[84..92]);
        let timestamp_monotonic = u64::from_be_bytes(ts_bytes);

        let mut exp_bytes = [0u8; 8];
        exp_bytes.copy_from_slice(&buf[92..100]);
        let expiry_monotonic = u64::from_be_bytes(exp_bytes);
        let body_len = u16::from_be_bytes([buf[100], buf[101]]) as usize;

        if buf.len() < APP_FRAME_HEADER_LEN + body_len {
            return Err(Error::Crypto(format!(
                "ApplicationFrame body truncated: expected {} bytes, got {}",
                body_len,
                buf.len() - APP_FRAME_HEADER_LEN
            )));
        }

        let body = buf[APP_FRAME_HEADER_LEN..APP_FRAME_HEADER_LEN + body_len].to_vec();

        Ok(Self {
            version,
            kind,
            logical_id,
            sender_peer_id,
            recipient_peer_id,
            timestamp_monotonic,
            expiry_monotonic,
            body,
        })
    }
}

/// Deduplication outcome for an inbound application frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DedupStatus {
    /// Newly seen logical message: display to user and send fresh ACK.
    New,
    /// Previously delivered logical message: suppress duplicate display, send fresh ACK.
    Duplicate,
}

/// Endpoint display deduplicator ensuring exactly-once display per (sender, logical_id).
#[derive(Debug, Clone)]
pub struct InboxDeduplicator {
    seen_messages: HashMap<(PeerId, LogicalMessageId), Instant>,
    capacity: usize,
    ttl: Duration,
}

impl Default for InboxDeduplicator {
    fn default() -> Self {
        Self::new(2000, Duration::from_secs(86400)) // 2000 entries, 24-hour TTL
    }
}

impl InboxDeduplicator {
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            seen_messages: HashMap::with_capacity(capacity),
            capacity,
            ttl,
        }
    }

    pub fn check_or_insert(
        &mut self,
        sender: &PeerId,
        logical_id: &LogicalMessageId,
        now: Instant,
    ) -> DedupStatus {
        self.cleanup_stale(now);

        let key = (*sender, *logical_id);
        if let Some(prev_time) = self.seen_messages.get_mut(&key) {
            *prev_time = now;
            DedupStatus::Duplicate
        } else {
            if self.seen_messages.len() >= self.capacity {
                if let Some((&oldest, _)) = self.seen_messages.iter().min_by_key(|(_, t)| *t) {
                    self.seen_messages.remove(&oldest);
                }
            }
            self.seen_messages.insert(key, now);
            DedupStatus::New
        }
    }

    pub fn cleanup_stale(&mut self, now: Instant) {
        let ttl = self.ttl;
        self.seen_messages
            .retain(|_, last_seen| now.saturating_duration_since(*last_seen) <= ttl);
    }
}

/// Tracking record for an outgoing logical message.
#[derive(Debug, Clone)]
pub struct OutgoingRecord {
    pub logical_id: LogicalMessageId,
    pub recipient_peer_id: PeerId,
    pub recipient_short_id: ShortId,
    pub state: DeliveryState,
    pub created_at: Instant,
    pub expires_at: Instant,
    pub expires_at_unix: u64,
    pub ack_timeout: Duration,
    pub attempts: u32,
    pub max_attempts: u32,
    pub handshake_attempts: u32,
    pub max_handshake_attempts: u32,
    pub next_attempt_at: Instant,
    pub body: Vec<u8>,
    pub mode: MessageMode,
    pub is_privacy_mode: bool,
    pub attempted_gateways: Vec<iroh::EndpointId>,
    pub last_gateway: Option<iroh::EndpointId>,
    pub last_reason: RouteReason,
    pub last_transport: ObservedTransport,
    last_emitted: Option<(DeliveryState, RouteReason)>,
}

impl OutgoingRecord {
    /// Remaining duration before hard expiration at `now`.
    pub fn remaining_ttl(&self, now: Instant) -> Duration {
        self.expires_at.saturating_duration_since(now)
    }
}

/// Action produced by the delivery coordinator on timeout check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryAction {
    ReadyToRetry(LogicalMessageId),
    Expired(LogicalMessageId),
    Failed(LogicalMessageId),
}

pub const DEFAULT_MAX_HANDSHAKE_ATTEMPTS: u32 = 5;
pub const DEFAULT_RECONNECT_WAKEUP_COOLDOWN: Duration = Duration::from_secs(5);
pub const DEFAULT_ACK_TIMEOUT: Duration = Duration::from_secs(4);
pub const MAX_CONSECUTIVE_URGENT_DISPATCHES: u32 = 3;
pub const URGENT_DEADLINE_WINDOW: Duration = Duration::from_secs(120);

/// Sender delivery coordinator managing logical message states, retries, and authenticated ACKs.
#[derive(Debug, Clone)]
pub struct DeliveryCoordinator {
    local_peer_id: PeerId,
    records: HashMap<LogicalMessageId, OutgoingRecord>,
    last_reconnect_wakeup: HashMap<ShortId, Instant>,
    reconnect_cooldown: Duration,
    edf_consecutive_urgent: u32,
}

/// Request payload to enqueue a new outgoing logical message.
#[derive(Debug, Clone)]
pub struct EnqueueMessage {
    pub logical_id: LogicalMessageId,
    pub recipient_peer_id: PeerId,
    pub recipient_short_id: ShortId,
    pub body: Vec<u8>,
    pub ttl: Duration,
    pub max_attempts: u32,
    pub is_privacy_mode: bool,
}

impl DeliveryCoordinator {
    pub fn new(local_peer_id: PeerId) -> Self {
        Self {
            local_peer_id,
            records: HashMap::new(),
            last_reconnect_wakeup: HashMap::new(),
            reconnect_cooldown: DEFAULT_RECONNECT_WAKEUP_COOLDOWN,
            edf_consecutive_urgent: 0,
        }
    }

    pub fn enqueue(&mut self, req: EnqueueMessage, now: Instant) {
        let mode = if req.is_privacy_mode {
            MessageMode::Privacy
        } else {
            MessageMode::Normal
        };
        self.enqueue_with_meta(req, mode, 0, DEFAULT_ACK_TIMEOUT, now);
    }

    /// Enqueue with explicit mode, wall-clock expiry, and per-attempt ACK timeout.
    pub fn enqueue_with_meta(
        &mut self,
        req: EnqueueMessage,
        mode: MessageMode,
        expires_at_unix: u64,
        ack_timeout: Duration,
        now: Instant,
    ) {
        let record = OutgoingRecord {
            logical_id: req.logical_id,
            recipient_peer_id: req.recipient_peer_id,
            recipient_short_id: req.recipient_short_id,
            state: DeliveryState::Queued,
            created_at: now,
            expires_at: now + req.ttl,
            expires_at_unix,
            ack_timeout,
            attempts: 0,
            max_attempts: req.max_attempts,
            handshake_attempts: 0,
            max_handshake_attempts: DEFAULT_MAX_HANDSHAKE_ATTEMPTS,
            next_attempt_at: now,
            body: req.body,
            mode,
            is_privacy_mode: req.is_privacy_mode || mode == MessageMode::Privacy,
            attempted_gateways: Vec::new(),
            last_gateway: None,
            last_reason: RouteReason::NoRouteAvailable,
            last_transport: ObservedTransport::Unknown,
            last_emitted: None,
        };
        self.records.insert(req.logical_id, record);
    }

    /// Rehydrate a pending `OutboxRecord` from persistent storage after restart using
    /// conservative remaining TTL (bounded by original `expires_at - created_at` against clock rollback).
    pub fn rehydrate_record(
        &mut self,
        rec: OutboxRecord,
        now_instant: Instant,
        now_secs: u64,
        ack_timeout: Duration,
    ) -> Option<DeliveryAction> {
        if rec.state.is_terminal() {
            return None;
        }
        let original_ttl_secs = rec.expires_at.saturating_sub(rec.created_at);
        if now_secs >= rec.expires_at {
            return Some(DeliveryAction::Expired(rec.logical_id));
        }
        let remaining_secs = rec
            .expires_at
            .saturating_sub(now_secs)
            .min(original_ttl_secs);
        if remaining_secs == 0 {
            return Some(DeliveryAction::Expired(rec.logical_id));
        }

        let record = OutgoingRecord {
            logical_id: rec.logical_id,
            recipient_peer_id: rec.recipient,
            recipient_short_id: rec.recipient.short_id(),
            state: DeliveryState::Queued,
            created_at: now_instant,
            expires_at: now_instant + Duration::from_secs(remaining_secs),
            expires_at_unix: rec.expires_at,
            ack_timeout,
            attempts: rec.attempt_count,
            max_attempts: rec.mode.max_attempts(),
            handshake_attempts: 0,
            max_handshake_attempts: DEFAULT_MAX_HANDSHAKE_ATTEMPTS,
            next_attempt_at: now_instant,
            body: rec.body,
            mode: rec.mode,
            is_privacy_mode: rec.mode == MessageMode::Privacy,
            attempted_gateways: Vec::new(),
            last_gateway: None,
            last_reason: RouteReason::NoRouteAvailable,
            last_transport: ObservedTransport::Unknown,
            last_emitted: None,
        };
        self.records.insert(rec.logical_id, record);
        None
    }

    pub fn record_gateway_attempt(
        &mut self,
        logical_id: &LogicalMessageId,
        gateway: iroh::EndpointId,
    ) {
        if let Some(rec) = self.records.get_mut(logical_id) {
            if !rec.attempted_gateways.contains(&gateway) {
                rec.attempted_gateways.push(gateway);
            }
            rec.last_gateway = Some(gateway);
        }
    }

    pub fn clear_attempted_gateways(&mut self, logical_id: &LogicalMessageId) {
        if let Some(rec) = self.records.get_mut(logical_id) {
            rec.attempted_gateways.clear();
        }
    }

    pub fn clear_attempted_gateways_for_peer(&mut self, recipient: &ShortId) {
        for rec in self.records.values_mut() {
            if rec.recipient_short_id == *recipient && !rec.state.is_terminal() {
                rec.attempted_gateways.clear();
            }
        }
    }

    pub fn record_emitted_event(
        &mut self,
        logical_id: &LogicalMessageId,
        state: DeliveryState,
        reason: RouteReason,
    ) -> bool {
        if let Some(rec) = self.records.get_mut(logical_id) {
            let key = (state, reason);
            if rec.last_emitted == Some(key) {
                return false;
            }
            rec.last_emitted = Some(key);
            true
        } else {
            true
        }
    }

    pub fn set_route_meta(
        &mut self,
        logical_id: &LogicalMessageId,
        reason: RouteReason,
        transport: ObservedTransport,
    ) {
        if let Some(rec) = self.records.get_mut(logical_id) {
            rec.last_reason = reason;
            rec.last_transport = transport;
        }
    }

    pub fn has_active_in_flight_for(&self, recipient: &PeerId, now: Instant) -> bool {
        self.records.values().any(|r| {
            r.recipient_peer_id == *recipient
                && r.state == DeliveryState::InFlight
                && now < r.next_attempt_at
        })
    }

    pub fn get_state(&self, logical_id: &LogicalMessageId) -> Option<DeliveryState> {
        self.records.get(logical_id).map(|r| r.state)
    }

    pub fn get_record(&self, logical_id: &LogicalMessageId) -> Option<&OutgoingRecord> {
        self.records.get(logical_id)
    }

    pub fn queued_count(&self) -> usize {
        self.records
            .values()
            .filter(|r| r.state == DeliveryState::Queued)
            .count()
    }

    pub fn in_flight_count(&self) -> usize {
        self.records
            .values()
            .filter(|r| r.state == DeliveryState::InFlight)
            .count()
    }

    pub fn mark_in_flight(
        &mut self,
        logical_id: &LogicalMessageId,
        backoff_delay: Duration,
        now: Instant,
    ) -> Result<()> {
        let record = self
            .records
            .get_mut(logical_id)
            .ok_or_else(|| Error::Crypto(format!("Unknown logical message {logical_id:?}")))?;

        if record.state.is_terminal() {
            return Err(Error::Crypto(format!(
                "Cannot mark terminal message {:?} in-flight",
                record.state
            )));
        }

        record.attempts = record.attempts.saturating_add(1);
        record.state = DeliveryState::InFlight;
        record.next_attempt_at = now + backoff_delay;

        Ok(())
    }

    /// Defer dispatch when no route is available or when waiting on a Noise XX handshake.
    ///
    /// Gate 0.5 invariants:
    /// - `NoRouteAvailable` (`has_route == false`) keeps the message `Queued` and does NOT
    ///   consume `attempts` or `handshake_attempts`.
    /// - Session handshake initiation (`has_route == true`) increments `handshake_attempts`
    ///   (bounded separately by `max_handshake_attempts`) while keeping the message `Queued`
    ///   and preserving the message's `attempts` budget for actual payload transmissions.
    pub fn defer_until_session_or_route(
        &mut self,
        logical_id: &LogicalMessageId,
        has_route: bool,
        backoff_delay: Duration,
        now: Instant,
    ) -> Result<()> {
        let record = self
            .records
            .get_mut(logical_id)
            .ok_or_else(|| Error::Crypto(format!("Unknown logical message {logical_id:?}")))?;

        if record.state.is_terminal() {
            return Err(Error::Crypto(format!(
                "Cannot defer terminal message {:?}",
                record.state
            )));
        }

        if has_route {
            record.handshake_attempts = record.handshake_attempts.saturating_add(1);
        }
        record.state = DeliveryState::Queued;
        record.next_attempt_at = now + backoff_delay;
        Ok(())
    }

    /// Process an authenticated inbound ACK.
    ///
    /// Validates sender identity, recipient binding, and transitions message to Delivered.
    /// Late ACKs for already terminal messages do not alter state.
    pub fn handle_ack(
        &mut self,
        ack: &ApplicationFrame,
        authenticated_sender: &PeerId,
    ) -> Result<bool> {
        if ack.kind != ApplicationFrameKind::Ack {
            return Err(Error::Crypto("Frame is not an Ack".into()));
        }

        // ACK must be cryptographically signed/authenticated by the remote sender
        if ack.sender_peer_id != *authenticated_sender {
            return Err(Error::Crypto(format!(
                "ACK claimed sender {:?} differs from authenticated sender {:?}",
                ack.sender_peer_id, authenticated_sender
            )));
        }

        // ACK must be addressed to local identity
        if ack.recipient_peer_id != self.local_peer_id {
            return Err(Error::Crypto(format!(
                "ACK recipient {:?} is not local peer {:?}",
                ack.recipient_peer_id, self.local_peer_id
            )));
        }

        if let Some(record) = self.records.get_mut(&ack.logical_id) {
            // Validate that the ACK sender is the intended recipient of this message
            if record.recipient_peer_id != *authenticated_sender {
                return Err(Error::Crypto(format!(
                    "ACK sender {:?} does not match intended message recipient {:?}",
                    authenticated_sender, record.recipient_peer_id
                )));
            }

            // Monotonic state transition: only non-terminal messages become Delivered
            if !record.state.is_terminal() {
                record.state = DeliveryState::Delivered;
                return Ok(true);
            }
        }

        Ok(false)
    }

    pub fn cancel(&mut self, logical_id: &LogicalMessageId) -> bool {
        if let Some(record) = self.records.get_mut(logical_id) {
            if !record.state.is_terminal() {
                record.state = DeliveryState::Cancelled;
                return true;
            }
        }
        false
    }

    /// Transition a non-terminal message directly to terminal `Failed` (e.g., recipient blocked).
    pub fn fail(&mut self, logical_id: &LogicalMessageId, reason: RouteReason) -> bool {
        if let Some(record) = self.records.get_mut(logical_id) {
            if !record.state.is_terminal() {
                record.last_reason = reason;
                record.state = DeliveryState::Failed;
                return true;
            }
        }
        false
    }

    /// Immediately fail all non-terminal messages queued/in-flight for a blocked/revoked recipient.
    pub fn fail_for_recipient(
        &mut self,
        recipient: &PeerId,
        reason: RouteReason,
    ) -> Vec<(LogicalMessageId, u32, u64)> {
        let mut failed = Vec::new();
        for record in self.records.values_mut() {
            if record.recipient_peer_id == *recipient && !record.state.is_terminal() {
                record.last_reason = reason;
                record.state = DeliveryState::Failed;
                failed.push((record.logical_id, record.attempts, record.expires_at_unix));
            }
        }
        failed
    }

    /// Wake up any non-terminal messages queued for a verified peer, coalesced by cooldown.
    ///
    /// Gate 0.5 invariants:
    /// - Unverified ShortId link announcements (`is_identity_verified == false`) cannot trigger
    ///   wakeup resets (prevents spoofed beacon wakeup storms).
    /// - Repeated reconnects for the same peer within `reconnect_cooldown` are coalesced.
    /// - Never extends `expires_at` (`TTL`) or resets `attempts`.
    pub fn reset_backoff_for_verified_peer(
        &mut self,
        recipient: &ShortId,
        is_identity_verified: bool,
        now: Instant,
    ) -> bool {
        if !is_identity_verified {
            return false;
        }
        if let Some(&last) = self.last_reconnect_wakeup.get(recipient) {
            if now.saturating_duration_since(last) < self.reconnect_cooldown {
                return false;
            }
        }
        self.last_reconnect_wakeup.insert(*recipient, now);
        for record in self.records.values_mut() {
            if record.recipient_short_id == *recipient && !record.state.is_terminal() {
                record.next_attempt_at = now;
            }
        }
        true
    }

    /// Wake up any non-terminal messages queued for this peer so they attempt immediately.
    pub fn reset_backoff_for_peer(&mut self, recipient: &ShortId, now: Instant) {
        let _ = self.reset_backoff_for_verified_peer(recipient, true, now);
    }

    /// Check timers, advancing expired/exhausted messages to terminal states and emitting
    /// at most one active retry candidate per recipient peer.
    pub fn check_timeouts(&mut self, now: Instant) -> Vec<DeliveryAction> {
        let mut actions = Vec::new();

        // Pass 1: Enforce hard expiry and retry/handshake budget exhaustion first.
        for record in self.records.values_mut() {
            if record.state.is_terminal() {
                continue;
            }
            if now >= record.expires_at {
                record.state = DeliveryState::Expired;
                actions.push(DeliveryAction::Expired(record.logical_id));
            } else if record.attempts >= record.max_attempts
                || record.handshake_attempts >= record.max_handshake_attempts
            {
                record.state = DeliveryState::Failed;
                actions.push(DeliveryAction::Failed(record.logical_id));
            }
        }

        // Pass 2: Identify recipients that already have an active InFlight attempt waiting for ACK.
        let busy_recipients: HashSet<PeerId> = self
            .records
            .values()
            .filter(|r| r.state == DeliveryState::InFlight && now < r.next_attempt_at)
            .map(|r| r.recipient_peer_id)
            .collect();

        // Pass 3: Bounded 3:1 Earliest-Deadline-First scheduling (max 1 active attempt per recipient).
        let mut urgent: Vec<(PeerId, Instant, Instant, LogicalMessageId)> = Vec::new();
        let mut ordinary: Vec<(PeerId, Instant, Instant, LogicalMessageId)> = Vec::new();
        for record in self.records.values() {
            if record.state.is_terminal()
                || busy_recipients.contains(&record.recipient_peer_id)
                || now < record.next_attempt_at
            {
                continue;
            }
            let item = (
                record.recipient_peer_id,
                record.expires_at,
                record.created_at,
                record.logical_id,
            );
            if record.remaining_ttl(now) <= URGENT_DEADLINE_WINDOW {
                urgent.push(item);
            } else {
                ordinary.push(item);
            }
        }

        urgent.sort_by(|a, b| {
            a.1.cmp(&b.1)
                .then_with(|| a.2.cmp(&b.2))
                .then_with(|| a.3.cmp(&b.3))
        });
        ordinary.sort_by(|a, b| {
            a.2.cmp(&b.2)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.3.cmp(&b.3))
        });

        let mut picked_recipients = HashSet::new();
        let mut u_idx = 0usize;
        let mut o_idx = 0usize;
        loop {
            while u_idx < urgent.len() && picked_recipients.contains(&urgent[u_idx].0) {
                u_idx += 1;
            }
            while o_idx < ordinary.len() && picked_recipients.contains(&ordinary[o_idx].0) {
                o_idx += 1;
            }

            let pick_ordinary = if o_idx < ordinary.len()
                && (self.edf_consecutive_urgent >= MAX_CONSECUTIVE_URGENT_DISPATCHES
                    || u_idx >= urgent.len())
            {
                true
            } else if u_idx < urgent.len() {
                false
            } else {
                break;
            };

            if pick_ordinary {
                let (recipient, _, _, logical_id) = ordinary[o_idx];
                o_idx += 1;
                picked_recipients.insert(recipient);
                self.edf_consecutive_urgent = 0;
                actions.push(DeliveryAction::ReadyToRetry(logical_id));
            } else {
                let (recipient, _, _, logical_id) = urgent[u_idx];
                u_idx += 1;
                picked_recipients.insert(recipient);
                self.edf_consecutive_urgent = self.edf_consecutive_urgent.saturating_add(1);
                actions.push(DeliveryAction::ReadyToRetry(logical_id));
            }
        }

        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_application_frame_encoding_roundtrip() {
        let id_a = crate::identity::Identity::generate();
        let id_b = crate::identity::Identity::generate();
        let logical_id = generate_logical_id();
        let body = b"Test application body".to_vec();

        let frame = ApplicationFrame::new_message(
            logical_id,
            id_a.peer_id(),
            id_b.peer_id(),
            body.clone(),
            Duration::from_secs(3600),
            1000,
        );

        let encoded = frame.encode();
        assert_eq!(encoded.len(), APP_FRAME_HEADER_LEN + body.len());

        let decoded = ApplicationFrame::decode(&encoded).unwrap();
        assert_eq!(decoded.version, APP_FRAME_VERSION);
        assert_eq!(decoded.kind, ApplicationFrameKind::Message);
        assert_eq!(decoded.logical_id, logical_id);
        assert_eq!(decoded.sender_peer_id, id_a.peer_id());
        assert_eq!(decoded.recipient_peer_id, id_b.peer_id());
        assert_eq!(decoded.body, body);
    }

    #[test]
    fn test_ack_frame_roundtrip() {
        let id_a = crate::identity::Identity::generate();
        let id_b = crate::identity::Identity::generate();
        let logical_id = generate_logical_id();

        let ack = ApplicationFrame::new_ack(logical_id, id_b.peer_id(), id_a.peer_id(), 2000);
        let encoded = ack.encode();
        let decoded = ApplicationFrame::decode(&encoded).unwrap();

        assert_eq!(decoded.kind, ApplicationFrameKind::Ack);
        assert_eq!(decoded.logical_id, logical_id);
        assert!(decoded.body.is_empty());
    }

    #[test]
    fn test_inbox_deduplicator_exactly_once_display() {
        let mut dedup = InboxDeduplicator::default();
        let sender = crate::identity::Identity::generate().peer_id();
        let logical_id = generate_logical_id();
        let now = Instant::now();

        // 1. First reception: New -> display to user
        let s1 = dedup.check_or_insert(&sender, &logical_id, now);
        assert_eq!(s1, DedupStatus::New);

        // 2. Duplicate reception: Duplicate -> suppress display, still generate ACK
        let s2 = dedup.check_or_insert(&sender, &logical_id, now + Duration::from_secs(1));
        assert_eq!(s2, DedupStatus::Duplicate);

        // 3. Different logical ID from same sender: New
        let other_id = generate_logical_id();
        let s3 = dedup.check_or_insert(&sender, &other_id, now + Duration::from_secs(2));
        assert_eq!(s3, DedupStatus::New);
    }

    #[test]
    fn test_delivery_state_lifecycle_and_terminal_guarantees() {
        let id_local = crate::identity::Identity::generate();
        let id_remote = crate::identity::Identity::generate();
        let s_remote = id_remote.short_id();

        let mut coord = DeliveryCoordinator::new(id_local.peer_id());
        let logical_id = generate_logical_id();
        let now = Instant::now();

        coord.enqueue(
            EnqueueMessage {
                logical_id,
                recipient_peer_id: id_remote.peer_id(),
                recipient_short_id: s_remote,
                body: b"test".to_vec(),
                ttl: Duration::from_secs(60),
                max_attempts: 3,
                is_privacy_mode: false,
            },
            now,
        );
        assert_eq!(coord.get_state(&logical_id), Some(DeliveryState::Queued));

        // Attempt 1 -> InFlight
        coord
            .mark_in_flight(&logical_id, Duration::from_secs(5), now)
            .unwrap();
        assert_eq!(coord.get_state(&logical_id), Some(DeliveryState::InFlight));

        // Valid authenticated ACK arrives -> Delivered
        let ack =
            ApplicationFrame::new_ack(logical_id, id_remote.peer_id(), id_local.peer_id(), 100);
        let delivered = coord.handle_ack(&ack, &id_remote.peer_id()).unwrap();
        assert!(delivered);
        assert_eq!(coord.get_state(&logical_id), Some(DeliveryState::Delivered));

        // Late duplicate ACK cannot downgrade Delivered state
        let late_ack = coord.handle_ack(&ack, &id_remote.peer_id()).unwrap();
        assert!(!late_ack);
        assert_eq!(coord.get_state(&logical_id), Some(DeliveryState::Delivered));

        // Delivered state cannot be marked InFlight again
        let re_err = coord.mark_in_flight(&logical_id, Duration::from_secs(5), now);
        assert!(re_err.is_err());
    }

    #[test]
    fn test_ack_from_wrong_peer_rejected() {
        let id_local = crate::identity::Identity::generate();
        let id_bob = crate::identity::Identity::generate();
        let id_eve = crate::identity::Identity::generate();

        let mut coord = DeliveryCoordinator::new(id_local.peer_id());
        let logical_id = generate_logical_id();
        let now = Instant::now();

        coord.enqueue(
            EnqueueMessage {
                logical_id,
                recipient_peer_id: id_bob.peer_id(),
                recipient_short_id: id_bob.short_id(),
                body: b"confidential".to_vec(),
                ttl: Duration::from_secs(60),
                max_attempts: 3,
                is_privacy_mode: false,
            },
            now,
        );
        coord
            .mark_in_flight(&logical_id, Duration::from_secs(5), now)
            .unwrap();

        // Eve crafts ACK claiming to be for Bob's message
        let eve_ack =
            ApplicationFrame::new_ack(logical_id, id_eve.peer_id(), id_local.peer_id(), 100);

        // Rejected because Eve is not the intended recipient Bob
        let err = coord.handle_ack(&eve_ack, &id_eve.peer_id());
        assert!(err.is_err(), "ACK from non-recipient must be rejected");
        assert_eq!(coord.get_state(&logical_id), Some(DeliveryState::InFlight));
    }

    #[test]
    fn test_delivery_retry_exhaustion_and_expiry() {
        let id_local = crate::identity::Identity::generate();
        let id_remote = crate::identity::Identity::generate();

        let mut coord = DeliveryCoordinator::new(id_local.peer_id());
        let logical_id = generate_logical_id();
        let now = Instant::now();

        coord.enqueue(
            EnqueueMessage {
                logical_id,
                recipient_peer_id: id_remote.peer_id(),
                recipient_short_id: id_remote.short_id(),
                body: b"data".to_vec(),
                ttl: Duration::from_secs(10), // 10s TTL
                max_attempts: 2,              // Max 2 attempts
                is_privacy_mode: false,
            },
            now,
        );

        // Attempt 1
        coord
            .mark_in_flight(&logical_id, Duration::from_secs(2), now)
            .unwrap();
        // Step forward 3s -> retry ready
        let actions = coord.check_timeouts(now + Duration::from_secs(3));
        assert_eq!(actions, vec![DeliveryAction::ReadyToRetry(logical_id)]);

        // Attempt 2 (reaches max_attempts)
        coord
            .mark_in_flight(
                &logical_id,
                Duration::from_secs(2),
                now + Duration::from_secs(3),
            )
            .unwrap();
        let actions_fail = coord.check_timeouts(now + Duration::from_secs(6));
        assert_eq!(actions_fail, vec![DeliveryAction::Failed(logical_id)]);
        assert_eq!(coord.get_state(&logical_id), Some(DeliveryState::Failed));

        // Test TTL expiry
        let id_exp = generate_logical_id();
        coord.enqueue(
            EnqueueMessage {
                logical_id: id_exp,
                recipient_peer_id: id_remote.peer_id(),
                recipient_short_id: id_remote.short_id(),
                body: b"expiring".to_vec(),
                ttl: Duration::from_secs(5),
                max_attempts: 10,
                is_privacy_mode: false,
            },
            now,
        );
        let actions_exp = coord.check_timeouts(now + Duration::from_secs(6));
        assert!(actions_exp.contains(&DeliveryAction::Expired(id_exp)));
        assert_eq!(coord.get_state(&id_exp), Some(DeliveryState::Expired));
    }

    #[test]
    fn test_no_route_and_handshake_separation_plus_coalesced_wakeup() {
        let id_local = crate::identity::Identity::generate();
        let id_remote = crate::identity::Identity::generate();
        let s_remote = id_remote.short_id();

        let mut coord = DeliveryCoordinator::new(id_local.peer_id());
        let logical_id = generate_logical_id();
        let now = Instant::now();

        coord.enqueue(
            EnqueueMessage {
                logical_id,
                recipient_peer_id: id_remote.peer_id(),
                recipient_short_id: s_remote,
                body: b"queued payload".to_vec(),
                ttl: Duration::from_secs(60),
                max_attempts: 3,
                is_privacy_mode: false,
            },
            now,
        );

        // 1. NoRouteAvailable (`has_route = false`) must keep message Queued and burn 0 attempts
        for step in 1..=10 {
            coord
                .defer_until_session_or_route(
                    &logical_id,
                    false,
                    Duration::from_secs(1),
                    now + Duration::from_secs(step),
                )
                .unwrap();
        }
        let rec = coord.get_record(&logical_id).unwrap();
        assert_eq!(rec.state, DeliveryState::Queued);
        assert_eq!(rec.attempts, 0);
        assert_eq!(rec.handshake_attempts, 0);

        // 2. Handshake attempts (`has_route = true`) consume handshake_attempts, not message attempts
        coord
            .defer_until_session_or_route(
                &logical_id,
                true,
                Duration::from_secs(10),
                now + Duration::from_secs(11),
            )
            .unwrap();
        let rec = coord.get_record(&logical_id).unwrap();
        assert_eq!(rec.state, DeliveryState::Queued);
        assert_eq!(rec.attempts, 0);
        assert_eq!(rec.handshake_attempts, 1);

        // 3. Unverified ShortId link announcements cannot wake up backoff
        assert!(!coord.reset_backoff_for_verified_peer(
            &s_remote,
            false,
            now + Duration::from_secs(12),
        ));
        assert_eq!(
            coord.get_record(&logical_id).unwrap().next_attempt_at,
            now + Duration::from_secs(21)
        );

        // 4. Verified peer wakeup resets next_attempt_at once, coalesces within cooldown, and preserves TTL
        let orig_expiry = coord.get_record(&logical_id).unwrap().expires_at;
        assert!(coord.reset_backoff_for_verified_peer(
            &s_remote,
            true,
            now + Duration::from_secs(12),
        ));
        assert_eq!(
            coord.get_record(&logical_id).unwrap().next_attempt_at,
            now + Duration::from_secs(12)
        );
        assert_eq!(
            coord.get_record(&logical_id).unwrap().expires_at,
            orig_expiry
        );

        // Rapid second wakeup 1s later is coalesced (rejected)
        assert!(!coord.reset_backoff_for_verified_peer(
            &s_remote,
            true,
            now + Duration::from_secs(13),
        ));
    }
}
