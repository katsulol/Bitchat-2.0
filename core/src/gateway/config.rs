use iroh::EndpointId;
use std::collections::HashSet;

pub const DEFAULT_MAX_GATEWAY_CONNECTIONS: usize = 4;
pub const DEFAULT_MAX_FORWARD_QUEUE_LEN: usize = 64;
pub const DEFAULT_MAX_FORWARD_QUEUE_BYTES: usize = 1024 * 1024; // 1 MiB

/// Gateway operational configuration and manual authorization whitelist.
#[derive(Clone, Debug)]
pub struct GatewayConfig {
    pub enabled: bool,
    pub max_connections: usize,
    pub max_queue_len: usize,
    pub max_queue_bytes: usize,
    approved_gateways: HashSet<EndpointId>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_connections: DEFAULT_MAX_GATEWAY_CONNECTIONS,
            max_queue_len: DEFAULT_MAX_FORWARD_QUEUE_LEN,
            max_queue_bytes: DEFAULT_MAX_FORWARD_QUEUE_BYTES,
            approved_gateways: HashSet::new(),
        }
    }
}

impl GatewayConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Authorize a remote gateway endpoint ID.
    pub fn approve_gateway(&mut self, endpoint_id: EndpointId) {
        self.approved_gateways.insert(endpoint_id);
    }

    /// Revoke authorization for a remote gateway endpoint ID.
    pub fn revoke_gateway(&mut self, endpoint_id: &EndpointId) -> bool {
        self.approved_gateways.remove(endpoint_id)
    }

    /// Check if a remote gateway is authorized.
    pub fn is_approved(&self, endpoint_id: &EndpointId) -> bool {
        self.approved_gateways.contains(endpoint_id)
    }

    /// Return count of approved gateways.
    pub fn approved_count(&self) -> usize {
        self.approved_gateways.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    #[test]
    fn test_gateway_authorization() {
        let mut cfg = GatewayConfig::new();
        let gw1 = SecretKey::generate().public();
        let gw2 = SecretKey::generate().public();

        assert!(!cfg.is_approved(&gw1));
        cfg.approve_gateway(gw1);
        assert!(cfg.is_approved(&gw1));
        assert!(!cfg.is_approved(&gw2));

        assert!(cfg.revoke_gateway(&gw1));
        assert!(!cfg.is_approved(&gw1));
    }
}
