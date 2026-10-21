use rand::Rng;
use std::time::Duration;

// ponytail: adaptive relay policy with density-scaled jitter and counter-based suppression.

/// Context provided to relay policies when an envelope arrives for forwarding.
#[derive(Debug, Clone)]
pub struct RelayContext {
    pub neighbor_count: usize,
    pub hop_count: u8,
    pub priority: bool,
    pub duplicates_heard: u32,
}

/// Interface governing mesh forwarding decisions, timing, and collision suppression.
pub trait RelayPolicy: Send + Sync {
    /// Return `true` if this envelope should be forwarded.
    fn should_relay(&self, ctx: &RelayContext) -> bool;

    /// Return forwarding delay before transmission.
    fn delay(&self, ctx: &RelayContext) -> Duration;

    /// Number of duplicate receptions that will cancel a scheduled pending relay.
    fn suppress_after(&self, ctx: &RelayContext) -> u8;
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
