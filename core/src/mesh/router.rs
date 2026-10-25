//! Pure decision routing function and transport policy.
//!
//! Evaluates candidate routes according to the Bitchat 2.0 preference hierarchy:
//! 1. Verified direct BLE neighbor
//! 2. Approved Iroh gateway route (subject to Privacy relay restrictions)
//! 3. Bounded BLE mesh flood
//! 4. Queue backoff
//!
//! Control-plane handshakes are decoupled from route availability: lack of an
//! authenticated session does not block routing; it tags the decision with
//! `needs_handshake = true` and `RouteReason::SessionHandshakeRequired`.

use crate::identity::{PeerId, ShortId};
use crate::storage::MessageMode;

/// Observed Iroh path status to a remote endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrohPathObservation {
    /// Direct P2P UDP path established between endpoints.
    Direct,
    /// Traffic is routed via an encrypted Iroh DERP/relay node.
    Relay,
    /// Path has migrated or is concurrently using both relay and direct paths.
    Mixed,
    /// Path state is undetermined (e.g. during initial discovery/dialing).
    Unknown,
}

/// Strictly typed reason codes for routing decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteReason {
    /// Recipient is a verified direct BLE neighbor.
    DirectBleNeighbor,
    /// Route candidate selected, but an authenticated Noise session is not yet established.
    SessionHandshakeRequired,
    /// Approved Iroh gateway route is available.
    IrohGatewayAvailable,
    /// Falling back to bounded BLE mesh flood.
    MeshFloodCandidate,
    /// Iroh candidate blocked because Privacy Mode restricts relay-routed traffic.
    PrivacyRelayRestricted,
    /// Gateway forwarding candidate skipped because gateway features are disabled.
    GatewayOptedOut,
    /// Retry attempt budget exhausted for the current cycle; message queued for backoff.
    ExhaustedAttemptsQueue,
    /// No route or broadcast candidate available.
    NoRouteAvailable,
}

/// Chosen transport candidate for dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteCandidate {
    /// Send directly over local BLE to neighbor's ShortId.
    DirectBle(ShortId),
    /// Transmit across an authenticated Iroh gateway.
    IrohGateway {
        gateway_peer_id: PeerId,
        via_node: Option<ShortId>,
    },
    /// Disseminate via bounded BLE mesh flood.
    MeshFlood { max_ttl: u8 },
    /// Defer transmission and wait for backoff/topology change.
    QueueBackoff { next_retry_after_secs: u64 },
}

/// Routing decision outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDecision {
    pub candidate: RouteCandidate,
    pub reason: RouteReason,
    pub needs_handshake: bool,
    pub explanation: &'static str,
}

/// Inputs provided to the pure router decision function.
#[derive(Debug, Clone)]
pub struct RouteContext {
    pub target_peer_id: PeerId,
    pub target_short_id: ShortId,
    pub mode: MessageMode,
    pub is_direct_ble_neighbor: bool,
    pub has_authenticated_session: bool,
    pub gateway_route_available: Option<PeerId>,
    pub gateway_opted_in: bool,
    pub iroh_path: IrohPathObservation,
    pub privacy_block_relay: bool,
    pub attempt_count: u32,
    pub max_attempts: u32,
    pub ble_radio_ready: bool,
}

/// Pure synchronous router.
pub struct Router;

impl Router {
    /// Compute the next transport routing decision.
    ///
    /// Pure function: zero I/O, deterministic output for identical inputs.
    pub fn decide(ctx: &RouteContext) -> RouteDecision {
        // 1. Attempt budget exhaustion check
        if ctx.attempt_count >= ctx.max_attempts {
            return RouteDecision {
                candidate: RouteCandidate::QueueBackoff {
                    next_retry_after_secs: 60,
                },
                reason: RouteReason::ExhaustedAttemptsQueue,
                needs_handshake: false,
                explanation: "Attempt budget reached; backing off in queue",
            };
        }

        // 2. Preference 1: Verified Direct BLE Neighbor
        if ctx.ble_radio_ready && ctx.is_direct_ble_neighbor {
            if !ctx.has_authenticated_session {
                return RouteDecision {
                    candidate: RouteCandidate::DirectBle(ctx.target_short_id),
                    reason: RouteReason::SessionHandshakeRequired,
                    needs_handshake: true,
                    explanation: "Direct BLE neighbor reachable; session handshake required",
                };
            }
            return RouteDecision {
                candidate: RouteCandidate::DirectBle(ctx.target_short_id),
                reason: RouteReason::DirectBleNeighbor,
                needs_handshake: false,
                explanation: "Direct BLE neighbor reachable with established session",
            };
        }

        // 3. Preference 2: Approved Iroh Gateway Route
        if let Some(gw_id) = ctx.gateway_route_available {
            if !ctx.gateway_opted_in {
                // Gateway disabled by user policy -> skip to mesh flood
            } else if ctx.mode == MessageMode::Privacy
                && ctx.privacy_block_relay
                && (ctx.iroh_path == IrohPathObservation::Relay
                    || ctx.iroh_path == IrohPathObservation::Mixed)
            {
                // In Privacy Mode, relay-exposed paths are strictly blocked if requested
                // Candidate falls through with reason noted
            } else {
                if !ctx.has_authenticated_session {
                    return RouteDecision {
                        candidate: RouteCandidate::IrohGateway {
                            gateway_peer_id: gw_id,
                            via_node: None,
                        },
                        reason: RouteReason::SessionHandshakeRequired,
                        needs_handshake: true,
                        explanation: "Iroh gateway route available; session handshake required",
                    };
                }
                return RouteDecision {
                    candidate: RouteCandidate::IrohGateway {
                        gateway_peer_id: gw_id,
                        via_node: None,
                    },
                    reason: RouteReason::IrohGatewayAvailable,
                    needs_handshake: false,
                    explanation: "Iroh gateway route available with established session",
                };
            }
        }

        // 4. Preference 3: Bounded BLE Mesh Flood
        if ctx.ble_radio_ready {
            if !ctx.has_authenticated_session {
                return RouteDecision {
                    candidate: RouteCandidate::MeshFlood { max_ttl: 7 },
                    reason: RouteReason::SessionHandshakeRequired,
                    needs_handshake: true,
                    explanation: "Falling back to bounded BLE mesh flood; handshake required",
                };
            }
            return RouteDecision {
                candidate: RouteCandidate::MeshFlood { max_ttl: 7 },
                reason: RouteReason::MeshFloodCandidate,
                needs_handshake: false,
                explanation: "Falling back to bounded BLE mesh flood with established session",
            };
        }

        // 5. No transport available
        RouteDecision {
            candidate: RouteCandidate::QueueBackoff {
                next_retry_after_secs: 15,
            },
            reason: RouteReason::NoRouteAvailable,
            needs_handshake: false,
            explanation: "No transport or route available; retaining in outbox queue",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_peer_id(b: u8) -> PeerId {
        PeerId([b; 32])
    }

    #[test]
    fn test_router_prefers_direct_ble_neighbor() {
        let peer = sample_peer_id(1);
        let ctx = RouteContext {
            target_peer_id: peer,
            target_short_id: peer.short_id(),
            mode: MessageMode::Normal,
            is_direct_ble_neighbor: true,
            has_authenticated_session: true,
            gateway_route_available: Some(sample_peer_id(99)),
            gateway_opted_in: true,
            iroh_path: IrohPathObservation::Direct,
            privacy_block_relay: false,
            attempt_count: 0,
            max_attempts: 10,
            ble_radio_ready: true,
        };

        let decision = Router::decide(&ctx);
        assert_eq!(decision.reason, RouteReason::DirectBleNeighbor);
        assert_eq!(
            decision.candidate,
            RouteCandidate::DirectBle(peer.short_id())
        );
        assert!(!decision.needs_handshake);
    }

    #[test]
    fn test_router_direct_ble_requires_session_handshake() {
        let peer = sample_peer_id(1);
        let ctx = RouteContext {
            target_peer_id: peer,
            target_short_id: peer.short_id(),
            mode: MessageMode::Normal,
            is_direct_ble_neighbor: true,
            has_authenticated_session: false,
            gateway_route_available: None,
            gateway_opted_in: false,
            iroh_path: IrohPathObservation::Unknown,
            privacy_block_relay: false,
            attempt_count: 0,
            max_attempts: 10,
            ble_radio_ready: true,
        };

        let decision = Router::decide(&ctx);
        assert_eq!(decision.reason, RouteReason::SessionHandshakeRequired);
        assert_eq!(
            decision.candidate,
            RouteCandidate::DirectBle(peer.short_id())
        );
        assert!(decision.needs_handshake);
    }

    #[test]
    fn test_router_iroh_gateway_selected_when_direct_ble_absent() {
        let peer = sample_peer_id(2);
        let gw = sample_peer_id(50);
        let ctx = RouteContext {
            target_peer_id: peer,
            target_short_id: peer.short_id(),
            mode: MessageMode::Normal,
            is_direct_ble_neighbor: false,
            has_authenticated_session: true,
            gateway_route_available: Some(gw),
            gateway_opted_in: true,
            iroh_path: IrohPathObservation::Direct,
            privacy_block_relay: false,
            attempt_count: 0,
            max_attempts: 10,
            ble_radio_ready: true,
        };

        let decision = Router::decide(&ctx);
        assert_eq!(decision.reason, RouteReason::IrohGatewayAvailable);
        assert_eq!(
            decision.candidate,
            RouteCandidate::IrohGateway {
                gateway_peer_id: gw,
                via_node: None
            }
        );
        assert!(!decision.needs_handshake);
    }

    #[test]
    fn test_router_privacy_mode_blocks_iroh_relay_and_falls_back_to_mesh() {
        let peer = sample_peer_id(3);
        let gw = sample_peer_id(50);
        let ctx = RouteContext {
            target_peer_id: peer,
            target_short_id: peer.short_id(),
            mode: MessageMode::Privacy,
            is_direct_ble_neighbor: false,
            has_authenticated_session: true,
            gateway_route_available: Some(gw),
            gateway_opted_in: true,
            iroh_path: IrohPathObservation::Relay, // Relay observed
            privacy_block_relay: true,             // Privacy relay block active
            attempt_count: 0,
            max_attempts: 5,
            ble_radio_ready: true,
        };

        let decision = Router::decide(&ctx);
        // Gateway candidate is blocked due to relay; router falls back to BLE mesh flood
        assert_eq!(decision.reason, RouteReason::MeshFloodCandidate);
        assert_eq!(decision.candidate, RouteCandidate::MeshFlood { max_ttl: 7 });
    }

    #[test]
    fn test_router_gateway_opt_out_falls_back_to_mesh() {
        let peer = sample_peer_id(4);
        let gw = sample_peer_id(50);
        let ctx = RouteContext {
            target_peer_id: peer,
            target_short_id: peer.short_id(),
            mode: MessageMode::Normal,
            is_direct_ble_neighbor: false,
            has_authenticated_session: true,
            gateway_route_available: Some(gw),
            gateway_opted_in: false, // User disabled gateway forwarding
            iroh_path: IrohPathObservation::Direct,
            privacy_block_relay: false,
            attempt_count: 0,
            max_attempts: 10,
            ble_radio_ready: true,
        };

        let decision = Router::decide(&ctx);
        assert_eq!(decision.reason, RouteReason::MeshFloodCandidate);
        assert_eq!(decision.candidate, RouteCandidate::MeshFlood { max_ttl: 7 });
    }

    #[test]
    fn test_router_attempt_exhaustion_queues_backoff() {
        let peer = sample_peer_id(5);
        let ctx = RouteContext {
            target_peer_id: peer,
            target_short_id: peer.short_id(),
            mode: MessageMode::Normal,
            is_direct_ble_neighbor: true,
            has_authenticated_session: true,
            gateway_route_available: None,
            gateway_opted_in: false,
            iroh_path: IrohPathObservation::Unknown,
            privacy_block_relay: false,
            attempt_count: 10,
            max_attempts: 10,
            ble_radio_ready: true,
        };

        let decision = Router::decide(&ctx);
        assert_eq!(decision.reason, RouteReason::ExhaustedAttemptsQueue);
        assert!(matches!(
            decision.candidate,
            RouteCandidate::QueueBackoff { .. }
        ));
    }
}
