use std::time::Duration;

// ponytail: relay policy trait & naive flooding baseline. Deliberate zero-delay zero-suppression reference model.

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
