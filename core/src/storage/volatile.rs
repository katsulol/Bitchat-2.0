// CRYPTO-REVIEW:
// What it does: Implements authenticated E2E volatile conversation capability negotiation (`BC2V`),
// strict no-downgrade enforcement, bounded memory-only message retention with zeroize-on-drop,
// and pending-send cancellation on conversation close/drop.
// Primitives / crates: `zeroize`, `rand` (OsRng).
// Reviewer checklist:
// 1. Negotiation frames (`Offer`, `Accept`, `RejectUnsupported`, `CloseSession`) are exchanged
//    exclusively inside authenticated Noise XX `ApplicationFrame` payloads.
// 2. Replayed `session_id` offers/accepts are rejected via bounded replay tracking.
// 3. `VolatileRequired` conversations never silently downgrade to durable SQLite (`MessageMode::Normal`).
// 4. Plaintext message bodies in `VolatileMessageEntry` are zeroized on expiry, close, or drop.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use rand::rngs::OsRng;
use rand::RngCore;
use zeroize::Zeroize;

use crate::error::{Error, Result};
use crate::identity::PeerId;
use crate::mesh::delivery::{DeliveryCoordinator, LogicalMessageId};
use crate::storage::{MessageMode, OutboxManager};

pub const VOLATILE_MAGIC: &[u8; 4] = b"BC2V";
pub const VOLATILE_WIRE_VERSION: u8 = 1;
pub const VOLATILE_FRAME_LEN: usize = 26; // 4 (magic) + 1 (ver) + 1 (kind) + 16 (session_id) + 4 (retention_secs)

/// Default and maximum pending/retention lifetime (15 minutes = 900 seconds, matching Phase 4 Privacy TTL).
pub const MAX_VOLATILE_RETENTION_SECS: u32 = 900;
pub const MIN_VOLATILE_RETENTION_SECS: u32 = 10;
pub const DEFAULT_MAX_MESSAGES_PER_CONVERSATION: usize = 64;
pub const DEFAULT_MAX_BYTES_PER_CONVERSATION: usize = 64 * 1024;
pub const MAX_TRACKED_SESSION_IDS: usize = 256;

/// Kind of authenticated volatile capability negotiation frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VolatileFrameKind {
    Offer = 1,
    Accept = 2,
    RejectUnsupported = 3,
    CloseSession = 4,
}

impl VolatileFrameKind {
    pub fn from_u8(val: u8) -> Result<Self> {
        match val {
            1 => Ok(Self::Offer),
            2 => Ok(Self::Accept),
            3 => Ok(Self::RejectUnsupported),
            4 => Ok(Self::CloseSession),
            other => Err(Error::Crypto(format!(
                "Invalid VolatileFrameKind: {other}"
            ))),
        }
    }
}

/// Wire frame carried inside an authenticated `ApplicationFrame` to negotiate volatile retention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolatileNegotiationFrame {
    pub version: u8,
    pub kind: VolatileFrameKind,
    pub session_id: [u8; 16],
    pub retention_secs: u32,
}

impl VolatileNegotiationFrame {
    pub fn new_offer(retention_secs: u32) -> Self {
        let mut session_id = [0u8; 16];
        OsRng.fill_bytes(&mut session_id);
        let bounded = retention_secs.clamp(MIN_VOLATILE_RETENTION_SECS, MAX_VOLATILE_RETENTION_SECS);
        Self {
            version: VOLATILE_WIRE_VERSION,
            kind: VolatileFrameKind::Offer,
            session_id,
            retention_secs: bounded,
        }
    }

    pub fn new_accept(session_id: [u8; 16], retention_secs: u32) -> Self {
        let bounded = retention_secs.clamp(MIN_VOLATILE_RETENTION_SECS, MAX_VOLATILE_RETENTION_SECS);
        Self {
            version: VOLATILE_WIRE_VERSION,
            kind: VolatileFrameKind::Accept,
            session_id,
            retention_secs: bounded,
        }
    }

    pub fn new_reject(session_id: [u8; 16]) -> Self {
        Self {
            version: VOLATILE_WIRE_VERSION,
            kind: VolatileFrameKind::RejectUnsupported,
            session_id,
            retention_secs: 0,
        }
    }

    pub fn new_close(session_id: [u8; 16]) -> Self {
        Self {
            version: VOLATILE_WIRE_VERSION,
            kind: VolatileFrameKind::CloseSession,
            session_id,
            retention_secs: 0,
        }
    }

    pub fn encode(&self) -> [u8; VOLATILE_FRAME_LEN] {
        let mut out = [0u8; VOLATILE_FRAME_LEN];
        out[0..4].copy_from_slice(VOLATILE_MAGIC);
        out[4] = self.version;
        out[5] = self.kind as u8;
        out[6..22].copy_from_slice(&self.session_id);
        out[22..26].copy_from_slice(&self.retention_secs.to_be_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != VOLATILE_FRAME_LEN {
            return Err(Error::Crypto(format!(
                "Invalid VolatileNegotiationFrame length: expected {}, got {}",
                VOLATILE_FRAME_LEN,
                bytes.len()
            )));
        }
        if &bytes[0..4] != VOLATILE_MAGIC {
            return Err(Error::Crypto("Invalid volatile frame magic".into()));
        }
        let version = bytes[4];
        if version != VOLATILE_WIRE_VERSION {
            return Err(Error::Crypto(format!(
                "Unsupported VolatileNegotiationFrame version: {version}"
            )));
        }
        let kind = VolatileFrameKind::from_u8(bytes[5])?;
        let mut session_id = [0u8; 16];
        session_id.copy_from_slice(&bytes[6..22]);
        let mut ret_bytes = [0u8; 4];
        ret_bytes.copy_from_slice(&bytes[22..26]);
        let retention_secs = u32::from_be_bytes(ret_bytes);

        if matches!(kind, VolatileFrameKind::Offer | VolatileFrameKind::Accept)
            && !(MIN_VOLATILE_RETENTION_SECS..=MAX_VOLATILE_RETENTION_SECS).contains(&retention_secs)
        {
            return Err(Error::Crypto(format!(
                "Volatile retention_secs {} out of allowed bounds [{}..={}]",
                retention_secs, MIN_VOLATILE_RETENTION_SECS, MAX_VOLATILE_RETENTION_SECS
            )));
        }

        Ok(Self {
            version,
            kind,
            session_id,
            retention_secs,
        })
    }
}

/// Negotiation state for a peer conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolatileSessionState {
    /// Standard durable mode (`MessageMode::Normal`).
    NotRequested,
    /// Local user requested volatile chat; waiting for authenticated peer `Accept`.
    PendingNegotiation {
        session_id: [u8; 16],
        requested_retention: Duration,
    },
    /// Both peers negotiated and accepted volatile retention; ready for `MessageMode::Privacy` sends.
    Active {
        session_id: [u8; 16],
        effective_retention: Duration,
    },
    /// Remote peer explicitly rejected volatile chat or does not support it.
    UnsupportedByPeer,
}

/// Outcome of checking whether a send is permitted under the conversation's retention policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolatileSendCheck {
    /// Proceed using `MessageMode::Privacy` (zero SQLite disk writes) with `ttl`.
    AllowVolatile { ttl: Duration },
    /// Proceed using `MessageMode::Normal` (only when volatile mode was NOT requested).
    AllowNormal,
    /// Blocked: volatile mode is required, waiting for capability negotiation to complete.
    BlockedPendingNegotiation,
    /// Rejected: remote peer does not support volatile chat; never silently downgrade to Normal!
    RejectedUnsupportedPeer,
}

/// In-memory volatile message entry that zeroizes its plaintext buffer on drop.
#[derive(Debug, Clone)]
pub struct VolatileMessageEntry {
    pub logical_id: LogicalMessageId,
    pub sender: PeerId,
    pub plaintext: Vec<u8>,
    pub expires_at: Instant,
}

impl Drop for VolatileMessageEntry {
    fn drop(&mut self) {
        self.plaintext.zeroize();
    }
}

/// Coordinator for volatile conversation negotiation, memory-only message retention,
/// and pending-send cancellation on session close/drop.
pub struct VolatileSessionManager {
    local_supports_volatile: bool,
    sessions: HashMap<PeerId, VolatileSessionState>,
    messages: HashMap<PeerId, VecDeque<VolatileMessageEntry>>,
    pending_sends: HashMap<PeerId, Vec<LogicalMessageId>>,
    seen_session_ids: VecDeque<(PeerId, [u8; 16])>,
}

impl Default for VolatileSessionManager {
    fn default() -> Self {
        Self::new(true)
    }
}

impl VolatileSessionManager {
    pub fn new(local_supports_volatile: bool) -> Self {
        Self {
            local_supports_volatile,
            sessions: HashMap::new(),
            messages: HashMap::new(),
            pending_sends: HashMap::new(),
            seen_session_ids: VecDeque::new(),
        }
    }

    pub fn session_state(&self, peer: &PeerId) -> VolatileSessionState {
        self.sessions
            .get(peer)
            .cloned()
            .unwrap_or(VolatileSessionState::NotRequested)
    }

    /// Initiate volatile mode for a conversation with `peer`, returning the `Offer` frame to send.
    pub fn initiate_volatile_session(
        &mut self,
        peer: PeerId,
        retention_secs: u32,
    ) -> VolatileNegotiationFrame {
        let offer = VolatileNegotiationFrame::new_offer(retention_secs);
        self.record_seen_session(peer, offer.session_id);
        self.sessions.insert(
            peer,
            VolatileSessionState::PendingNegotiation {
                session_id: offer.session_id,
                requested_retention: Duration::from_secs(offer.retention_secs as u64),
            },
        );
        offer
    }

    /// Process an incoming authenticated `VolatileNegotiationFrame` from `peer`.
    ///
    /// Returns an optional response frame (`Accept` or `RejectUnsupported`) to transmit back.
    pub fn handle_negotiation_frame(
        &mut self,
        peer: PeerId,
        frame: &VolatileNegotiationFrame,
        outbox: &mut OutboxManager,
        delivery_coord: &mut DeliveryCoordinator,
    ) -> Result<Option<VolatileNegotiationFrame>> {
        match frame.kind {
            VolatileFrameKind::Offer => {
                // Reject replayed session_id offers
                if self.seen_session_ids.contains(&(peer, frame.session_id)) {
                    return Err(Error::Crypto(
                        "Replayed volatile session Offer rejected".into(),
                    ));
                }
                self.record_seen_session(peer, frame.session_id);

                if !self.local_supports_volatile {
                    self.sessions
                        .insert(peer, VolatileSessionState::UnsupportedByPeer);
                    return Ok(Some(VolatileNegotiationFrame::new_reject(frame.session_id)));
                }

                let bounded_secs = frame
                    .retention_secs
                    .clamp(MIN_VOLATILE_RETENTION_SECS, MAX_VOLATILE_RETENTION_SECS);
                self.sessions.insert(
                    peer,
                    VolatileSessionState::Active {
                        session_id: frame.session_id,
                        effective_retention: Duration::from_secs(bounded_secs as u64),
                    },
                );
                Ok(Some(VolatileNegotiationFrame::new_accept(
                    frame.session_id,
                    bounded_secs,
                )))
            }
            VolatileFrameKind::Accept => {
                match self.sessions.get(&peer) {
                    Some(VolatileSessionState::PendingNegotiation {
                        session_id,
                        requested_retention,
                    }) if *session_id == frame.session_id => {
                        let peer_ret = Duration::from_secs(frame.retention_secs as u64);
                        let effective = (*requested_retention).min(peer_ret);
                        self.sessions.insert(
                            peer,
                            VolatileSessionState::Active {
                                session_id: frame.session_id,
                                effective_retention: effective,
                            },
                        );
                        Ok(None)
                    }
                    _ => Err(Error::Crypto(
                        "Unexpected or replayed volatile Accept frame".into(),
                    )),
                }
            }
            VolatileFrameKind::RejectUnsupported => {
                if let Some(VolatileSessionState::PendingNegotiation { session_id, .. }) =
                    self.sessions.get(&peer)
                {
                    if *session_id == frame.session_id {
                        self.sessions
                            .insert(peer, VolatileSessionState::UnsupportedByPeer);
                        self.cancel_pending_sends_for_peer(&peer, outbox, delivery_coord);
                        return Ok(None);
                    }
                }
                Err(Error::Crypto("Unexpected volatile Reject frame".into()))
            }
            VolatileFrameKind::CloseSession => {
                self.close_conversation(&peer, outbox, delivery_coord);
                Ok(None)
            }
        }
    }

    /// Check whether sending a message to `peer` is permitted and enforce no-downgrade semantics.
    ///
    /// If `require_volatile` is true or the conversation is in a volatile state, this method
    /// NEVER permits falling back to `MessageMode::Normal`.
    pub fn check_send_policy(&self, peer: &PeerId, requested_mode: MessageMode) -> VolatileSendCheck {
        let state = self.session_state(peer);
        match (requested_mode, state) {
            (_, VolatileSessionState::Active { effective_retention, .. }) => {
                // Once volatile session is active, all sends in this session use volatile Privacy storage
                VolatileSendCheck::AllowVolatile {
                    ttl: effective_retention,
                }
            }
            (_, VolatileSessionState::PendingNegotiation { .. }) => {
                VolatileSendCheck::BlockedPendingNegotiation
            }
            (_, VolatileSessionState::UnsupportedByPeer) => {
                VolatileSendCheck::RejectedUnsupportedPeer
            }
            (MessageMode::Privacy, VolatileSessionState::NotRequested) => {
                VolatileSendCheck::BlockedPendingNegotiation
            }
            (MessageMode::Normal, VolatileSessionState::NotRequested) => {
                VolatileSendCheck::AllowNormal
            }
        }
    }

    /// Track a queued volatile message ID so it can be cancelled if the conversation is closed before delivery.
    pub fn register_pending_volatile_send(&mut self, peer: PeerId, logical_id: LogicalMessageId) {
        self.pending_sends.entry(peer).or_default().push(logical_id);
    }

    /// Store a received/displayed message in the bounded volatile conversation buffer.
    pub fn record_volatile_message(
        &mut self,
        peer: PeerId,
        logical_id: LogicalMessageId,
        sender: PeerId,
        plaintext: Vec<u8>,
        now: Instant,
    ) -> Result<()> {
        let ttl = match self.session_state(&peer) {
            VolatileSessionState::Active {
                effective_retention,
                ..
            } => effective_retention,
            _ => {
                return Err(Error::Storage(
                    "Cannot store volatile message without active volatile session".into(),
                ))
            }
        };

        if plaintext.len() > DEFAULT_MAX_BYTES_PER_CONVERSATION {
            return Err(Error::Storage("Volatile message exceeds conversation byte cap".into()));
        }

        let queue = self.messages.entry(peer).or_default();
        queue.retain(|m| m.expires_at > now);

        while queue.len() >= DEFAULT_MAX_MESSAGES_PER_CONVERSATION
            || queue.iter().map(|m| m.plaintext.len()).sum::<usize>() + plaintext.len()
                > DEFAULT_MAX_BYTES_PER_CONVERSATION
        {
            queue.pop_front(); // Zeroizes evicted entry via Drop
        }

        queue.push_back(VolatileMessageEntry {
            logical_id,
            sender,
            plaintext,
            expires_at: now + ttl,
        });
        Ok(())
    }

    /// Return non-expired volatile messages for `peer`, pruning any expired entries first.
    pub fn active_messages(&mut self, peer: &PeerId, now: Instant) -> Vec<&VolatileMessageEntry> {
        self.prune_expired(now);
        self.messages
            .get(peer)
            .map(|q| q.iter().collect())
            .unwrap_or_default()
    }

    /// Prune and zeroize all expired volatile messages across all conversations.
    pub fn prune_expired(&mut self, now: Instant) -> usize {
        let mut pruned = 0;
        for queue in self.messages.values_mut() {
            let before = queue.len();
            queue.retain(|m| m.expires_at > now);
            pruned += before.saturating_sub(queue.len());
        }
        pruned
    }

    /// Close a volatile conversation, zeroizing all in-memory messages and cancelling any
    /// still-pending (`Queued` / `InFlight`) outgoing sends in `OutboxManager` and `DeliveryCoordinator`.
    pub fn close_conversation(
        &mut self,
        peer: &PeerId,
        outbox: &mut OutboxManager,
        delivery_coord: &mut DeliveryCoordinator,
    ) -> usize {
        self.sessions.remove(peer);
        if let Some(mut msgs) = self.messages.remove(peer) {
            msgs.clear(); // Triggers Drop -> Zeroize on every VolatileMessageEntry
        }
        self.cancel_pending_sends_for_peer(peer, outbox, delivery_coord)
    }

    fn cancel_pending_sends_for_peer(
        &mut self,
        peer: &PeerId,
        outbox: &mut OutboxManager,
        delivery_coord: &mut DeliveryCoordinator,
    ) -> usize {
        let mut cancelled = 0;
        if let Some(ids) = self.pending_sends.remove(peer) {
            for id in ids {
                if delivery_coord.cancel(&id) {
                    cancelled += 1;
                }
                let _ = outbox.mark_cancelled(&id);
            }
        }
        cancelled
    }

    fn record_seen_session(&mut self, peer: PeerId, session_id: [u8; 16]) {
        if self.seen_session_ids.len() >= MAX_TRACKED_SESSION_IDS {
            self.seen_session_ids.pop_front();
        }
        self.seen_session_ids.push_back((peer, session_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use crate::mesh::delivery::EnqueueMessage;
    use crate::storage::{
        InjectedKeyProvider, MemoryOutboxStore, OutboxLimits, SendResult, SqliteOutboxStore,
    };
    use std::sync::Arc;

    fn make_outbox_and_coord(id: &Identity) -> (OutboxManager, DeliveryCoordinator) {
        let limits = OutboxLimits::default();
        let kp = Arc::new(InjectedKeyProvider::random(1));
        let sqlite = SqliteOutboxStore::in_memory(kp, limits).unwrap();
        let memory = MemoryOutboxStore::new(limits);
        (
            OutboxManager::new(id.peer_id(), sqlite, memory, limits),
            DeliveryCoordinator::new(id.peer_id()),
        )
    }

    #[test]
    fn test_volatile_negotiation_no_downgrade_and_replay_rejection() {
        let alice = Identity::generate();
        let bob = Identity::generate();
        let (mut outbox_a, mut coord_a) = make_outbox_and_coord(&alice);
        let (mut outbox_b, mut coord_b) = make_outbox_and_coord(&bob);

        let mut mgr_a = VolatileSessionManager::new(true);
        let mut mgr_b_unsupported = VolatileSessionManager::new(false);

        // 1. Before negotiation, sending in Privacy/Volatile mode is blocked pending negotiation
        assert_eq!(
            mgr_a.check_send_policy(&bob.peer_id(), MessageMode::Privacy),
            VolatileSendCheck::BlockedPendingNegotiation
        );

        // 2. Alice initiates offer; Bob does not support volatile chat -> RejectUnsupported
        let offer = mgr_a.initiate_volatile_session(bob.peer_id(), 120);
        let encoded_offer = offer.encode();
        let decoded_offer = VolatileNegotiationFrame::decode(&encoded_offer).unwrap();

        let resp = mgr_b_unsupported
            .handle_negotiation_frame(alice.peer_id(), &decoded_offer, &mut outbox_b, &mut coord_b)
            .unwrap()
            .expect("Must return RejectUnsupported");
        assert_eq!(resp.kind, VolatileFrameKind::RejectUnsupported);

        // Replaying the same Offer to Bob is rejected!
        assert!(mgr_b_unsupported
            .handle_negotiation_frame(alice.peer_id(), &decoded_offer, &mut outbox_b, &mut coord_b)
            .is_err());

        // Alice processes RejectUnsupported -> strictly blocks both Privacy AND Normal sends (no silent downgrade!)
        mgr_a
            .handle_negotiation_frame(bob.peer_id(), &resp, &mut outbox_a, &mut coord_a)
            .unwrap();
        assert_eq!(
            mgr_a.check_send_policy(&bob.peer_id(), MessageMode::Privacy),
            VolatileSendCheck::RejectedUnsupportedPeer
        );
        assert_eq!(
            mgr_a.check_send_policy(&bob.peer_id(), MessageMode::Normal),
            VolatileSendCheck::RejectedUnsupportedPeer
        );
    }

    #[test]
    fn test_volatile_active_session_zero_sqlite_expiry_and_close_cancellation() {
        let alice = Identity::generate();
        let bob = Identity::generate();
        let (mut outbox_a, mut coord_a) = make_outbox_and_coord(&alice);
        let (mut outbox_b, mut coord_b) = make_outbox_and_coord(&bob);

        let mut mgr_a = VolatileSessionManager::new(true);
        let mut mgr_b = VolatileSessionManager::new(true);

        let offer = mgr_a.initiate_volatile_session(bob.peer_id(), 60);
        let accept = mgr_b
            .handle_negotiation_frame(alice.peer_id(), &offer, &mut outbox_b, &mut coord_b)
            .unwrap()
            .expect("Bob accepts volatile session");
        mgr_a
            .handle_negotiation_frame(bob.peer_id(), &accept, &mut outbox_a, &mut coord_a)
            .unwrap();

        // Even if caller passes MessageMode::Normal during an Active volatile session,
        // check_send_policy enforces AllowVolatile with the negotiated 60s TTL!
        let ttl = match mgr_a.check_send_policy(&bob.peer_id(), MessageMode::Normal) {
            VolatileSendCheck::AllowVolatile { ttl } => ttl,
            other => panic!("Expected AllowVolatile, got {other:?}"),
        };
        assert_eq!(ttl, Duration::from_secs(60));

        // Enqueue volatile send in OutboxManager (Privacy mode) and DeliveryCoordinator
        let now = Instant::now();
        let send_res = outbox_a.send_to(
            bob.peer_id(),
            b"Ephemeral secret".to_vec(),
            MessageMode::Privacy,
            Some(ttl),
            1000,
        );
        let logical_id = match send_res {
            SendResult::Accepted { logical_id, .. } => logical_id,
            other => panic!("Unexpected: {other:?}"),
        };
        coord_a.enqueue(
            EnqueueMessage {
                logical_id,
                recipient_peer_id: bob.peer_id(),
                recipient_short_id: bob.short_id(),
                body: b"Ephemeral secret".to_vec(),
                ttl,
                max_attempts: 3,
                is_privacy_mode: true,
            },
            now,
        );
        mgr_a.register_pending_volatile_send(bob.peer_id(), logical_id);

        // Record received volatile message on Bob
        mgr_b
            .record_volatile_message(
                alice.peer_id(),
                logical_id,
                alice.peer_id(),
                b"Ephemeral secret".to_vec(),
                now,
            )
            .unwrap();
        assert_eq!(mgr_b.active_messages(&alice.peer_id(), now).len(), 1);

        // After 61 seconds, Bob's volatile message automatically expires and is purged
        assert_eq!(
            mgr_b
                .active_messages(&alice.peer_id(), now + Duration::from_secs(61))
                .len(),
            0
        );

        // Closing conversation on Alice cancels the still-queued volatile send!
        let cancelled = mgr_a.close_conversation(&bob.peer_id(), &mut outbox_a, &mut coord_a);
        assert_eq!(cancelled, 1);
        assert_eq!(
            coord_a.get_state(&logical_id),
            Some(crate::mesh::delivery::DeliveryState::Cancelled)
        );
    }
}
