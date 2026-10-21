use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

// ponytail: lockless atomic counters for mesh packet stats + mutex-guarded latency samples.

/// Snapshot of mesh operation metrics for evaluation and monitoring.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MeshStatsSnapshot {
    pub transmissions: u64,
    pub duplicates_dropped: u64,
    pub relays_suppressed: u64,
    pub deliveries: u64,
    pub hop_count_exceeded: u64,
    pub latencies: Vec<Duration>,
}

/// Thread-safe counters tracking mesh routing efficiency and traffic volume.
#[derive(Debug, Default)]
pub struct MeshStats {
    transmissions: AtomicU64,
    duplicates_dropped: AtomicU64,
    relays_suppressed: AtomicU64,
    deliveries: AtomicU64,
    hop_count_exceeded: AtomicU64,
    latencies: Mutex<Vec<Duration>>,
}

impl MeshStats {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn inc_transmissions(&self) {
        self.transmissions.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_duplicates_dropped(&self) {
        self.duplicates_dropped.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_relays_suppressed(&self) {
        self.relays_suppressed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_deliveries(&self) {
        self.deliveries.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_hop_count_exceeded(&self) {
        self.hop_count_exceeded.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_latency(&self, latency: Duration) {
        if let Ok(mut lock) = self.latencies.lock() {
            lock.push(latency);
        }
    }

    pub fn snapshot(&self) -> MeshStatsSnapshot {
        let latencies = self.latencies.lock().map(|l| l.clone()).unwrap_or_default();

        MeshStatsSnapshot {
            transmissions: self.transmissions.load(Ordering::Relaxed),
            duplicates_dropped: self.duplicates_dropped.load(Ordering::Relaxed),
            relays_suppressed: self.relays_suppressed.load(Ordering::Relaxed),
            deliveries: self.deliveries.load(Ordering::Relaxed),
            hop_count_exceeded: self.hop_count_exceeded.load(Ordering::Relaxed),
            latencies,
        }
    }
}
