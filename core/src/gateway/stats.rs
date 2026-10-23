use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

/// Thread-safe counters and telemetry for internet gateway bridging.
#[derive(Debug, Default)]
pub struct GatewayStats {
    pub forwards_exported: AtomicU64,
    pub forwards_imported: AtomicU64,
    pub forwards_rejected: AtomicU64,
    pub loops_prevented: AtomicU64,
    pub active_connections: AtomicU64,
    pub last_error: RwLock<Option<String>>,
}

/// Snapshot of gateway metrics for UI display.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GatewayStatsSnapshot {
    pub forwards_exported: u64,
    pub forwards_imported: u64,
    pub forwards_rejected: u64,
    pub loops_prevented: u64,
    pub active_connections: u64,
    pub last_error: Option<String>,
}

impl GatewayStats {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn inc_exported(&self) {
        self.forwards_exported.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_imported(&self) {
        self.forwards_imported.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_rejected(&self) {
        self.forwards_rejected.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_loops_prevented(&self) {
        self.loops_prevented.fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_active_connections(&self, count: u64) {
        self.active_connections.store(count, Ordering::Relaxed);
    }

    pub fn record_error(&self, err: String) {
        if let Ok(mut lock) = self.last_error.write() {
            *lock = Some(err);
        }
    }

    pub fn snapshot(&self) -> GatewayStatsSnapshot {
        GatewayStatsSnapshot {
            forwards_exported: self.forwards_exported.load(Ordering::Relaxed),
            forwards_imported: self.forwards_imported.load(Ordering::Relaxed),
            forwards_rejected: self.forwards_rejected.load(Ordering::Relaxed),
            loops_prevented: self.loops_prevented.load(Ordering::Relaxed),
            active_connections: self.active_connections.load(Ordering::Relaxed),
            last_error: self.last_error.read().ok().and_then(|l| l.clone()),
        }
    }
}
