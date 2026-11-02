use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use rand::Rng;

use crate::identity::ShortId;

// ponytail: adaptive relay policy with seeded deterministic RNG, injected clock, bounded priority quota,
// and explicit separation of direct packet-observed vs third-party inferred neighborhood coverage.

/// Explicit confidence level of neighborhood coverage evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageConfidence {
    /// Neighbor discovery is empty or incomplete; cannot prove coverage.
    UnknownIncomplete,
    /// At least one local neighbor has neither transmitted a duplicate nor been covered.
    UncoveredNeighborsRemain,
    /// All local neighbors are covered, but relies on third-party claimed neighbor lists (inferred, not proven receipt).
    InferredComplete,
    /// Every local neighbor (excluding sender) has directly transmitted a duplicate of THIS specific packet.
    DirectlyObservedComplete,
}

/// Context provided to relay policies when an envelope arrives for forwarding.
#[derive(Debug, Clone)]
pub struct RelayContext {
    pub neighbor_count: usize,
    pub hop_count: u8,
    pub priority: bool,
    pub is_addressed: bool,
    pub duplicates_heard: u32,
    pub from_peer: Option<ShortId>,
    pub local_neighbors: Vec<ShortId>,
    pub duplicate_peers: Vec<ShortId>,
    pub now: Option<Instant>,
}

impl RelayContext {
    pub fn simple(
        neighbor_count: usize,
        hop_count: u8,
        priority: bool,
        duplicates_heard: u32,
    ) -> Self {
        Self {
            neighbor_count,
            hop_count,
            priority,
            is_addressed: false,
            duplicates_heard,
            from_peer: None,
            local_neighbors: Vec::new(),
            duplicate_peers: Vec::new(),
            now: None,
        }
    }
}

/// Deterministic splitmix64 PRNG for reproducible simulation and evaluation.
#[derive(Debug, Clone)]
pub struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    pub fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0x9E37_79B9_7F4A_7C15,
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn gen_range_inclusive(&mut self, max_inclusive: u64) -> u64 {
        if max_inclusive == 0 {
            return 0;
        }
        self.next_u64() % (max_inclusive.saturating_add(1))
    }
}

/// Bounded quota window preventing unauthenticated priority flags from granting unbounded bypass.
#[derive(Debug, Clone)]
pub struct PriorityQuotaWindow {
    window_start: Option<Instant>,
    window_duration: Duration,
    used_in_window: u32,
    max_per_window: u32,
}

impl PriorityQuotaWindow {
    pub fn new(max_per_window: u32, window_duration: Duration) -> Self {
        Self {
            window_start: None,
            window_duration,
            used_in_window: 0,
            max_per_window,
        }
    }

    pub fn try_consume(&mut self, now: Instant) -> bool {
        if self.max_per_window == 0 {
            return false;
        }
        match self.window_start {
            Some(start) if now.saturating_duration_since(start) < self.window_duration => {
                self.used_in_window = self.used_in_window.saturating_add(1);
                self.used_in_window <= self.max_per_window
            }
            _ => {
                self.window_start = Some(now);
                self.used_in_window = 1;
                true
            }
        }
    }

    pub fn is_within_quota(&self, now: Instant) -> bool {
        if self.max_per_window == 0 {
            return false;
        }
        match self.window_start {
            Some(start) if now.saturating_duration_since(start) < self.window_duration => {
                self.used_in_window <= self.max_per_window
            }
            _ => true,
        }
    }
}

/// Interface governing mesh forwarding decisions, timing, and collision suppression.
pub trait RelayPolicy: Send + Sync {
    /// Return `true` if this envelope should be forwarded.
    fn should_relay(&self, ctx: &RelayContext) -> bool;

    /// Return forwarding delay before transmission.
    fn delay(&self, ctx: &RelayContext) -> Duration;

    /// Number of duplicate receptions that will cancel a scheduled pending relay.
    fn suppress_after(&self, ctx: &RelayContext) -> u8;

    /// Check whether a pending relay should be suppressed given duplicates heard and coverage.
    fn should_suppress(
        &self,
        ctx: &RelayContext,
        _duplicate_sources: &[ShortId],
        _from_peer: Option<ShortId>,
    ) -> bool {
        ctx.duplicates_heard >= self.suppress_after(ctx) as u32
    }
}

/// Baseline relay policy: immediately relays every first-seen envelope without delay or suppression.
/// Serves as the naive flooding reference matching standard ad-hoc broadcast behavior.
#[derive(Debug, Clone, Default)]
pub struct NaiveFlood;

impl NaiveFlood {
    pub fn new() -> Self {
        Self
    }
}

impl RelayPolicy for NaiveFlood {
    fn should_relay(&self, _ctx: &RelayContext) -> bool {
        true
    }

    fn delay(&self, _ctx: &RelayContext) -> Duration {
        Duration::ZERO
    }

    fn suppress_after(&self, _ctx: &RelayContext) -> u8 {
        u8::MAX // Never suppress
    }
}

/// Configuration parameters for `AdaptiveFlood`.
#[derive(Debug, Clone)]
pub struct AdaptiveFloodConfig {
    /// Minimum jitter delay window base.
    pub base_delay: Duration,
    /// Additional delay window scaled per direct radio neighbor.
    pub per_neighbor_delay: Duration,
    /// Absolute maximum ceiling for the delay window.
    pub max_delay: Duration,
    /// Number of duplicate packets heard that cancels pending relay (k threshold).
    pub suppression_threshold: u8,
    /// Delay used for priority messages within quota.
    pub priority_delay: Duration,
    /// Maximum priority packets permitted to bypass delay/suppression per quota window.
    pub max_priority_per_window: u32,
    /// Duration of the priority bypass quota window.
    pub priority_window: Duration,
}

impl Default for AdaptiveFloodConfig {
    fn default() -> Self {
        Self {
            base_delay: Duration::from_millis(20),
            per_neighbor_delay: Duration::from_millis(15),
            max_delay: Duration::from_millis(300),
            suppression_threshold: 2,
            priority_delay: Duration::ZERO,
            max_priority_per_window: 8,
            priority_window: Duration::from_secs(10),
        }
    }
}

/// Adaptive mesh relay policy combining density-scaled random delays and counter-based suppression.
#[derive(Debug, Clone)]
pub struct AdaptiveFlood {
    config: AdaptiveFloodConfig,
    seeded_rng: Option<Arc<Mutex<DeterministicRng>>>,
    priority_quota: Arc<Mutex<PriorityQuotaWindow>>,
}

impl Default for AdaptiveFlood {
    fn default() -> Self {
        Self::new(AdaptiveFloodConfig::default())
    }
}

impl AdaptiveFlood {
    pub fn new(config: AdaptiveFloodConfig) -> Self {
        let quota =
            PriorityQuotaWindow::new(config.max_priority_per_window, config.priority_window);
        Self {
            config,
            seeded_rng: None,
            priority_quota: Arc::new(Mutex::new(quota)),
        }
    }

    pub fn with_seed(config: AdaptiveFloodConfig, seed: u64) -> Self {
        let quota =
            PriorityQuotaWindow::new(config.max_priority_per_window, config.priority_window);
        Self {
            config,
            seeded_rng: Some(Arc::new(Mutex::new(DeterministicRng::new(seed)))),
            priority_quota: Arc::new(Mutex::new(quota)),
        }
    }

    pub fn config(&self) -> &AdaptiveFloodConfig {
        &self.config
    }

    /// Calculate the upper bound of the random delay window D(neighbor_count).
    pub fn window_upper_bound(&self, neighbor_count: usize) -> Duration {
        let base_ms = self.config.base_delay.as_millis();
        let added_ms = neighbor_count as u128 * self.config.per_neighbor_delay.as_millis();
        let total_ms = base_ms.saturating_add(added_ms);
        let capped_ms = total_ms.min(self.config.max_delay.as_millis());
        Duration::from_millis(capped_ms as u64)
    }

    #[inline]
    pub fn is_relay_eligible(&self, _ctx: &RelayContext) -> bool {
        true
    }
}

impl RelayPolicy for AdaptiveFlood {
    fn should_relay(&self, ctx: &RelayContext) -> bool {
        self.is_relay_eligible(ctx)
    }

    fn delay(&self, ctx: &RelayContext) -> Duration {
        let now = ctx.now.unwrap_or_else(Instant::now);
        if ctx.priority {
            if let Ok(mut q) = self.priority_quota.lock() {
                if q.try_consume(now) {
                    return self.config.priority_delay;
                }
            }
        }

        let upper = self.window_upper_bound(ctx.neighbor_count);
        let upper_ms = upper.as_millis() as u64;

        if upper_ms == 0 {
            Duration::ZERO
        } else if let Some(ref rng_arc) = self.seeded_rng {
            let jitter_ms = rng_arc
                .lock()
                .map(|mut r| r.gen_range_inclusive(upper_ms))
                .unwrap_or(upper_ms / 2);
            Duration::from_millis(jitter_ms)
        } else {
            let mut rng = rand::thread_rng();
            let jitter_ms = rng.gen_range(0..=upper_ms);
            Duration::from_millis(jitter_ms)
        }
    }

    fn suppress_after(&self, ctx: &RelayContext) -> u8 {
        let now = ctx.now.unwrap_or_else(Instant::now);
        if ctx.priority {
            if let Ok(q) = self.priority_quota.lock() {
                if q.is_within_quota(now) {
                    return u8::MAX;
                }
            }
        }
        self.config.suppression_threshold
    }
}

/// Status of an observed or advertised neighbor identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeighborStatus {
    /// Directly observed via local radio/BLE frames.
    Observed,
    /// Claimed as a neighbor by another peer in a neighbor advertisement.
    Claimed,
    /// Cryptographically verified through an established Noise session.
    Verified,
}

/// Tracking record for a peer's observed state and known neighbor coverage.
#[derive(Debug, Clone)]
pub struct NeighborObservation {
    pub peer: ShortId,
    pub status: NeighborStatus,
    pub last_seen: Instant,
    pub reported_neighbors: HashSet<ShortId>,
    pub reported_at: Option<Instant>,
}

/// Bounded in-memory table storing neighbor observations and coverage sets.
#[derive(Debug, Clone)]
pub struct NeighborTable {
    entries: HashMap<ShortId, NeighborObservation>,
    max_entries: usize,
    ttl: Duration,
}

impl Default for NeighborTable {
    fn default() -> Self {
        Self::new(64, Duration::from_secs(30))
    }
}

impl NeighborTable {
    pub fn new(max_entries: usize, ttl: Duration) -> Self {
        Self {
            entries: HashMap::with_capacity(max_entries),
            max_entries,
            ttl,
        }
    }

    pub fn record_observed(&mut self, peer: ShortId, now: Instant) {
        self.cleanup_stale(now);
        if self.entries.len() >= self.max_entries && !self.entries.contains_key(&peer) {
            if let Some((&oldest, _)) = self.entries.iter().min_by_key(|(_, v)| v.last_seen) {
                self.entries.remove(&oldest);
            }
        }

        let entry = self
            .entries
            .entry(peer)
            .or_insert_with(|| NeighborObservation {
                peer,
                status: NeighborStatus::Observed,
                last_seen: now,
                reported_neighbors: HashSet::new(),
                reported_at: None,
            });
        entry.last_seen = now;
        if entry.status == NeighborStatus::Claimed {
            entry.status = NeighborStatus::Observed;
        }
    }

    pub fn record_verified(&mut self, peer: ShortId, now: Instant) {
        self.record_observed(peer, now);
        if let Some(entry) = self.entries.get_mut(&peer) {
            entry.status = NeighborStatus::Verified;
        }
    }

    pub fn record_claimed_neighbors(
        &mut self,
        peer: ShortId,
        neighbors: Vec<ShortId>,
        now: Instant,
    ) {
        self.record_observed(peer, now);
        if let Some(entry) = self.entries.get_mut(&peer) {
            entry.reported_neighbors = neighbors.into_iter().collect();
            entry.reported_at = Some(now);
        }
    }

    pub fn cleanup_stale(&mut self, now: Instant) {
        let ttl = self.ttl;
        self.entries
            .retain(|_, obs| now.saturating_duration_since(obs.last_seen) <= ttl);
    }

    /// Evaluate neighborhood coverage with explicit distinction between:
    /// - `DirectlyObservedComplete`: every non-sender local neighbor directly transmitted a duplicate of THIS packet.
    /// - `InferredComplete`: remaining local neighbors are only listed in third-party `reported_neighbors` claims.
    /// - `UnknownIncomplete`: `local_neighbors` is empty (incomplete discovery) or contains only `from_peer` with no other verified peers.
    /// - `UncoveredNeighborsRemain`: at least one local neighbor is neither directly observed nor inferred covered.
    pub fn evaluate_coverage(
        &self,
        local_neighbors: &[ShortId],
        duplicate_sources: &[ShortId],
        from_peer: Option<ShortId>,
        now: Instant,
    ) -> CoverageConfidence {
        // Empty neighbor list indicates incomplete or unknown discovery; NEVER auto-prove coverage!
        if local_neighbors.is_empty() {
            return CoverageConfidence::UnknownIncomplete;
        }

        let mut direct_uncovered: HashSet<ShortId> = local_neighbors.iter().copied().collect();
        if let Some(p) = from_peer {
            direct_uncovered.remove(&p);
        }

        // If our only neighbor is the sender we received this frame from, there are no other
        // known downstream neighbors to cover, unless duplicate_sources is also empty (dead-end leaf).
        if direct_uncovered.is_empty() {
            return CoverageConfidence::UnknownIncomplete;
        }

        // 1. Subtract only peers that directly transmitted a duplicate of THIS packet
        for src in duplicate_sources {
            direct_uncovered.remove(src);
        }

        if direct_uncovered.is_empty() {
            return CoverageConfidence::DirectlyObservedComplete;
        }

        // 2. Check whether third-party reported_neighbors lists infer coverage of the remainder
        let mut inferred_uncovered = direct_uncovered;
        for src in duplicate_sources {
            if let Some(obs) = self.entries.get(src) {
                if let Some(rep_at) = obs.reported_at {
                    if now.saturating_duration_since(rep_at) <= self.ttl {
                        for peer in &obs.reported_neighbors {
                            inferred_uncovered.remove(peer);
                        }
                    }
                }
            }
            if inferred_uncovered.is_empty() {
                return CoverageConfidence::InferredComplete;
            }
        }

        CoverageConfidence::UncoveredNeighborsRemain
    }
}

/// Configuration parameters for `CoverageAwareFlood`.
#[derive(Debug, Clone)]
pub struct CoverageAwareFloodConfig {
    pub base_delay: Duration,
    pub per_neighbor_delay: Duration,
    pub max_delay: Duration,
    pub suppression_threshold: u8,
    pub priority_delay: Duration,
    pub neighbor_ttl: Duration,
    pub max_neighbors: usize,
    /// Whether unaddressed broadcast flood packets may be suppressed via inferred 3rd-party neighbor lists.
    /// Reliable addressed traffic NEVER suppresses on inferred coverage alone.
    pub allow_inferred_broadcast_suppression: bool,
    pub max_priority_per_window: u32,
    pub priority_window: Duration,
}

impl Default for CoverageAwareFloodConfig {
    fn default() -> Self {
        Self {
            base_delay: Duration::from_millis(20),
            per_neighbor_delay: Duration::from_millis(15),
            max_delay: Duration::from_millis(300),
            suppression_threshold: 2,
            priority_delay: Duration::ZERO,
            neighbor_ttl: Duration::from_secs(30),
            max_neighbors: 64,
            allow_inferred_broadcast_suppression: true,
            max_priority_per_window: 8,
            priority_window: Duration::from_secs(10),
        }
    }
}

/// Coverage-aware adaptive mesh relay policy.
///
/// Distinguishes packet-specific `DirectlyObservedComplete` coverage from third-party
/// `InferredComplete` neighbor-list coverage. For reliable addressed traffic, suppression
/// requires direct packet observation from all local neighbors; inferred lists never suppress
/// addressed frames.
#[derive(Debug, Clone)]
pub struct CoverageAwareFlood {
    config: CoverageAwareFloodConfig,
    table: Arc<RwLock<NeighborTable>>,
    seeded_rng: Option<Arc<Mutex<DeterministicRng>>>,
    priority_quota: Arc<Mutex<PriorityQuotaWindow>>,
}

impl Default for CoverageAwareFlood {
    fn default() -> Self {
        Self::new(CoverageAwareFloodConfig::default())
    }
}

impl CoverageAwareFlood {
    pub fn new(config: CoverageAwareFloodConfig) -> Self {
        let table = Arc::new(RwLock::new(NeighborTable::new(
            config.max_neighbors,
            config.neighbor_ttl,
        )));
        let quota =
            PriorityQuotaWindow::new(config.max_priority_per_window, config.priority_window);
        Self {
            config,
            table,
            seeded_rng: None,
            priority_quota: Arc::new(Mutex::new(quota)),
        }
    }

    pub fn with_seed(config: CoverageAwareFloodConfig, seed: u64) -> Self {
        let table = Arc::new(RwLock::new(NeighborTable::new(
            config.max_neighbors,
            config.neighbor_ttl,
        )));
        let quota =
            PriorityQuotaWindow::new(config.max_priority_per_window, config.priority_window);
        Self {
            config,
            table,
            seeded_rng: Some(Arc::new(Mutex::new(DeterministicRng::new(seed)))),
            priority_quota: Arc::new(Mutex::new(quota)),
        }
    }

    pub fn table(&self) -> &Arc<RwLock<NeighborTable>> {
        &self.table
    }

    pub fn record_observed(&self, peer: ShortId, now: Instant) {
        if let Ok(mut t) = self.table.write() {
            t.record_observed(peer, now);
        }
    }

    pub fn record_verified(&self, peer: ShortId, now: Instant) {
        if let Ok(mut t) = self.table.write() {
            t.record_verified(peer, now);
        }
    }

    pub fn record_claimed_neighbors(&self, peer: ShortId, neighbors: Vec<ShortId>, now: Instant) {
        if let Ok(mut t) = self.table.write() {
            t.record_claimed_neighbors(peer, neighbors, now);
        }
    }

    pub fn window_upper_bound(&self, neighbor_count: usize) -> Duration {
        let base_ms = self.config.base_delay.as_millis();
        let added_ms = neighbor_count as u128 * self.config.per_neighbor_delay.as_millis();
        let total_ms = base_ms.saturating_add(added_ms);
        let capped_ms = total_ms.min(self.config.max_delay.as_millis());
        Duration::from_millis(capped_ms as u64)
    }
}

impl RelayPolicy for CoverageAwareFlood {
    fn should_relay(&self, _ctx: &RelayContext) -> bool {
        true
    }

    fn delay(&self, ctx: &RelayContext) -> Duration {
        let now = ctx.now.unwrap_or_else(Instant::now);
        if ctx.priority {
            if let Ok(mut q) = self.priority_quota.lock() {
                if q.try_consume(now) {
                    return self.config.priority_delay;
                }
            }
        }

        let upper = self.window_upper_bound(ctx.neighbor_count);
        let upper_ms = upper.as_millis() as u64;

        if upper_ms == 0 {
            Duration::ZERO
        } else if let Some(ref rng_arc) = self.seeded_rng {
            let jitter_ms = rng_arc
                .lock()
                .map(|mut r| r.gen_range_inclusive(upper_ms))
                .unwrap_or(upper_ms / 2);
            Duration::from_millis(jitter_ms)
        } else {
            let mut rng = rand::thread_rng();
            let jitter_ms = rng.gen_range(0..=upper_ms);
            Duration::from_millis(jitter_ms)
        }
    }

    fn suppress_after(&self, ctx: &RelayContext) -> u8 {
        let now = ctx.now.unwrap_or_else(Instant::now);
        if ctx.priority {
            if let Ok(q) = self.priority_quota.lock() {
                if q.is_within_quota(now) {
                    return u8::MAX;
                }
            }
        }
        self.config.suppression_threshold
    }

    fn should_suppress(
        &self,
        ctx: &RelayContext,
        duplicate_sources: &[ShortId],
        from_peer: Option<ShortId>,
    ) -> bool {
        let now = ctx.now.unwrap_or_else(Instant::now);
        if ctx.priority {
            if let Ok(q) = self.priority_quota.lock() {
                if q.is_within_quota(now) {
                    return false;
                }
            }
        }

        // Must satisfy basic counter threshold first
        if ctx.duplicates_heard < self.config.suppression_threshold as u32 {
            return false;
        }

        if let Ok(table) = self.table.read() {
            match table.evaluate_coverage(&ctx.local_neighbors, duplicate_sources, from_peer, now) {
                CoverageConfidence::DirectlyObservedComplete => true,
                CoverageConfidence::InferredComplete => {
                    // Addressed reliable traffic must NEVER be suppressed by 3rd-party inferred neighbor lists
                    !ctx.is_addressed && self.config.allow_inferred_broadcast_suppression
                }
                CoverageConfidence::UnknownIncomplete
                | CoverageConfidence::UncoveredNeighborsRemain => false,
            }
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coverage_aware_suppression_in_dense_cluster() {
        let policy = CoverageAwareFlood::default();
        let now = Instant::now();

        let me = ShortId([1; 8]);
        let peer_a = ShortId([2; 8]);
        let peer_b = ShortId([3; 8]);
        let peer_c = ShortId([4; 8]);

        // In dense cluster, peer_b reports seeing peer_a, peer_b, peer_c, me
        policy.record_claimed_neighbors(peer_b, vec![peer_a, peer_c, me], now);

        // Unaddressed broadcast frame can use inferred coverage in dense cluster
        let ctx_broadcast = RelayContext {
            neighbor_count: 3,
            hop_count: 1,
            priority: false,
            is_addressed: false,
            duplicates_heard: 2,
            from_peer: Some(peer_a),
            local_neighbors: vec![peer_a, peer_b, peer_c],
            duplicate_peers: vec![peer_b],
            now: Some(now),
        };
        assert!(policy.should_suppress(&ctx_broadcast, &[peer_b], Some(peer_a)));

        // Reliable addressed traffic is NOT suppressed merely by peer_b's claimed neighbor list!
        let mut ctx_addressed = ctx_broadcast.clone();
        ctx_addressed.is_addressed = true;
        assert!(!policy.should_suppress(&ctx_addressed, &[peer_b], Some(peer_a)));

        // Once BOTH peer_b and peer_c directly transmit duplicates, direct observed coverage is complete -> suppress!
        assert!(policy.should_suppress(&ctx_addressed, &[peer_b, peer_c], Some(peer_a)));
    }

    #[test]
    fn test_empty_or_incomplete_discovery_never_proves_coverage() {
        let policy = CoverageAwareFlood::default();
        let now = Instant::now();
        let peer_a = ShortId([2; 8]);
        let peer_b = ShortId([3; 8]);

        // Empty local_neighbors (discovery incomplete) must return UnknownIncomplete and NOT suppress
        let ctx_empty = RelayContext {
            neighbor_count: 0,
            hop_count: 1,
            priority: false,
            is_addressed: false,
            duplicates_heard: 5,
            from_peer: Some(peer_a),
            local_neighbors: Vec::new(),
            duplicate_peers: vec![peer_b],
            now: Some(now),
        };
        assert!(!policy.should_suppress(&ctx_empty, &[peer_b], Some(peer_a)));
    }

    #[test]
    fn test_asymmetric_links_and_lost_duplicate_do_not_suppress() {
        let policy = CoverageAwareFlood::default();
        let now = Instant::now();

        let peer_a = ShortId([2; 8]);
        let peer_b = ShortId([3; 8]);
        let peer_c = ShortId([4; 8]); // Asymmetric or lost duplicate from peer_c

        let ctx = RelayContext {
            neighbor_count: 3,
            hop_count: 1,
            priority: false,
            is_addressed: true,
            duplicates_heard: 2,
            from_peer: Some(peer_a),
            local_neighbors: vec![peer_a, peer_b, peer_c],
            duplicate_peers: vec![peer_b],
            now: Some(now),
        };

        // Only peer_b was heard directly; peer_c never transmitted a duplicate -> must NOT suppress
        assert!(!policy.should_suppress(&ctx, &[peer_b], Some(peer_a)));
    }

    #[test]
    fn test_forged_priority_flood_bounded_by_quota() {
        let cfg = CoverageAwareFloodConfig {
            max_priority_per_window: 2,
            priority_window: Duration::from_secs(10),
            ..Default::default()
        };
        let policy = CoverageAwareFlood::with_seed(cfg, 42);
        let now = Instant::now();
        let peer_a = ShortId([2; 8]);
        let peer_b = ShortId([3; 8]);

        let ctx = RelayContext {
            neighbor_count: 4,
            hop_count: 1,
            priority: true,
            is_addressed: false,
            duplicates_heard: 3,
            from_peer: Some(peer_a),
            local_neighbors: vec![peer_a, peer_b],
            duplicate_peers: vec![peer_b],
            now: Some(now),
        };

        // First 2 priority frames within window get 0 delay and bypass suppression
        assert_eq!(policy.delay(&ctx), Duration::ZERO);
        assert!(!policy.should_suppress(&ctx, &[peer_b], Some(peer_a)));
        assert_eq!(policy.delay(&ctx), Duration::ZERO);
        assert!(!policy.should_suppress(&ctx, &[peer_b], Some(peer_a)));

        // 3rd priority frame exceeds quota -> falls back to normal jitter delay and normal suppression!
        assert!(policy.delay(&ctx) > Duration::ZERO);
        assert!(policy.should_suppress(&ctx, &[peer_b], Some(peer_a)));
    }

    #[test]
    fn test_matched_seed_deterministic_reproducibility() {
        let p1 = CoverageAwareFlood::with_seed(CoverageAwareFloodConfig::default(), 9999);
        let p2 = CoverageAwareFlood::with_seed(CoverageAwareFloodConfig::default(), 9999);
        let now = Instant::now();
        let mut ctx = RelayContext::simple(5, 1, false, 0);
        ctx.now = Some(now);

        let seq1: Vec<Duration> = (0..20).map(|_| p1.delay(&ctx)).collect();
        let seq2: Vec<Duration> = (0..20).map(|_| p2.delay(&ctx)).collect();
        assert_eq!(seq1, seq2);
    }

    #[test]
    fn test_coverage_aware_prevents_partitioned_bridge_suppression() {
        let policy = CoverageAwareFlood::default();
        let now = Instant::now();

        let node14 = ShortId([14; 8]);
        let bridge15 = ShortId([15; 8]);
        let node1 = ShortId([1; 8]);
        let node2 = ShortId([2; 8]);

        // Nodes 1 and 2 in cluster A only see cluster A nodes (they do not see bridge15)
        policy.record_claimed_neighbors(node1, vec![node2, node14], now);
        policy.record_claimed_neighbors(node2, vec![node1, node14], now);

        // Node 14 has local neighbors node1, node2, and bridge15
        let ctx = RelayContext {
            neighbor_count: 3,
            hop_count: 1,
            priority: false,
            is_addressed: false,
            duplicates_heard: 3,
            from_peer: Some(node1),
            local_neighbors: vec![node1, node2, bridge15],
            duplicate_peers: vec![node1, node2],
            now: Some(now),
        };

        // Duplicates heard from node1 and node2 do NOT cover bridge15 -> must NOT suppress!
        let suppress = policy.should_suppress(&ctx, &[node1, node2], Some(node1));
        assert!(!suppress);
    }

    #[test]
    fn test_stale_coverage_falls_back_to_forwarding() {
        let policy = CoverageAwareFlood::default();
        let now = Instant::now();
        let old_time = now - Duration::from_secs(60); // older than 30s TTL

        let peer_a = ShortId([2; 8]);
        let peer_b = ShortId([3; 8]);
        let peer_c = ShortId([4; 8]);

        policy.record_claimed_neighbors(peer_b, vec![peer_c], old_time);

        let ctx = RelayContext {
            neighbor_count: 3,
            hop_count: 1,
            priority: false,
            is_addressed: false,
            duplicates_heard: 2,
            from_peer: Some(peer_a),
            local_neighbors: vec![peer_a, peer_b, peer_c],
            duplicate_peers: vec![peer_b],
            now: Some(now),
        };

        // Stale report -> cannot assume peer_c is covered -> do not suppress
        let suppress = policy.should_suppress(&ctx, &[peer_b], Some(peer_a));
        assert!(!suppress);
    }
}
