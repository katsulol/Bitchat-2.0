pub mod bridge;
pub mod config;
pub mod contact;
pub mod fairness;
pub mod identity_binding;
pub mod iroh_link;
pub mod routes;
pub mod stats;
pub mod wire;

pub use bridge::GatewayBridge;
pub use config::{GatewayConfig, GatewayConsentState, PendingGatewayPeer};
pub use contact::GatewayContact;
pub use fairness::{
    GatewayRateLimiter, QuotaRejection, CONTROL_PLANE_RESERVATION_RATIO,
    GLOBAL_BANDWIDTH_LIMIT_BPS, PEER_BANDWIDTH_LIMIT_BPS, PEER_ENVELOPE_LIMIT_PER_MIN,
};
pub use identity_binding::IdentityBinding;
pub use routes::GatewayRouteTable;
pub use stats::{GatewayStats, GatewayStatsSnapshot};
pub use wire::{GatewayForward, GatewayHello, GatewayMessage, RejectReason, WireError};
