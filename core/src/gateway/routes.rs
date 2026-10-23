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
    }
}
