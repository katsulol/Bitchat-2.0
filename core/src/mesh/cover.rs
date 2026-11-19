// CRYPTO-REVIEW:
// What it does: Experimental bounded direct-link cover traffic (dummy packet padding) carried
// exclusively inside authenticated Noise XX `ApplicationFrame` payloads with fresh nonces.
// Primitives / crates: `rand` (`OsRng`).
// Reviewer checklist:
// 1. Default OFF (`CoverTrafficConfig::default().enabled == false`).
// 2. Requires mutual capability negotiation (`Offer` / `Accept`) before emitting dummies;
//    unsupported or unapproved peers receive zero dummy traffic and no response amplification.
// 3. Dummy frames use fixed bucket sizes (`COVER_DUMMY_BUCKETS`) filled with CSPRNG bytes in production.
// 4. Dummies are silently discarded inside `poll_inbound` before ACK generation, deduplication,
//    outbox persistence, volatile storage, or UI notification.
// 5. Strict rate/byte/burst/lifetime budgets and resource-pause/thermal/real-traffic overrides.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use rand::rngs::OsRng;
use rand::RngCore;

use crate::error::{Error, Result};
use crate::identity::PeerId;
use crate::mesh::resource::{RelayResourceProfile, ThermalState};

pub const COVER_MAGIC: &[u8; 4] = b"BC2C";
pub const COVER_VERSION: u8 = 1;
pub const COVER_HEADER_LEN: usize = 8; // 4 (magic) + 1 (ver) + 1 (kind) + 2 (bucket_len)
pub const COVER_DUMMY_BUCKETS: [usize; 3] = [32, 96, 224];
pub const DEFAULT_COVER_MAX_BYTES_PER_MINUTE: usize = 2048;
pub const DEFAULT_COVER_MAX_BURST_FRAMES: usize = 2;
pub const DEFAULT_COVER_MIN_INTERVAL_MS: u64 = 1500;
pub const DEFAULT_COVER_MAX_INTERVAL_MS: u64 = 6000;
pub const DEFAULT_COVER_SESSION_LIFETIME_SECS: u64 = 1800;

/// Kind of authenticated cover-traffic frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CoverFrameKind {
    Offer = 1,
    Accept = 2,
    Reject = 3,
    Disable = 4,
    Dummy = 5,
}

impl CoverFrameKind {
    pub fn from_u8(val: u8) -> Result<Self> {
        match val {
            1 => Ok(Self::Offer),
            2 => Ok(Self::Accept),
            3 => Ok(Self::Reject),
            4 => Ok(Self::Disable),
            5 => Ok(Self::Dummy),
            other => Err(Error::Crypto(format!("Invalid CoverFrameKind: {other}"))),
        }
    }
}

/// Wire frame carried inside an encrypted Noise `ApplicationFrame`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverFrame {
    pub version: u8,
    pub kind: CoverFrameKind,
    pub bucket_len: u16,
    pub filler: Vec<u8>,
}

impl CoverFrame {
    pub fn control(kind: CoverFrameKind) -> Self {
        Self {
            version: COVER_VERSION,
            kind,
            bucket_len: 0,
            filler: Vec::new(),
        }
    }

    pub fn dummy(bucket_len: usize, rng: &mut dyn CoverRng) -> Result<Self> {
        if !COVER_DUMMY_BUCKETS.contains(&bucket_len) {
            return Err(Error::Envelope(format!(
                "Invalid cover bucket length {bucket_len}"
            )));
        }
        let mut filler = vec![0u8; bucket_len];
        rng.fill_bytes(&mut filler);
        Ok(Self {
            version: COVER_VERSION,
            kind: CoverFrameKind::Dummy,
            bucket_len: bucket_len as u16,
            filler,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(COVER_HEADER_LEN + self.filler.len());
        out.extend_from_slice(COVER_MAGIC);
        out.push(self.version);
        out.push(self.kind as u8);
        out.extend_from_slice(&self.bucket_len.to_be_bytes());
        out.extend_from_slice(&self.filler);
        out
    }

    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < COVER_HEADER_LEN || buf.len() > 512 {
            return Err(Error::Crypto(format!(
                "Invalid CoverFrame length: {}",
                buf.len()
            )));
        }
        if &buf[0..4] != COVER_MAGIC {
            return Err(Error::Crypto("Invalid CoverFrame magic".into()));
        }
        let version = buf[4];
        if version != COVER_VERSION {
            return Err(Error::Crypto(format!(
                "Unsupported CoverFrame version: {version}"
            )));
        }
        let kind = CoverFrameKind::from_u8(buf[5])?;
        let bucket_len = u16::from_be_bytes([buf[6], buf[7]]);
        let trailing = &buf[COVER_HEADER_LEN..];
        if trailing.len() != bucket_len as usize {
            return Err(Error::Crypto(format!(
                "CoverFrame length mismatch: header {bucket_len}, actual {}",
                trailing.len()
            )));
        }
        match kind {
            CoverFrameKind::Dummy => {
                if !COVER_DUMMY_BUCKETS.contains(&trailing.len()) {
                    return Err(Error::Crypto(format!(
                        "Unsupported CoverFrame dummy bucket: {}",
                        trailing.len()
                    )));
                }
            }
            _ => {
                if bucket_len != 0 {
                    return Err(Error::Crypto(
                        "Control CoverFrame must have zero payload".into(),
                    ));
                }
            }
        }
        Ok(Self {
            version,
            kind,
            bucket_len,
            filler: trailing.to_vec(),
        })
    }
}

/// Injectable randomness source for cover scheduling and dummy byte padding.
pub trait CoverRng: Send + Sync {
    fn next_u64(&mut self) -> u64;
    fn fill_bytes(&mut self, dst: &mut [u8]);
}

/// Production CSPRNG backed by `OsRng`.
pub struct OsCoverRng;

impl CoverRng for OsCoverRng {
    fn next_u64(&mut self) -> u64 {
        OsRng.next_u64()
    }

    fn fill_bytes(&mut self, dst: &mut [u8]) {
        OsRng.fill_bytes(dst);
    }
}

/// Deterministic PRNG strictly for reproducible simulation and unit tests (not cryptographic).
pub struct DeterministicCoverRng {
    state: u64,
}

impl DeterministicCoverRng {
    pub fn new(seed: u64) -> Self {
        Self {
            state: seed.wrapping_add(1),
        }
    }
}

impl CoverRng for DeterministicCoverRng {
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn fill_bytes(&mut self, dst: &mut [u8]) {
        let mut i = 0usize;
        while i < dst.len() {
            let chunk = self.next_u64().to_le_bytes();
            let take = (dst.len() - i).min(8);
            dst[i..i + take].copy_from_slice(&chunk[..take]);
            i += take;
        }
    }
}

/// Negotiation state with a specific verified direct peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverPeerState {
    NotNegotiated,
    Offered,
    Negotiated { expires_at: Instant },
    Unsupported,
}

/// Configuration for optional experimental cover traffic.
#[derive(Debug, Clone)]
pub struct CoverTrafficConfig {
    pub enabled: bool,
    pub max_bytes_per_minute: usize,
    pub max_burst_frames: usize,
    pub min_interval: Duration,
    pub max_interval: Duration,
    pub max_session_lifetime: Duration,
}

impl Default for CoverTrafficConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_bytes_per_minute: DEFAULT_COVER_MAX_BYTES_PER_MINUTE,
            max_burst_frames: DEFAULT_COVER_MAX_BURST_FRAMES,
            min_interval: Duration::from_millis(DEFAULT_COVER_MIN_INTERVAL_MS),
            max_interval: Duration::from_millis(DEFAULT_COVER_MAX_INTERVAL_MS),
            max_session_lifetime: Duration::from_secs(DEFAULT_COVER_SESSION_LIFETIME_SECS),
        }
    }
}

/// Bounded scheduler and negotiation state machine for experimental direct-link cover traffic.
pub struct CoverTrafficManager {
    config: CoverTrafficConfig,
    rng: Box<dyn CoverRng>,
    peers: HashMap<PeerId, CoverPeerState>,
    recent_bytes: VecDeque<(Instant, usize)>,
    next_emit_at: Option<Instant>,
    peer_cursor: usize,
    pub dummy_frames_sent: u64,
    pub dummy_bytes_sent: u64,
    pub dummy_frames_discarded: u64,
}

impl Default for CoverTrafficManager {
    fn default() -> Self {
        Self::new(CoverTrafficConfig::default(), Box::new(OsCoverRng))
    }
}

impl CoverTrafficManager {
    pub fn new(config: CoverTrafficConfig, rng: Box<dyn CoverRng>) -> Self {
        Self {
            config,
            rng,
            peers: HashMap::new(),
            recent_bytes: VecDeque::new(),
            next_emit_at: None,
            peer_cursor: 0,
            dummy_frames_sent: 0,
            dummy_bytes_sent: 0,
            dummy_frames_discarded: 0,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn max_bytes_per_minute(&self) -> usize {
        self.config.max_bytes_per_minute
    }

    pub fn negotiated_peer_count(&self, now: Instant) -> usize {
        self.peers
            .values()
            .filter(
                |st| matches!(st, CoverPeerState::Negotiated { expires_at } if now < *expires_at),
            )
            .count()
    }

    pub fn peer_state(&self, peer: &PeerId) -> CoverPeerState {
        self.peers
            .get(peer)
            .copied()
            .unwrap_or(CoverPeerState::NotNegotiated)
    }

    /// Update interval and rate-budget parameters within safe operational ceilings.
    pub fn configure_policy(
        &mut self,
        min_interval_ms: u64,
        max_interval_ms: u64,
        max_bytes_per_minute: usize,
    ) {
        let min_clamped = min_interval_ms.clamp(20, 60_000);
        let max_clamped = max_interval_ms.clamp(min_clamped, 120_000);
        self.config.min_interval = Duration::from_millis(min_clamped);
        self.config.max_interval = Duration::from_millis(max_clamped);
        self.config.max_bytes_per_minute = max_bytes_per_minute.clamp(64, 16_384);
        if self.config.enabled {
            let now = Instant::now();
            self.next_emit_at = Some(now + self.config.min_interval);
        }
    }

    /// Toggle cover traffic ON or OFF. Turning OFF emits `Disable` notices for negotiated peers.
    pub fn set_enabled(&mut self, enabled: bool, now: Instant) -> Vec<(PeerId, CoverFrame)> {
        if self.config.enabled == enabled {
            return Vec::new();
        }
        self.config.enabled = enabled;
        if enabled {
            self.next_emit_at = Some(now + self.config.min_interval);
            Vec::new()
        } else {
            self.next_emit_at = None;
            let mut notices = Vec::new();
            for (peer, st) in self.peers.iter_mut() {
                if matches!(
                    st,
                    CoverPeerState::Negotiated { .. } | CoverPeerState::Offered
                ) {
                    *st = CoverPeerState::NotNegotiated;
                    notices.push((*peer, CoverFrame::control(CoverFrameKind::Disable)));
                }
            }
            notices
        }
    }

    /// Initiate capability negotiation with an eligible approved direct peer.
    pub fn offer_peer(&mut self, peer: PeerId) -> Option<CoverFrame> {
        if !self.config.enabled {
            return None;
        }
        match self.peer_state(&peer) {
            CoverPeerState::NotNegotiated => {
                self.peers.insert(peer, CoverPeerState::Offered);
                Some(CoverFrame::control(CoverFrameKind::Offer))
            }
            _ => None,
        }
    }

    /// Remove per-peer cover state cleanly (e.g., on peer block/revocation or disconnect).
    pub fn remove_peer(&mut self, peer: &PeerId) {
        self.peers.remove(peer);
    }

    /// Process an inbound authenticated control `CoverFrame` (`Offer`/`Accept`/`Reject`/`Disable`).
    pub fn handle_control(
        &mut self,
        peer: PeerId,
        frame: &CoverFrame,
        now: Instant,
    ) -> Option<CoverFrame> {
        match frame.kind {
            CoverFrameKind::Offer => {
                if self.config.enabled {
                    self.peers.insert(
                        peer,
                        CoverPeerState::Negotiated {
                            expires_at: now + self.config.max_session_lifetime,
                        },
                    );
                    if self.next_emit_at.is_none() {
                        self.next_emit_at = Some(now + self.config.min_interval);
                    }
                    Some(CoverFrame::control(CoverFrameKind::Accept))
                } else {
                    self.peers.insert(peer, CoverPeerState::Unsupported);
                    Some(CoverFrame::control(CoverFrameKind::Reject))
                }
            }
            CoverFrameKind::Accept => {
                if self.config.enabled
                    && matches!(self.peers.get(&peer), Some(CoverPeerState::Offered))
                {
                    self.peers.insert(
                        peer,
                        CoverPeerState::Negotiated {
                            expires_at: now + self.config.max_session_lifetime,
                        },
                    );
                    if self.next_emit_at.is_none() {
                        self.next_emit_at = Some(now + self.config.min_interval);
                    }
                }
                None
            }
            CoverFrameKind::Reject => {
                self.peers.insert(peer, CoverPeerState::Unsupported);
                None
            }
            CoverFrameKind::Disable => {
                self.peers.insert(peer, CoverPeerState::NotNegotiated);
                None
            }
            CoverFrameKind::Dummy => None,
        }
    }

    pub fn record_discarded(&mut self) {
        self.dummy_frames_discarded = self.dummy_frames_discarded.saturating_add(1);
    }

    fn window_bytes_at(&mut self, now: Instant) -> usize {
        let cutoff = now.checked_sub(Duration::from_secs(60));
        if let Some(c) = cutoff {
            while let Some((ts, _)) = self.recent_bytes.front() {
                if *ts <= c {
                    self.recent_bytes.pop_front();
                } else {
                    break;
                }
            }
        }
        self.recent_bytes.iter().map(|(_, b)| *b).sum()
    }

    fn next_interval(&mut self) -> Duration {
        let min_ms = self.config.min_interval.as_millis() as u64;
        let max_ms = (self.config.max_interval.as_millis() as u64).max(min_ms);
        let span = max_ms - min_ms + 1;
        let offset = self.rng.next_u64() % span;
        Duration::from_millis(min_ms + offset)
    }

    /// Emit bounded dummy frames when all resource, mode, priority, and negotiation checks allow.
    pub fn poll_due_dummies(
        &mut self,
        now: Instant,
        eligible_peers: &[PeerId],
        profile: RelayResourceProfile,
        thermal: ThermalState,
        has_real_pending: bool,
    ) -> Vec<(PeerId, CoverFrame)> {
        if !self.config.enabled || has_real_pending || eligible_peers.is_empty() {
            return Vec::new();
        }
        if matches!(
            profile,
            RelayResourceProfile::Paused | RelayResourceProfile::Minimal
        ) {
            return Vec::new();
        }
        if matches!(thermal, ThermalState::Serious | ThermalState::Critical) {
            return Vec::new();
        }

        // Expire any negotiated session past lifetime
        let mut active_peers = Vec::new();
        for peer in eligible_peers {
            match self.peers.get(peer).copied() {
                Some(CoverPeerState::Negotiated { expires_at }) if now < expires_at => {
                    active_peers.push(*peer);
                }
                Some(CoverPeerState::Negotiated { .. }) => {
                    self.peers.insert(*peer, CoverPeerState::NotNegotiated);
                }
                _ => {}
            }
        }
        if active_peers.is_empty() {
            return Vec::new();
        }

        match self.next_emit_at {
            None => {
                let interval = self.next_interval();
                self.next_emit_at = Some(now + interval);
                return Vec::new();
            }
            Some(due) if now < due => return Vec::new(),
            Some(_) => {}
        }

        let mut emitted = Vec::new();
        let used = self.window_bytes_at(now);
        let mut remaining_budget = self.config.max_bytes_per_minute.saturating_sub(used);

        let burst = self
            .config
            .max_burst_frames
            .clamp(1, DEFAULT_COVER_MAX_BURST_FRAMES);
        for _ in 0..burst {
            let bucket_idx = (self.rng.next_u64() as usize) % COVER_DUMMY_BUCKETS.len();
            let bucket = COVER_DUMMY_BUCKETS[bucket_idx];
            let frame_wire_cost = COVER_HEADER_LEN + bucket;
            let chosen_bucket = if frame_wire_cost <= remaining_budget {
                bucket
            } else if COVER_HEADER_LEN + COVER_DUMMY_BUCKETS[0] <= remaining_budget {
                COVER_DUMMY_BUCKETS[0]
            } else {
                break;
            };
            if let Ok(frame) = CoverFrame::dummy(chosen_bucket, &mut *self.rng) {
                let target = active_peers[self.peer_cursor % active_peers.len()];
                self.peer_cursor = self.peer_cursor.wrapping_add(1);
                let bytes = frame.encode().len();
                remaining_budget = remaining_budget.saturating_sub(bytes);
                self.recent_bytes.push_back((now, bytes));
                self.dummy_frames_sent = self.dummy_frames_sent.saturating_add(1);
                self.dummy_bytes_sent = self.dummy_bytes_sent.saturating_add(bytes as u64);
                emitted.push((target, frame));
            }
        }

        let interval = self.next_interval();
        self.next_emit_at = Some(now + interval);
        emitted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    #[test]
    fn test_cover_default_off_and_codec_bounds() {
        let mgr = CoverTrafficManager::default();
        assert!(!mgr.is_enabled());

        let mut rng = DeterministicCoverRng::new(42);
        let d = CoverFrame::dummy(96, &mut rng).unwrap();
        let enc = d.encode();
        let dec = CoverFrame::decode(&enc).unwrap();
        assert_eq!(dec, d);

        // Invalid bucket rejected
        let mut bad = enc.clone();
        bad[6..8].copy_from_slice(&44u16.to_be_bytes());
        assert!(CoverFrame::decode(&bad).is_err());

        // Control frame with non-zero trailer rejected
        let mut bad_ctrl = CoverFrame::control(CoverFrameKind::Offer).encode();
        bad_ctrl.push(0x99);
        assert!(CoverFrame::decode(&bad_ctrl).is_err());
    }

    #[test]
    fn test_cover_negotiation_resource_and_budget_bounds() {
        let mut alice = CoverTrafficManager::new(
            CoverTrafficConfig {
                enabled: false,
                max_bytes_per_minute: 128,
                max_burst_frames: 1,
                min_interval: Duration::from_millis(20),
                max_interval: Duration::from_millis(20),
                max_session_lifetime: Duration::from_secs(60),
            },
            Box::new(DeterministicCoverRng::new(7)),
        );
        let mut bob = CoverTrafficManager::new(
            CoverTrafficConfig {
                enabled: false,
                ..CoverTrafficConfig::default()
            },
            Box::new(DeterministicCoverRng::new(9)),
        );

        let bob_id = Identity::generate().peer_id();
        let alice_id = Identity::generate().peer_id();
        let mut t = Instant::now();

        // Unsupported peer when Bob disabled -> Reject
        alice.set_enabled(true, t);
        let offer = alice.offer_peer(bob_id).unwrap();
        let resp = bob.handle_control(alice_id, &offer, t).unwrap();
        assert_eq!(resp.kind, CoverFrameKind::Reject);
        alice.handle_control(bob_id, &resp, t);
        assert_eq!(alice.peer_state(&bob_id), CoverPeerState::Unsupported);
        t += Duration::from_millis(50);
        assert!(alice
            .poll_due_dummies(
                t,
                &[bob_id],
                RelayResourceProfile::Balanced,
                ThermalState::Nominal,
                false
            )
            .is_empty());

        // Now reset & enable Bob -> mutually Negotiated
        alice.remove_peer(&bob_id);
        bob.set_enabled(true, t);
        let offer2 = alice.offer_peer(bob_id).unwrap();
        let accept = bob.handle_control(alice_id, &offer2, t).unwrap();
        assert_eq!(accept.kind, CoverFrameKind::Accept);
        alice.handle_control(bob_id, &accept, t);

        // Paused or real traffic pending suppresses cover
        t += Duration::from_millis(50);
        assert!(alice
            .poll_due_dummies(
                t,
                &[bob_id],
                RelayResourceProfile::Paused,
                ThermalState::Nominal,
                false
            )
            .is_empty());
        assert!(alice
            .poll_due_dummies(
                t,
                &[bob_id],
                RelayResourceProfile::Balanced,
                ThermalState::Nominal,
                true
            )
            .is_empty());

        // Emit until 128B/min budget exhausted
        let mut count = 0usize;
        for _ in 0..10 {
            t += Duration::from_millis(50);
            let out = alice.poll_due_dummies(
                t,
                &[bob_id],
                RelayResourceProfile::Balanced,
                ThermalState::Nominal,
                false,
            );
            count += out.len();
        }
        assert!(count >= 1);
        assert!(alice.dummy_bytes_sent <= 128);
    }
}
