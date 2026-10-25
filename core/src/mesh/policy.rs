use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use rand::Rng;

use crate::identity::ShortId;

// ponytail: adaptive relay policy with density-scaled jitter, counter-based suppression, and coverage-aware neighborhood tracking.

/// Context provided to relay policies when an envelope arrives for forwarding.
#[derive(Debug, Clone)]
pub struct RelayContext {
    pub neighbor_count: usize,
    pub hop_count: u8,
    pub priority: bool,
    pub duplicates_heard: u32,
    pub from_peer: Option<ShortId>,
    pub local_neighbors: Vec<ShortId>,
    pub duplicate_peers: Vec<ShortId>,
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
            duplicates_heard,
            from_peer: None,
            local_neighbors: Vec::new(),
            duplicate_peers: Vec::new(),
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
    /// Delay used for priority messages.
    pub priority_delay: Duration,
}

impl Default for AdaptiveFloodConfig {
    fn default() -> Self {
        Self {
            base_delay: Duration::from_millis(20),
            per_neighbor_delay: Duration::from_millis(15),
            max_delay: Duration::from_millis(300),
            suppression_threshold: 2,
            priority_delay: Duration::ZERO,
        }
    }
}

/// Adaptive mesh relay policy combining density-scaled random delays and counter-based suppression.
///
/// Reduces broadcast storm contention by spreading transmission bursts in dense topologies,
/// and cancels redundant rebroadcasts once a neighborhood is already covered by peer transmissions.
#[derive(Debug, Clone)]
pub struct AdaptiveFlood {
    config: AdaptiveFloodConfig,
}

impl Default for AdaptiveFlood {
    fn default() -> Self {
        Self::new(AdaptiveFloodConfig::default())
    }
}

impl AdaptiveFlood {
    pub fn new(config: AdaptiveFloodConfig) -> Self {
        Self { config }
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

    // =========================================================================
    // EXTENSION POINT (Phase 5): Device-Class-Aware Routing
    // In Phase 5, nodes will inspect battery levels, power state, and device role
    // (e.g. mains-powered gateway vs ultra-low-power BLE peripheral) to adjust
    // relay eligibility, delay windows, and duty cycles.
    // =========================================================================
    #[inline]
    pub fn is_relay_eligible(&self, _ctx: &RelayContext) -> bool {
        // Default behavior: all nodes participate in mesh forwarding.
        true
    }
}

impl RelayPolicy for AdaptiveFlood {
    fn should_relay(&self, ctx: &RelayContext) -> bool {
        self.is_relay_eligible(ctx)
    }

    fn delay(&self, ctx: &RelayContext) -> Duration {
        if ctx.priority {
            return self.config.priority_delay;
        }

        let upper = self.window_upper_bound(ctx.neighbor_count);
        let upper_ms = upper.as_millis() as u64;

        if upper_ms == 0 {
            Duration::ZERO
        } else {
            let mut rng = rand::thread_rng();
            let jitter_ms = rng.gen_range(0..=upper_ms);
            Duration::from_millis(jitter_ms)
        }
    }

    fn suppress_after(&self, ctx: &RelayContext) -> u8 {
        if ctx.priority {
            // Priority bypass: priority messages are NEVER suppressed
            u8::MAX
        } else {
            self.config.suppression_threshold
        }
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
            // Evict oldest entry
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

    /// Check whether a set of local neighbors is fully covered by overheard duplicate transmissions.
    pub fn is_coverage_complete(
        &self,
        local_neighbors: &[ShortId],
        duplicate_sources: &[ShortId],
        from_peer: Option<ShortId>,
        now: Instant,
    ) -> bool {
        if local_neighbors.is_empty() {
            return true;
        }

        // Neighbors needing coverage exclude the sender we received this packet from
        let mut uncovered: HashSet<ShortId> = local_neighbors.iter().copied().collect();
        if let Some(p) = from_peer {
            uncovered.remove(&p);
        }

        if uncovered.is_empty() {
            return true;
        }

        // Each duplicate source can cover itself and its freshly reported neighbors
        for src in duplicate_sources {
            uncovered.remove(src);

            if let Some(obs) = self.entries.get(src) {
                // Only fresh observations authorize coverage
                if let Some(rep_at) = obs.reported_at {
                    if now.saturating_duration_since(rep_at) <= self.ttl {
                        for peer in &obs.reported_neighbors {
                            uncovered.remove(peer);
                        }
                    }
                }
            }

            if uncovered.is_empty() {
                return true;
            }
        }

        false
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
        }
    }
}

/// Coverage-aware adaptive mesh relay policy.
///
/// Cancels pending redundant rebroadcasts ONLY when overheard duplicate transmissions
/// cryptographically or observationally confirm that ALL reachable local neighbors have
/// already been covered. Prevents premature bridge suppression in partitioned or bottleneck topologies.
#[derive(Debug, Clone)]
pub struct CoverageAwareFlood {
    config: CoverageAwareFloodConfig,
    table: Arc<RwLock<NeighborTable>>,
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
        Self { config, table }
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
        if ctx.priority {
            return self.config.priority_delay;
        }

        let upper = self.window_upper_bound(ctx.neighbor_count);
        let upper_ms = upper.as_millis() as u64;

        if upper_ms == 0 {
            Duration::ZERO
        } else {
            let mut rng = rand::thread_rng();
            let jitter_ms = rng.gen_range(0..=upper_ms);
            Duration::from_millis(jitter_ms)
        }
    }

    fn suppress_after(&self, ctx: &RelayContext) -> u8 {
        if ctx.priority {
            u8::MAX
        } else {
            self.config.suppression_threshold
        }
    }

    fn should_suppress(
        &self,
        ctx: &RelayContext,
        duplicate_sources: &[ShortId],
        from_peer: Option<ShortId>,
    ) -> bool {
        if ctx.priority {
            return false; // Priority never suppressed
        }

        // Must satisfy basic counter threshold first
        if ctx.duplicates_heard < self.config.suppression_threshold as u32 {
            return false;
        }

        // Check if coverage is complete across all local neighbors
        if let Ok(table) = self.table.read() {
            let now = Instant::now();
            table.is_coverage_complete(&ctx.local_neighbors, duplicate_sources, from_peer, now)
        } else {
            // Lock poisoned or contention: fallback to conservative forwarding
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

        let ctx = RelayContext {
            neighbor_count: 3,
            hop_count: 1,
            priority: false,
            duplicates_heard: 2,
            from_peer: Some(peer_a),
            local_neighbors: vec![peer_a, peer_b, peer_c],
            duplicate_peers: vec![peer_b],
        };

        // Duplicate from peer_b whose coverage includes peer_c -> full coverage -> suppress
        let suppress = policy.should_suppress(&ctx, &[peer_b], Some(peer_a));
        assert!(suppress);
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
            duplicates_heard: 3,
            from_peer: Some(node1),
            local_neighbors: vec![node1, node2, bridge15],
            duplicate_peers: vec![node1, node2],
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
            duplicates_heard: 2,
            from_peer: Some(peer_a),
            local_neighbors: vec![peer_a, peer_b, peer_c],
            duplicate_peers: vec![peer_b],
        };

        // Stale report -> cannot assume peer_c is covered -> do not suppress
        let suppress = policy.should_suppress(&ctx, &[peer_b], Some(peer_a));
        assert!(!suppress);
    }
}
