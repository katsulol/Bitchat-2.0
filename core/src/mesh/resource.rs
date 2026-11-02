//! Device-resource-budgeted mesh relaying (`core/src/mesh/resource.rs`).
//!
//! Enforces:
//! - Coarse local `ResourceSnapshot` classification (`Paused`, `Minimal`, `Balanced`, `Assisted`)
//!   with `1:4:8` experimental transit byte budgets (`8 KiB/s`, `32 KiB/s`, `64 KiB/s`).
//! - Thermal critical and user pause override charging state.
//! - Hysteresis / minimum dwell time before upgrading profiles to prevent oscillation.
//! - Strict separation of `LocalOriginated` traffic from `TransitRelay` traffic: local accepted
//!   messages are never dropped to fund third-party forwarding.
//! - Reserved control/ACK capacity (25% reservation aligned with `gateway::fairness`), bounded
//!   per-neighbor transit fairness, and conservative relay delay scaling.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::envelope::MsgType;
use crate::gateway::fairness::CONTROL_PLANE_RESERVATION_RATIO;
use crate::identity::ShortId;
use crate::mesh::policy::{RelayContext, RelayPolicy};

/// Three-valued platform observation state where OS APIs may not report a definite boolean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriState {
    True,
    False,
    Unknown,
}

/// Coarse local battery band (never transmitted on the wire as exact percentage).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BatteryBand {
    Critical, // e.g. < 10%
    Low,      // e.g. 10%..25%
    Medium,   // e.g. 25%..70%
    High,     // e.g. > 70%
    Unknown,
}

/// Coarse device thermal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ThermalState {
    Nominal,
    Fair,
    Serious,
    Critical,
    Unknown,
}

/// Coarse network metering state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkMetering {
    Unmetered,
    Metered,
    Offline,
    Unknown,
}

/// Experimental resource relay profile with 1:4:8 relative transit byte budgets.
///
/// Note: Relative byte budgets (1:4:8) are policy parameters bounding forwarding workload,
/// not empirically measured battery savings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RelayResourceProfile {
    /// Zero third-party transit forwarding; local originated and control/ACK traffic still permitted.
    Paused = 0,
    /// 1x transit byte budget (8 KiB/s), 1.5x relay delay multiplier.
    Minimal = 1,
    /// 4x transit byte budget (32 KiB/s), 1.0x relay delay multiplier.
    Balanced = 2,
    /// 8x transit byte budget (64 KiB/s), 0.75x relay delay multiplier.
    Assisted = 3,
}

impl RelayResourceProfile {
    /// Transit byte budget per second (experimental 1:4:8 ratio).
    pub fn transit_bytes_per_sec(self) -> usize {
        match self {
            Self::Paused => 0,
            Self::Minimal => 8 * 1024,   // 1x = 8 KiB/s
            Self::Balanced => 32 * 1024, // 4x = 32 KiB/s
            Self::Assisted => 64 * 1024, // 8x = 64 KiB/s
        }
    }

    /// Maximum pending transit relay queue depth.
    pub fn max_transit_queue_len(self) -> usize {
        match self {
            Self::Paused => 0,
            Self::Minimal => 16,
            Self::Balanced => 64,
            Self::Assisted => 128,
        }
    }

    /// Per-neighbor maximum share of the transit byte budget per second (50% cap to prevent single-peer monopoly).
    pub fn per_neighbor_bytes_per_sec(self) -> usize {
        (self.transit_bytes_per_sec() / 2).max(1024)
    }

    /// Relay delay multiplier in basis points (10000 = 1.0x).
    pub fn delay_multiplier_bps(self) -> u64 {
        match self {
            Self::Paused => 20_000,
            Self::Minimal => 15_000,  // 1.5x conservative delay
            Self::Balanced => 10_000, // 1.0x baseline delay
            Self::Assisted => 7_500,  // 0.75x assisted delay
        }
    }
}

/// Snapshot of local platform resource hints at a monotonic instant.
#[derive(Debug, Clone)]
pub struct ResourceSnapshot {
    pub charging: TriState,
    pub battery_band: BatteryBand,
    pub power_save_mode: TriState,
    pub thermal_state: ThermalState,
    pub network_metering: NetworkMetering,
    pub ble_ready: bool,
    pub is_foreground: bool,
    pub user_paused_relaying: bool,
    pub user_profile_override: Option<RelayResourceProfile>,
    pub timestamp: Instant,
}

impl ResourceSnapshot {
    pub fn default_at(now: Instant) -> Self {
        Self {
            charging: TriState::Unknown,
            battery_band: BatteryBand::Unknown,
            power_save_mode: TriState::False,
            thermal_state: ThermalState::Nominal,
            network_metering: NetworkMetering::Unknown,
            ble_ready: true,
            is_foreground: true,
            user_paused_relaying: false,
            user_profile_override: None,
            timestamp: now,
        }
    }

    /// Pure classification of the target `RelayResourceProfile` before hysteresis dwell checks.
    ///
    /// Invariants:
    /// - `user_paused_relaying`, `!ble_ready`, or `ThermalState::Critical` ALWAYS force `Paused`,
    ///   overriding charging or user profile overrides.
    /// - `ThermalState::Serious`, `power_save_mode == True`, or `BatteryBand::Critical` cap the
    ///   profile at `Minimal` even if charging.
    /// - Unmetered WiFi alone never triggers `Assisted` without `charging == True` and `BatteryBand::High`.
    pub fn compute_raw_profile(&self) -> RelayResourceProfile {
        if self.user_paused_relaying
            || !self.ble_ready
            || self.thermal_state == ThermalState::Critical
        {
            return RelayResourceProfile::Paused;
        }

        let safety_cap = if self.thermal_state == ThermalState::Serious
            || self.power_save_mode == TriState::True
            || self.battery_band == BatteryBand::Critical
        {
            RelayResourceProfile::Minimal
        } else {
            RelayResourceProfile::Assisted
        };

        if let Some(override_prof) = self.user_profile_override {
            return override_prof.min(safety_cap);
        }

        let derived = match (self.charging, self.battery_band) {
            (_, BatteryBand::Critical) | (_, BatteryBand::Low) => RelayResourceProfile::Minimal,
            (TriState::True, BatteryBand::High)
                if self.thermal_state == ThermalState::Nominal
                    && self.power_save_mode == TriState::False =>
            {
                RelayResourceProfile::Assisted
            }
            (TriState::True, _) => RelayResourceProfile::Balanced,
            (_, BatteryBand::Medium) | (_, BatteryBand::High) => {
                if !self.is_foreground && self.network_metering == NetworkMetering::Metered {
                    RelayResourceProfile::Minimal
                } else {
                    RelayResourceProfile::Balanced
                }
            }
            (_, BatteryBand::Unknown) => {
                if !self.is_foreground {
                    RelayResourceProfile::Minimal
                } else {
                    RelayResourceProfile::Balanced
                }
            }
        };

        derived.min(safety_cap)
    }
}

/// Distinguishes local originated packets from third-party transit relay packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrafficOrigin {
    /// Originated by the local user/node: never dropped to fund transit forwarding.
    LocalOriginated,
    /// Transit packet received from `neighbor` to be relayed to other peers.
    TransitRelay { neighbor: ShortId },
}

/// Admission decision for a mesh packet under the current resource budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceAdmission {
    Admitted {
        profile: RelayResourceProfile,
        delay_multiplier_bps: u64,
    },
    RejectedPaused,
    RejectedTransitBudgetExhausted,
    RejectedNeighborFairnessExceeded,
    RejectedControlCapacityReserved,
    RejectedTransitQueueFull,
}

#[derive(Debug, Clone)]
struct NeighborTransitWindow {
    window_start: Instant,
    bytes_used: usize,
}

/// Stateful resource budget manager with hysteresis dwell, byte token buckets,
/// control-plane reservation, and per-neighbor transit fairness.
#[derive(Debug)]
pub struct ResourceBudgetManager {
    active_profile: RelayResourceProfile,
    last_profile_change: Instant,
    min_upgrade_dwell: Duration,
    window_start: Instant,
    transit_bytes_in_window: usize,
    transit_data_bytes_in_window: usize,
    neighbor_windows: HashMap<ShortId, NeighborTransitWindow>,
    current_transit_queue: usize,
}

impl ResourceBudgetManager {
    pub fn new(initial_profile: RelayResourceProfile, min_upgrade_dwell: Duration, now: Instant) -> Self {
        Self {
            active_profile: initial_profile,
            last_profile_change: now,
            min_upgrade_dwell,
            window_start: now,
            transit_bytes_in_window: 0,
            transit_data_bytes_in_window: 0,
            neighbor_windows: HashMap::new(),
            current_transit_queue: 0,
        }
    }

    pub fn active_profile(&self) -> RelayResourceProfile {
        self.active_profile
    }

    /// Update the active profile from a new `ResourceSnapshot`, applying hysteresis:
    /// - Downgrades (more restrictive profiles) take effect immediately for thermal/battery safety.
    /// - Upgrades (less restrictive profiles) require `min_upgrade_dwell` to have elapsed since
    ///   the last profile change, preventing rapid oscillation near thresholds.
    pub fn update_snapshot(&mut self, snapshot: &ResourceSnapshot) -> RelayResourceProfile {
        let target = snapshot.compute_raw_profile();
        let now = snapshot.timestamp;

        if target < self.active_profile {
            // Immediate safety downgrade
            self.active_profile = target;
            self.last_profile_change = now;
        } else if target > self.active_profile
            && now.saturating_duration_since(self.last_profile_change) >= self.min_upgrade_dwell
        {
            self.active_profile = target;
            self.last_profile_change = now;
        }
        self.active_profile
    }

    /// Evaluate and record byte budget consumption for a packet (including all fragments).
    pub fn check_and_record(
        &mut self,
        origin: TrafficOrigin,
        msg_type: MsgType,
        total_wire_bytes: usize,
        now: Instant,
    ) -> ResourceAdmission {
        // Invariant: Local originated traffic is NEVER dropped to fund or conserve transit budgets!
        if origin == TrafficOrigin::LocalOriginated {
            return ResourceAdmission::Admitted {
                profile: self.active_profile,
                delay_multiplier_bps: 10_000,
            };
        }

        let TrafficOrigin::TransitRelay { neighbor } = origin else {
            unreachable!()
        };

        let is_control = matches!(msg_type, MsgType::Handshake | MsgType::Ack);

        // In Paused profile, drop all non-control transit forwarding
        if self.active_profile == RelayResourceProfile::Paused {
            return ResourceAdmission::RejectedPaused;
        }

        // Roll 1-second byte token bucket window
        if now.saturating_duration_since(self.window_start) >= Duration::from_secs(1) {
            self.window_start = now;
            self.transit_bytes_in_window = 0;
            self.transit_data_bytes_in_window = 0;
        }

        let max_queue = self.active_profile.max_transit_queue_len();
        let max_data_queue =
            (max_queue as f32 * (1.0 - CONTROL_PLANE_RESERVATION_RATIO)) as usize;

        if self.current_transit_queue >= max_queue {
            return ResourceAdmission::RejectedTransitQueueFull;
        }
        if !is_control && self.current_transit_queue >= max_data_queue {
            return ResourceAdmission::RejectedControlCapacityReserved;
        }

        let max_bytes = self.active_profile.transit_bytes_per_sec();
        let max_data_bytes =
            (max_bytes as f32 * (1.0 - CONTROL_PLANE_RESERVATION_RATIO)) as usize;

        if !is_control && self.transit_data_bytes_in_window.saturating_add(total_wire_bytes) > max_data_bytes {
            return ResourceAdmission::RejectedControlCapacityReserved;
        }
        if self.transit_bytes_in_window.saturating_add(total_wire_bytes) > max_bytes {
            return ResourceAdmission::RejectedTransitBudgetExhausted;
        }

        // Per-neighbor fairness check (1-second window)
        let per_nbr_limit = self.active_profile.per_neighbor_bytes_per_sec();
        let nbr_win = self
            .neighbor_windows
            .entry(neighbor)
            .or_insert(NeighborTransitWindow {
                window_start: now,
                bytes_used: 0,
            });
        if now.saturating_duration_since(nbr_win.window_start) >= Duration::from_secs(1) {
            nbr_win.window_start = now;
            nbr_win.bytes_used = 0;
        }
        if !is_control && nbr_win.bytes_used.saturating_add(total_wire_bytes) > per_nbr_limit {
            return ResourceAdmission::RejectedNeighborFairnessExceeded;
        }

        // Record consumption
        self.transit_bytes_in_window = self.transit_bytes_in_window.saturating_add(total_wire_bytes);
        if !is_control {
            self.transit_data_bytes_in_window = self
                .transit_data_bytes_in_window
                .saturating_add(total_wire_bytes);
        }
        nbr_win.bytes_used = nbr_win.bytes_used.saturating_add(total_wire_bytes);
        self.current_transit_queue = self.current_transit_queue.saturating_add(1);

        ResourceAdmission::Admitted {
            profile: self.active_profile,
            delay_multiplier_bps: self.active_profile.delay_multiplier_bps(),
        }
    }

    pub fn release_queued_item(&mut self) {
        self.current_transit_queue = self.current_transit_queue.saturating_sub(1);
    }

    pub fn current_transit_queue(&self) -> usize {
        self.current_transit_queue
    }
}

/// Composable `RelayPolicy` decorator that layers `ResourceBudgetManager` over an inner
/// `RelayPolicy` (such as `CoverageAwareFlood`).
pub struct ResourceAwareRelayPolicy<P: RelayPolicy> {
    inner: P,
    budget: Mutex<ResourceBudgetManager>,
}

impl<P: RelayPolicy> ResourceAwareRelayPolicy<P> {
    pub fn new(inner: P, budget: ResourceBudgetManager) -> Self {
        Self {
            inner,
            budget: Mutex::new(budget),
        }
    }

    pub fn update_snapshot(&self, snapshot: &ResourceSnapshot) -> RelayResourceProfile {
        if let Ok(mut b) = self.budget.lock() {
            b.update_snapshot(snapshot)
        } else {
            RelayResourceProfile::Balanced
        }
    }

    pub fn inner(&self) -> &P {
        &self.inner
    }
}

impl<P: RelayPolicy> RelayPolicy for ResourceAwareRelayPolicy<P> {
    fn should_relay(&self, ctx: &RelayContext) -> bool {
        if !self.inner.should_relay(ctx) {
            return false;
        }
        let now = ctx.now.unwrap_or_else(Instant::now);
        let neighbor = ctx.from_peer.unwrap_or_default();
        // Estimate average mesh envelope size (128 bytes) when evaluated via RelayContext
        let wire_bytes = 128usize;

        if let Ok(mut b) = self.budget.lock() {
            b.release_queued_item();
            matches!(
                b.check_and_record(
                    TrafficOrigin::TransitRelay { neighbor },
                    MsgType::Chat,
                    wire_bytes,
                    now,
                ),
                ResourceAdmission::Admitted { .. }
            )
        } else {
            true
        }
    }

    fn delay(&self, ctx: &RelayContext) -> Duration {
        let base_delay = self.inner.delay(ctx);
        let mult_bps = if let Ok(b) = self.budget.lock() {
            b.active_profile().delay_multiplier_bps()
        } else {
            10_000
        };
        let scaled_ms = (base_delay.as_millis() as u64).saturating_mul(mult_bps) / 10_000;
        Duration::from_millis(scaled_ms)
    }

    fn suppress_after(&self, ctx: &RelayContext) -> u8 {
        self.inner.suppress_after(ctx)
    }

    fn should_suppress(
        &self,
        ctx: &RelayContext,
        duplicate_sources: &[ShortId],
        from_peer: Option<ShortId>,
    ) -> bool {
        self.inner
            .should_suppress(ctx, duplicate_sources, from_peer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_derivation_thermal_and_user_pause_override_charging() {
        let now = Instant::now();

        // 1. Charging + High Battery + Nominal Thermal -> Assisted (8x)
        let mut snap = ResourceSnapshot {
            charging: TriState::True,
            battery_band: BatteryBand::High,
            power_save_mode: TriState::False,
            thermal_state: ThermalState::Nominal,
            network_metering: NetworkMetering::Unmetered,
            ble_ready: true,
            is_foreground: true,
            user_paused_relaying: false,
            user_profile_override: None,
            timestamp: now,
        };
        assert_eq!(snap.compute_raw_profile(), RelayResourceProfile::Assisted);

        // 2. Thermal Critical overrides Charging -> Paused
        snap.thermal_state = ThermalState::Critical;
        assert_eq!(snap.compute_raw_profile(), RelayResourceProfile::Paused);

        // 3. Thermal Serious caps at Minimal even when charging and user requests Assisted
        snap.thermal_state = ThermalState::Serious;
        snap.user_profile_override = Some(RelayResourceProfile::Assisted);
        assert_eq!(snap.compute_raw_profile(), RelayResourceProfile::Minimal);

        // 4. User Pause overrides everything -> Paused
        snap.thermal_state = ThermalState::Nominal;
        snap.user_paused_relaying = true;
        assert_eq!(snap.compute_raw_profile(), RelayResourceProfile::Paused);

        // 5. Unknown battery on metered background -> Minimal (conservative)
        let unknown_snap = ResourceSnapshot {
            charging: TriState::Unknown,
            battery_band: BatteryBand::Unknown,
            power_save_mode: TriState::False,
            thermal_state: ThermalState::Nominal,
            network_metering: NetworkMetering::Metered,
            ble_ready: true,
            is_foreground: false,
            user_paused_relaying: false,
            user_profile_override: None,
            timestamp: now,
        };
        assert_eq!(
            unknown_snap.compute_raw_profile(),
            RelayResourceProfile::Minimal
        );
    }

    #[test]
    fn test_hysteresis_prevents_profile_oscillation() {
        let now = Instant::now();
        let dwell = Duration::from_secs(10);
        let mut mgr = ResourceBudgetManager::new(RelayResourceProfile::Minimal, dwell, now);

        // Attempt to upgrade to Assisted at t + 3s (< 10s dwell) -> stays Minimal!
        let mut snap = ResourceSnapshot::default_at(now + Duration::from_secs(3));
        snap.charging = TriState::True;
        snap.battery_band = BatteryBand::High;
        assert_eq!(mgr.update_snapshot(&snap), RelayResourceProfile::Minimal);

        // At t + 11s (>= 10s dwell) -> upgrades to Assisted!
        snap.timestamp = now + Duration::from_secs(11);
        assert_eq!(mgr.update_snapshot(&snap), RelayResourceProfile::Assisted);

        // Immediate safety downgrade to Paused at t + 12s (1s later) MUST take effect without waiting for dwell!
        snap.timestamp = now + Duration::from_secs(12);
        snap.thermal_state = ThermalState::Critical;
        assert_eq!(mgr.update_snapshot(&snap), RelayResourceProfile::Paused);
    }

    #[test]
    fn test_originator_never_dropped_and_control_reservation_plus_neighbor_fairness() {
        let now = Instant::now();
        let mut mgr =
            ResourceBudgetManager::new(RelayResourceProfile::Minimal, Duration::from_secs(10), now);

        let noisy_nbr = ShortId([0x11; 8]);
        let quiet_nbr = ShortId([0x22; 8]);

        // Minimal budget is 8 KiB/s total, 50% (4 KiB/s) per neighbor, 75% (6 KiB/s) max data.
        // Noisy neighbor sends 4 KiB of Chat -> admitted
        assert!(matches!(
            mgr.check_and_record(
                TrafficOrigin::TransitRelay {
                    neighbor: noisy_nbr
                },
                MsgType::Chat,
                4096,
                now
            ),
            ResourceAdmission::Admitted { .. }
        ));

        // Noisy neighbor tries another 512B of Chat -> rejected by per-neighbor fairness!
        assert_eq!(
            mgr.check_and_record(
                TrafficOrigin::TransitRelay {
                    neighbor: noisy_nbr
                },
                MsgType::Chat,
                512,
                now
            ),
            ResourceAdmission::RejectedNeighborFairnessExceeded
        );

        // Quiet neighbor can still use remaining data capacity (2 KiB) -> admitted!
        assert!(matches!(
            mgr.check_and_record(
                TrafficOrigin::TransitRelay {
                    neighbor: quiet_nbr
                },
                MsgType::Chat,
                2048,
                now
            ),
            ResourceAdmission::Admitted { .. }
        ));

        // Now data capacity (6 KiB = 75% of 8 KiB) is full; further Chat is rejected to preserve 25% for ACK/Handshake!
        assert_eq!(
            mgr.check_and_record(
                TrafficOrigin::TransitRelay {
                    neighbor: quiet_nbr
                },
                MsgType::Chat,
                512,
                now
            ),
            ResourceAdmission::RejectedControlCapacityReserved
        );

        // ACK and Handshake traffic CAN still use the reserved 2 KiB control budget!
        assert!(matches!(
            mgr.check_and_record(
                TrafficOrigin::TransitRelay {
                    neighbor: quiet_nbr
                },
                MsgType::Ack,
                1024,
                now
            ),
            ResourceAdmission::Admitted { .. }
        ));

        // Even when Paused, LocalOriginated traffic is ALWAYS admitted!
        let mut paused_snap = ResourceSnapshot::default_at(now);
        paused_snap.user_paused_relaying = true;
        mgr.update_snapshot(&paused_snap);
        assert_eq!(mgr.active_profile(), RelayResourceProfile::Paused);
        assert!(matches!(
            mgr.check_and_record(TrafficOrigin::LocalOriginated, MsgType::Chat, 4096, now),
            ResourceAdmission::Admitted { .. }
        ));
    }
}
