use crate::identity::PeerId;
use crate::trust::model::{
    AuthOutcome, AuthReason, AuthorizationContext, AuthorizationDecision, Capability,
    PeerTrustState,
};
use crate::trust::store::TrustStore;

/// Pure authorization decision engine evaluating local trust state and one-hop vouchers.
pub struct TrustPolicy;

impl TrustPolicy {
    /// Evaluate whether `subject` is authorized for `capability` under `ctx`.
    ///
    /// Order of precedence:
    /// 1. Local `Blocked` overrides all capabilities, invitations, and vouchers (`Deny(LocalBlocked)`).
    /// 2. Gateway forwarding capability requires local `gateway_opted_in == true`.
    /// 3. Explicit local `Approved` grants capabilities present in the peer's approved set.
    /// 4. Valid one-hop `SignedVoucher` from an approved introducer grants `ReceiveDirectMessage`
    ///    (or `UseGatewayForForwarding` when `ctx.allow_vouched_gateway_forwarding` is true).
    ///    If `ctx.now_unix` is `None`, automatic voucher elevation is refused (`ClockValidityUnknown`).
    /// 5. Local `Revoked` denies unapproved access (`Deny(LocalRevoked)`).
    /// 6. For `ReceiveDirectMessage`, unrecognized or pending-consent peers are allowed as a bounded
    ///    message request (`Pending(DefaultMessageRequestAllowed)` or `Pending(PendingUserConsent)`)
    ///    within `max_pending_requests`.
    pub fn authorize(
        store: &mut TrustStore,
        subject: &PeerId,
        capability: Capability,
        ctx: &AuthorizationContext,
    ) -> AuthorizationDecision {
        let state = store.peer_state(subject);

        // 1. Local Blocked overrides every other decision
        if matches!(state, PeerTrustState::Blocked { .. }) {
            return AuthorizationDecision {
                outcome: AuthOutcome::Deny,
                reason: AuthReason::LocalBlocked,
                expires_at: None,
            };
        }

        // 2. Gateway forwarding requires local gateway opt-in
        if capability == Capability::UseGatewayForForwarding && !ctx.gateway_opted_in {
            return AuthorizationDecision {
                outcome: AuthOutcome::Deny,
                reason: AuthReason::GatewayNotOptedIn,
                expires_at: None,
            };
        }

        // 3. Explicit local user approval
        if let PeerTrustState::Approved { capabilities, .. } = &state {
            if capabilities.contains(&capability) {
                return AuthorizationDecision {
                    outcome: AuthOutcome::Allow,
                    reason: AuthReason::ExplicitlyApproved,
                    expires_at: None,
                };
            }
        }

        // 4. One-hop voucher evaluation (for delegable capabilities permitted by local policy)
        let voucher_eligible = match capability {
            Capability::ReceiveDirectMessage => true,
            Capability::UseGatewayForForwarding => ctx.allow_vouched_gateway_forwarding,
            Capability::ActAsIntroducer
            | Capability::PermitOutboundGateway
            | Capability::IssueIntroduction => false,
        };

        if voucher_eligible {
            match store.find_valid_voucher(subject, capability, ctx.now_monotonic, ctx.now_unix) {
                Ok(Some(grant)) => {
                    return AuthorizationDecision {
                        outcome: AuthOutcome::Allow,
                        reason: AuthReason::ValidOneHopVoucher,
                        expires_at: Some(grant.expires_at_monotonic),
                    };
                }
                Err(_) => {
                    // Check if a voucher exists for this subject/capability when clock is unknown
                    store.prune_expired_vouchers(ctx.now_monotonic, None);
                    return AuthorizationDecision {
                        outcome: AuthOutcome::Deny,
                        reason: AuthReason::ClockValidityUnknown,
                        expires_at: None,
                    };
                }
                Ok(None) => {
                    if store.has_expired_voucher_for(subject, capability) {
                        return AuthorizationDecision {
                            outcome: AuthOutcome::Deny,
                            reason: AuthReason::VoucherExpired,
                            expires_at: None,
                        };
                    }
                }
            }
        }

        // 5. Local Revoked denies elevated capabilities
        if matches!(state, PeerTrustState::Revoked { .. }) {
            return AuthorizationDecision {
                outcome: AuthOutcome::Deny,
                reason: AuthReason::LocalRevoked,
                expires_at: None,
            };
        }

        // 6. Default bounded message request behavior for new/pending peers
        if capability == Capability::ReceiveDirectMessage {
            return match state {
                PeerTrustState::IdentityVerifiedPendingConsent => AuthorizationDecision {
                    outcome: AuthOutcome::Pending,
                    reason: AuthReason::PendingUserConsent,
                    expires_at: None,
                },
                PeerTrustState::Unrecognized => {
                    if store.pending_request_count() < store.quotas().max_pending_requests {
                        AuthorizationDecision {
                            outcome: AuthOutcome::Pending,
                            reason: AuthReason::DefaultMessageRequestAllowed,
                            expires_at: None,
                        }
                    } else {
                        AuthorizationDecision {
                            outcome: AuthOutcome::Deny,
                            reason: AuthReason::MessageRequestQuotaExceeded,
                            expires_at: None,
                        }
                    }
                }
                _ => AuthorizationDecision {
                    outcome: AuthOutcome::Deny,
                    reason: AuthReason::CapabilityNotGranted,
                    expires_at: None,
                },
            };
        }

        if matches!(state, PeerTrustState::IdentityVerifiedPendingConsent) {
            return AuthorizationDecision {
                outcome: AuthOutcome::Pending,
                reason: AuthReason::PendingUserConsent,
                expires_at: None,
            };
        }

        AuthorizationDecision {
            outcome: AuthOutcome::Deny,
            reason: AuthReason::CapabilityNotGranted,
            expires_at: None,
        }
    }
}
