//! Coarse local BLE proximity classification (`core/src/mesh/proximity.rs`).
//!
//! Privacy & accuracy invariants:
//! - Exposes only coarse categories (`Nearby`, `Further`, `Unknown`) — never distance in meters.
//! - Uses bounded in-memory per-link RSSI samples (median-smoothed, outlier-filtered) with
//!   minimum sample count, stale sample expiry, and 8 dB hysteresis deadband.
//! - Strictly direct-link only (`hop_count <= 1`): multi-hop relayed packets are ignored so a
//!   nearby relay never makes a distant sender appear `Nearby`.
//! - Binds link observations to a `PeerId` only after cryptographic verification.
//! - Zero disk persistence, zero Iroh export, and zero influence on trust or routing decisions.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::identity::PeerId;

pub const DEFAULT_MAX_SAMPLES_PER_LINK: usize = 8;
pub const DEFAULT_MIN_SAMPLES_REQUIRED: usize = 3;
pub const DEFAULT_SAMPLE_TTL: Duration = Duration::from_secs(15);
pub const DEFAULT_NEARBY_ENTER_DBM: i8 = -65;
pub const DEFAULT_NEARBY_EXIT_DBM: i8 = -73; // 8 dB hysteresis deadband
pub const MIN_PLAUSIBLE_RSSI_DBM: i8 = -110;
pub const MAX_PLAUSIBLE_RSSI_DBM: i8 = -20;
pub const MAX_TRACKED_LINKS: usize = 64;

/// Coarse local signal category for UI presentation only.
///
/// Does NOT represent physical distance in meters and carries zero authorization authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum ProximityCategory {
    #[default]
    Unknown = 0,
    Further = 1,
    Nearby = 2,
}

/// Calibration and memory bounds for the coarse proximity classifier.
#[derive(Debug, Clone)]
pub struct ProximityConfig {
    pub max_samples_per_link: usize,
    pub min_samples_required: usize,
    pub sample_ttl: Duration,
    pub nearby_enter_dbm: i8,
    pub nearby_exit_dbm: i8,
    pub max_tracked_links: usize,
}

impl Default for ProximityConfig {
    fn default() -> Self {
        Self {
            max_samples_per_link: DEFAULT_MAX_SAMPLES_PER_LINK,
            min_samples_required: DEFAULT_MIN_SAMPLES_REQUIRED,
            sample_ttl: DEFAULT_SAMPLE_TTL,
            nearby_enter_dbm: DEFAULT_NEARBY_ENTER_DBM,
            nearby_exit_dbm: DEFAULT_NEARBY_EXIT_DBM,
            max_tracked_links: MAX_TRACKED_LINKS,
        }
    }
}

#[derive(Debug, Clone)]
struct LinkRssiState {
    samples: VecDeque<(i8, Instant)>,
    current_category: ProximityCategory,
    verified_peer: Option<PeerId>,
    last_updated: Instant,
}

/// Bounded in-memory coarse proximity tracker.
#[derive(Debug, Clone)]
pub struct ProximityTracker {
    config: ProximityConfig,
    links: HashMap<String, LinkRssiState>,
}

impl Default for ProximityTracker {
    fn default() -> Self {
        Self::new(ProximityConfig::default())
    }
}

impl ProximityTracker {
    pub fn new(config: ProximityConfig) -> Self {
        Self {
            config,
            links: HashMap::new(),
        }
    }

    /// Record a direct-link BLE RSSI sample.
    ///
    /// - Multi-hop observations (`hop_count > 1`) are ignored so a nearby relay cannot make a
    ///   remote origin peer appear `Nearby`.
    /// - Implausible hardware outliers outside `[-110, -20]` dBm are ignored.
    pub fn record_direct_link_rssi(
        &mut self,
        link_id: &str,
        rssi_dbm: i8,
        hop_count: u8,
        now: Instant,
    ) {
        if hop_count > 1 {
            return;
        }
        if !(MIN_PLAUSIBLE_RSSI_DBM..=MAX_PLAUSIBLE_RSSI_DBM).contains(&rssi_dbm) {
            return;
        }

        if !self.links.contains_key(link_id) && self.links.len() >= self.config.max_tracked_links {
            if let Some(oldest_key) = self
                .links
                .iter()
                .min_by_key(|(_, st)| st.last_updated)
                .map(|(k, _)| k.clone())
            {
                self.links.remove(&oldest_key);
            }
        }

        let state = self
            .links
            .entry(link_id.to_string())
            .or_insert_with(|| LinkRssiState {
                samples: VecDeque::with_capacity(self.config.max_samples_per_link),
                current_category: ProximityCategory::Unknown,
                verified_peer: None,
                last_updated: now,
            });

        state.last_updated = now;
        Self::prune_link_samples(state, self.config.sample_ttl, now);

        if state.samples.len() >= self.config.max_samples_per_link {
            state.samples.pop_front();
        }
        state.samples.push_back((rssi_dbm, now));
        Self::recompute_link_category(state, &self.config);
    }

    /// Associate a direct link with a cryptographically verified `PeerId`.
    pub fn bind_verified_peer(&mut self, link_id: &str, peer_id: PeerId, now: Instant) {
        if !self.links.contains_key(link_id) && self.links.len() >= self.config.max_tracked_links {
            return;
        }
        let state = self
            .links
            .entry(link_id.to_string())
            .or_insert_with(|| LinkRssiState {
                samples: VecDeque::with_capacity(self.config.max_samples_per_link),
                current_category: ProximityCategory::Unknown,
                verified_peer: Some(peer_id),
                last_updated: now,
            });
        state.verified_peer = Some(peer_id);
        state.last_updated = now;
    }

    /// Discard all RSSI samples and binding for a disconnected link immediately.
    pub fn on_link_disconnected(&mut self, link_id: &str) {
        self.links.remove(link_id);
    }

    /// Query the coarse proximity category for a verified `PeerId` across its active direct links.
    ///
    /// Unverified links never contribute to any peer's proximity category.
    pub fn peer_proximity(&mut self, peer_id: &PeerId, now: Instant) -> ProximityCategory {
        let ttl = self.config.sample_ttl;
        let cfg = self.config.clone();
        let mut best = ProximityCategory::Unknown;

        for state in self.links.values_mut() {
            if state.verified_peer.as_ref() != Some(peer_id) {
                continue;
            }
            Self::prune_link_samples(state, ttl, now);
            Self::recompute_link_category(state, &cfg);
            if state.current_category > best {
                best = state.current_category;
            }
        }
        best
    }

    pub fn tracked_link_count(&self) -> usize {
        self.links.len()
    }

    fn prune_link_samples(state: &mut LinkRssiState, ttl: Duration, now: Instant) {
        state
            .samples
            .retain(|&(_, ts)| now.saturating_duration_since(ts) <= ttl);
    }

    fn recompute_link_category(state: &mut LinkRssiState, cfg: &ProximityConfig) {
        if state.samples.len() < cfg.min_samples_required {
            state.current_category = ProximityCategory::Unknown;
            return;
        }

        // Compute median RSSI for robust outlier resistance
        let mut sorted: Vec<i8> = state.samples.iter().map(|&(r, _)| r).collect();
        sorted.sort_unstable();
        let median_dbm = sorted[sorted.len() / 2];

        state.current_category = match state.current_category {
            ProximityCategory::Nearby => {
                // Stay Nearby unless median drops at or below nearby_exit_dbm (hysteresis)
                if median_dbm <= cfg.nearby_exit_dbm {
                    ProximityCategory::Further
                } else {
                    ProximityCategory::Nearby
                }
            }
            ProximityCategory::Further | ProximityCategory::Unknown => {
                if median_dbm >= cfg.nearby_enter_dbm {
                    ProximityCategory::Nearby
                } else {
                    ProximityCategory::Further
                }
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    #[test]
    fn test_proximity_smoothing_hysteresis_stale_expiry_and_relay_confusion() {
        let mut tracker = ProximityTracker::default();
        let peer_a = Identity::generate().peer_id();
        let now = Instant::now();

        // 1. Unverified link observations never show up for peer_a
        tracker.record_direct_link_rssi("link-1", -55, 1, now);
        tracker.record_direct_link_rssi("link-1", -56, 1, now);
        tracker.record_direct_link_rssi("link-1", -54, 1, now);
        assert_eq!(
            tracker.peer_proximity(&peer_a, now),
            ProximityCategory::Unknown
        );

        // Bind link-1 to verified peer_a -> now Nearby!
        tracker.bind_verified_peer("link-1", peer_a, now);
        assert_eq!(
            tracker.peer_proximity(&peer_a, now),
            ProximityCategory::Nearby
        );

        // 2. Single extreme outlier (-105 dBm) or implausible value (0 dBm) does not flip median
        tracker.record_direct_link_rssi("link-1", 0, 1, now); // ignored (> -20 dBm)
        tracker.record_direct_link_rssi("link-1", -105, 1, now);
        assert_eq!(
            tracker.peer_proximity(&peer_a, now),
            ProximityCategory::Nearby
        );

        // 3. Hysteresis: dropping to -69 dBm (between -65 enter and -73 exit) stays Nearby!
        for _ in 0..5 {
            tracker.record_direct_link_rssi("link-1", -69, 1, now + Duration::from_secs(1));
        }
        assert_eq!(
            tracker.peer_proximity(&peer_a, now + Duration::from_secs(1)),
            ProximityCategory::Nearby
        );

        // Dropping to -78 dBm (<= -73 exit) transitions to Further!
        for _ in 0..5 {
            tracker.record_direct_link_rssi("link-1", -78, 1, now + Duration::from_secs(2));
        }
        assert_eq!(
            tracker.peer_proximity(&peer_a, now + Duration::from_secs(2)),
            ProximityCategory::Further
        );

        // 4. Multi-hop relayed packets (hop_count = 2) with strong RSSI (-45 dBm) are ignored!
        for _ in 0..5 {
            tracker.record_direct_link_rssi("link-1", -45, 2, now + Duration::from_secs(3));
        }
        assert_eq!(
            tracker.peer_proximity(&peer_a, now + Duration::from_secs(3)),
            ProximityCategory::Further
        );

        // 5. Stale expiry (> 15s) resets category to Unknown
        assert_eq!(
            tracker.peer_proximity(&peer_a, now + Duration::from_secs(25)),
            ProximityCategory::Unknown
        );

        // 6. Disconnect immediately discards link state
        tracker.on_link_disconnected("link-1");
        assert_eq!(tracker.tracked_link_count(), 0);
    }

    #[test]
    fn test_two_links_per_peer_and_memory_bounds() {
        let mut tracker = ProximityTracker::default();
        let peer = Identity::generate().peer_id();
        let now = Instant::now();

        tracker.bind_verified_peer("central-link", peer, now);
        tracker.bind_verified_peer("peripheral-link", peer, now);

        for _ in 0..3 {
            tracker.record_direct_link_rssi("central-link", -82, 1, now);
            tracker.record_direct_link_rssi("peripheral-link", -58, 1, now);
        }
        assert_eq!(
            tracker.peer_proximity(&peer, now),
            ProximityCategory::Nearby
        );

        // Disconnect stronger link -> falls back to remaining active link (Further)
        tracker.on_link_disconnected("peripheral-link");
        assert_eq!(
            tracker.peer_proximity(&peer, now),
            ProximityCategory::Further
        );

        // Flood 200 distinct links -> bounded at MAX_TRACKED_LINKS (64)
        for i in 0..200 {
            tracker.record_direct_link_rssi(&format!("flood-{i}"), -60, 1, now);
        }
        assert!(tracker.tracked_link_count() <= MAX_TRACKED_LINKS);
    }
}
