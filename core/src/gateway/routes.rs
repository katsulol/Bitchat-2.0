use std::collections::HashMap;
use std::time::{Duration, Instant};

use iroh::EndpointId;

use crate::identity::ShortId;

/// An individual route entry with lease expiry and sequence information.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteEntry {
    pub gateway: EndpointId,
    pub expires_at: Instant,
    pub epoch: u64,
    pub sequence: u64,
}

/// Dynamic authenticated routing table mapping destination mesh ShortIds to remote gateway EndpointIds.
#[derive(Clone, Debug, Default)]
pub struct GatewayRouteTable {
    routes: HashMap<ShortId, Vec<RouteEntry>>,
    gateway_state: HashMap<EndpointId, (u64, u64)>, // (epoch, last_sequence)
}

impl GatewayRouteTable {
    pub fn new() -> Self {
        Self {
            routes: HashMap::new(),
            gateway_state: HashMap::new(),
        }
    }

    /// Add a manual/static route for recipient ShortId through a remote gateway endpoint.
    pub fn set_route(&mut self, recipient: ShortId, gateway: EndpointId) {
        let entry = RouteEntry {
            gateway,
            expires_at: Instant::now() + Duration::from_secs(86400 * 365), // 1 year
            epoch: 1,
            sequence: 1,
        };
        let entries = self.routes.entry(recipient).or_default();
        entries.retain(|e| e.gateway != gateway);
        entries.push(entry);
    }

    /// Remove route for recipient ShortId.
    pub fn remove_route(&mut self, recipient: &ShortId) -> Option<EndpointId> {
        self.routes
            .remove(recipient)
            .and_then(|v| v.into_iter().next().map(|e| e.gateway))
    }

    /// Clear all active routes and gateway session records (atomic route withdrawal).
    pub fn clear(&mut self) {
        self.routes.clear();
        self.gateway_state.clear();
    }

    /// Lookup remote gateway endpoint for a recipient ShortId using current time.
    pub fn lookup(&self, recipient: &ShortId) -> Option<&EndpointId> {
        self.lookup_at(recipient, Instant::now())
    }

    /// Lookup remote gateway endpoint for a recipient ShortId at a specific timestamp.
    pub fn lookup_at(&self, recipient: &ShortId, now: Instant) -> Option<&EndpointId> {
        let entries = self.routes.get(recipient)?;
        entries
            .iter()
            .filter(|e| e.expires_at > now)
            .max_by_key(|e| e.sequence)
            .map(|e| &e.gateway)
    }

    /// Apply an authenticated RouteSnapshot atomically.
    ///
    /// Validates session epoch and strict sequence monotonicity.
    /// Replaces all routes for this gateway with the announced recipients and lease.
    /// An empty recipients list performs an atomic route withdrawal.
    pub fn apply_snapshot(
        &mut self,
        gateway: EndpointId,
        epoch: u64,
        sequence: u64,
        recipients: Vec<ShortId>,
        lease: Duration,
        now: Instant,
    ) -> Result<bool, &'static str> {
        if sequence == u64::MAX {
            return Err("Sequence number overflow: session must restart");
        }
        if let Some(&(last_epoch, last_seq)) = self.gateway_state.get(&gateway) {
            if epoch < last_epoch {
                return Err("Stale epoch rejected");
            }
            if epoch == last_epoch && sequence <= last_seq {
                return Err("Stale or duplicate sequence rejected");
            }
        }

        self.gateway_state.insert(gateway, (epoch, sequence));
        let expires_at = now + lease;

        // Atomic replace: remove existing routes for this gateway
        for entries in self.routes.values_mut() {
            entries.retain(|e| e.gateway != gateway);
        }

        // If recipients empty, this is an explicit atomic route withdrawal
        if recipients.is_empty() {
            self.cleanup_empty();
            return Ok(true);
        }

        for recipient in recipients {
            let entry = RouteEntry {
                gateway,
                expires_at,
                epoch,
                sequence,
            };
            self.routes.entry(recipient).or_default().push(entry);
        }

        self.cleanup_empty();
        Ok(true)
    }

    /// Helper for simple atomic replacement (defaults to 1-hour lease).
    pub fn replace_gateway_snapshot(&mut self, gateway: EndpointId, recipients: Vec<ShortId>) {
        let seq = self
            .gateway_state
            .get(&gateway)
            .map(|(_, s)| s + 1)
            .unwrap_or(1);
        let epoch = self
            .gateway_state
            .get(&gateway)
            .map(|(e, _)| *e)
            .unwrap_or(1);
        let _ = self.apply_snapshot(
            gateway,
            epoch,
            seq,
            recipients,
            Duration::from_secs(3600),
            Instant::now(),
        );
    }

    /// Remove all routes associated with a disconnected gateway endpoint.
    ///
    /// Preserves gateway_state (last epoch and sequence) to prevent replay attacks
    /// of prior-session RouteSnapshots upon reconnect.
    pub fn remove_routes_for_endpoint(&mut self, gateway: &EndpointId) {
        for entries in self.routes.values_mut() {
            entries.retain(|e| e.gateway != *gateway);
        }
        self.cleanup_empty();
    }

    fn cleanup_empty(&mut self) {
        self.routes.retain(|_, v| !v.is_empty());
    }

    /// List all configured active routes at current time.
    pub fn list_routes(&self) -> Vec<(ShortId, EndpointId)> {
        let now = Instant::now();
        self.routes
            .iter()
            .filter_map(|(k, v)| {
                v.iter()
                    .filter(|e| e.expires_at > now)
                    .max_by_key(|e| e.sequence)
                    .map(|e| (*k, e.gateway))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    #[test]
    fn test_route_table_operations() {
        let mut table = GatewayRouteTable::new();
        let target = ShortId([1, 2, 3, 4, 5, 6, 7, 8]);
        let gw = SecretKey::generate().public();

        assert!(table.lookup(&target).is_none());
        table.set_route(target, gw);
        assert_eq!(table.lookup(&target), Some(&gw));
        assert_eq!(table.remove_route(&target), Some(gw));
        assert!(table.lookup(&target).is_none());

        // Snapshot replacement test
        let t1 = ShortId([1, 1, 1, 1, 1, 1, 1, 1]);
        let t2 = ShortId([2, 2, 2, 2, 2, 2, 2, 2]);
        let t3 = ShortId([3, 3, 3, 3, 3, 3, 3, 3]);

        table.replace_gateway_snapshot(gw, vec![t1, t2]);
        assert_eq!(table.lookup(&t1), Some(&gw));
        assert_eq!(table.lookup(&t2), Some(&gw));
        assert!(table.lookup(&t3).is_none());

        table.replace_gateway_snapshot(gw, vec![t3]);
        assert!(table.lookup(&t1).is_none());
        assert_eq!(table.lookup(&t3), Some(&gw));

        table.remove_routes_for_endpoint(&gw);
        assert!(table.lookup(&t3).is_none());
    }

    #[test]
    fn test_snapshot_epoch_and_sequence_monotonicity() {
        let mut table = GatewayRouteTable::new();
        let gw = SecretKey::generate().public();
        let t1 = ShortId([1; 8]);
        let now = Instant::now();

        // Epoch 10, Seq 1 -> accepted
        let res = table.apply_snapshot(gw, 10, 1, vec![t1], Duration::from_secs(60), now);
        assert!(res.is_ok());
        assert_eq!(table.lookup_at(&t1, now), Some(&gw));

        // Epoch 10, Seq 1 duplicate -> rejected
        let res_dup = table.apply_snapshot(gw, 10, 1, vec![t1], Duration::from_secs(60), now);
        assert!(res_dup.is_err(), "Duplicate sequence must be rejected");

        // Epoch 10, Seq 0 stale -> rejected
        let res_stale = table.apply_snapshot(gw, 10, 0, vec![t1], Duration::from_secs(60), now);
        assert!(res_stale.is_err(), "Lower sequence must be rejected");

        // Old Epoch 9, Seq 500 -> rejected
        let res_old_epoch =
            table.apply_snapshot(gw, 9, 500, vec![t1], Duration::from_secs(60), now);
        assert!(res_old_epoch.is_err(), "Old epoch must be rejected");

        // New Epoch 11, Seq 1 -> accepted (reconnect scenario)
        let res_new_epoch = table.apply_snapshot(gw, 11, 1, vec![t1], Duration::from_secs(60), now);
        assert!(res_new_epoch.is_ok());
    }

    #[test]
    fn test_snapshot_lease_expiration() {
        let mut table = GatewayRouteTable::new();
        let gw = SecretKey::generate().public();
        let t1 = ShortId([1; 8]);
        let now = Instant::now();

        // 30-second lease
        table
            .apply_snapshot(gw, 1, 1, vec![t1], Duration::from_secs(30), now)
            .unwrap();

        // Active at 10s
        assert_eq!(
            table.lookup_at(&t1, now + Duration::from_secs(10)),
            Some(&gw)
        );

        // Expired at 31s
        assert!(table
            .lookup_at(&t1, now + Duration::from_secs(31))
            .is_none());
    }

    #[test]
    fn test_empty_snapshot_withdraws_routes() {
        let mut table = GatewayRouteTable::new();
        let gw = SecretKey::generate().public();
        let t1 = ShortId([1; 8]);
        let t2 = ShortId([2; 8]);
        let now = Instant::now();

        table
            .apply_snapshot(gw, 1, 1, vec![t1, t2], Duration::from_secs(60), now)
            .unwrap();
        assert_eq!(table.lookup_at(&t1, now), Some(&gw));
        assert_eq!(table.lookup_at(&t2, now), Some(&gw));

        // Empty snapshot withdraws all routes for gw
        table
            .apply_snapshot(gw, 1, 2, vec![], Duration::from_secs(60), now)
            .unwrap();
        assert!(table.lookup_at(&t1, now).is_none());
        assert!(table.lookup_at(&t2, now).is_none());
    }

    #[test]
    fn test_snapshot_reconnect_old_epoch_replay_rejected() {
        let mut table = GatewayRouteTable::new();
        let gw = SecretKey::generate().public();
        let t1 = ShortId([1; 8]);
        let now = Instant::now();

        // 1. Initial connection: Epoch 10, Seq 1 -> accepted
        let res = table.apply_snapshot(gw, 10, 1, vec![t1], Duration::from_secs(60), now);
        assert!(res.is_ok());
        assert_eq!(table.lookup_at(&t1, now), Some(&gw));

        // 2. Gateway disconnects: routes removed
        table.remove_routes_for_endpoint(&gw);
        assert!(table.lookup_at(&t1, now).is_none());

        // 3. Attacker captures and replays snapshot from previous connection (Epoch 10, Seq 1)
        let replay_res = table.apply_snapshot(gw, 10, 1, vec![t1], Duration::from_secs(60), now);
        assert_eq!(replay_res, Err("Stale or duplicate sequence rejected"));

        // 4. Attacker attempts to replay an older snapshot from Epoch 9
        let old_epoch_res = table.apply_snapshot(gw, 9, 50, vec![t1], Duration::from_secs(60), now);
        assert_eq!(old_epoch_res, Err("Stale epoch rejected"));

        // 5. Legitimate reconnect with strictly newer epoch (Epoch 11, Seq 1)
        let fresh_res = table.apply_snapshot(gw, 11, 1, vec![t1], Duration::from_secs(60), now);
        assert!(fresh_res.is_ok());
        assert_eq!(table.lookup_at(&t1, now), Some(&gw));
    }

    #[test]
    fn test_snapshot_sequence_overflow_rejected() {
        let mut table = GatewayRouteTable::new();
        let gw = SecretKey::generate().public();
        let t1 = ShortId([1; 8]);
        let now = Instant::now();

        // Sequence u64::MAX must be rejected to force session restart
        let res = table.apply_snapshot(gw, 1, u64::MAX, vec![t1], Duration::from_secs(60), now);
        assert_eq!(res, Err("Sequence number overflow: session must restart"));
    }
}
