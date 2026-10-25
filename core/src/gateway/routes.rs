use iroh::EndpointId;
use std::collections::HashMap;

use crate::identity::ShortId;

/// Manual static routing table mapping destination mesh ShortIds to remote gateway EndpointIds.
#[derive(Clone, Debug, Default)]
pub struct GatewayRouteTable {
    routes: HashMap<ShortId, EndpointId>,
}

impl GatewayRouteTable {
    pub fn new() -> Self {
        Self {
            routes: HashMap::new(),
        }
    }

    /// Add a route for recipient ShortId through a remote gateway endpoint.
    pub fn set_route(&mut self, recipient: ShortId, gateway: EndpointId) {
        self.routes.insert(recipient, gateway);
    }

    /// Remove route for recipient ShortId.
    pub fn remove_route(&mut self, recipient: &ShortId) -> Option<EndpointId> {
        self.routes.remove(recipient)
    }

    /// Lookup remote gateway endpoint for a recipient ShortId.
    pub fn lookup(&self, recipient: &ShortId) -> Option<&EndpointId> {
        self.routes.get(recipient)
    }

    /// Atomically replace all routes for a specific gateway endpoint from a verified RouteSnapshot.
    pub fn replace_gateway_snapshot(&mut self, gateway: EndpointId, recipients: Vec<ShortId>) {
        self.routes.retain(|_, gw| *gw != gateway);
        for recipient in recipients {
            self.routes.insert(recipient, gateway);
        }
    }

    /// Remove all routes associated with a disconnected gateway endpoint.
    pub fn remove_routes_for_endpoint(&mut self, gateway: &EndpointId) {
        self.routes.retain(|_, gw| gw != gateway);
    }

    /// List all configured routes.
    pub fn list_routes(&self) -> Vec<(ShortId, EndpointId)> {
        self.routes.iter().map(|(k, v)| (*k, *v)).collect()
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
}
