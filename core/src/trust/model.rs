use std::collections::HashSet;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::identity::PeerId;

/// Scoped authorization capabilities in Bitchat 2.0.
///
/// Phase 5 replaces universal numeric trust scores with explicit, non-transitive
/// capabilities bound to full 32-byte `PeerId` identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
pub enum Capability {
    /// Allowed to send direct E2E messages beyond initial bounded request limits.
    ReceiveDirectMessage = 1,
    /// Locally approved to sign one-hop introductions (`SignedVoucher`) for other peers.
    ActAsIntroducer = 2,
    /// Allowed to use this node's internet gateway bridge to forward addressed packets.
    UseGatewayForForwarding = 3,
    /// Approved by the local user as a trusted outbound gateway next hop.
    PermitOutboundGateway = 4,
    /// Allowed to issue an introduction on behalf of the local node.
    IssueIntroduction = 5,
}

impl Capability {
    pub fn from_u8(val: u8) -> Result<Self> {
        match val {
            1 => Ok(Self::ReceiveDirectMessage),
            2 => Ok(Self::ActAsIntroducer),
            3 => Ok(Self::UseGatewayForForwarding),
            4 => Ok(Self::PermitOutboundGateway),
            5 => Ok(Self::IssueIntroduction),
            other => Err(Error::Crypto(format!(
                "Invalid Capability enum value: {other}"
            ))),
        }
    }

    /// Returns true if this capability is eligible to be granted via a one-hop voucher
    /// when permitted by local policy.
    ///
    /// Introducer delegation (`ActAsIntroducer`, `IssueIntroduction`) and outbound gateway
    /// selection (`PermitOutboundGateway`) are strictly local-consent-only and can never
    /// be delegated by a third-party voucher.
    pub fn is_delegable_by_voucher(self) -> bool {
        matches!(
            self,
            Self::ReceiveDirectMessage | Self::UseGatewayForForwarding
        )
    }
}

/// Active one-hop voucher grant cached in local trust state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveVoucherGrant {
    pub credential_id: [u8; 16],
    pub issuer: PeerId,
    pub subject: PeerId,
    pub audience: PeerId,
    pub capability: Capability,
    pub issued_at_unix: u64,
    pub expires_at_unix: u64,
    pub expires_at_monotonic: Instant,
    pub revocation_seq: u32,
    pub permanently_expired: bool,
}

/// Explicit local relationship state for a remote `PeerId`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PeerTrustState {
    /// Never seen or not yet cryptographically verified.
    #[default]
    Unrecognized,
    /// Cryptographic key ownership verified (Noise XX or signed LinkHello),
    /// awaiting explicit local user decision.
    IdentityVerifiedPendingConsent,
    /// Explicitly approved by the local user for a specific set of capabilities.
    Approved {
        capabilities: HashSet<Capability>,
        granted_at_unix: u64,
    },
    /// Previously approved or introduced, now revoked by the user or issuer.
    Revoked { revoked_at_unix: u64 },
    /// Explicitly blocked by the local user. Overrides all vouchers, invitations, and capabilities.
    Blocked { blocked_at_unix: u64 },
}

/// High-level authorization outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthOutcome {
    Allow,
    Deny,
    Pending,
}

/// Typed machine-readable reason code for an authorization decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthReason {
    LocalBlocked,
    LocalRevoked,
    ExplicitlyApproved,
    ValidOneHopVoucher,
    PendingUserConsent,
    DefaultMessageRequestAllowed,
    MessageRequestQuotaExceeded,
    CapabilityNotGranted,
    GatewayNotOptedIn,
    VoucherExpired,
    IssuerNotAuthorized,
    ClockValidityUnknown,
}

/// Pure authorization decision returned by `TrustPolicy::authorize`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationDecision {
    pub outcome: AuthOutcome,
    pub reason: AuthReason,
    pub expires_at: Option<Instant>,
}

/// Evaluation context supplied to `authorize(subject, capability, context)`.
#[derive(Debug, Clone)]
pub struct AuthorizationContext {
    pub now_monotonic: Instant,
    /// Current wall-clock UNIX timestamp in seconds, or `None` if system clock is unverified.
    pub now_unix: Option<u64>,
    /// Whether the local user has opted in to acting as a gateway.
    pub gateway_opted_in: bool,
    /// Whether local policy allows vouchers to grant `UseGatewayForForwarding`.
    pub allow_vouched_gateway_forwarding: bool,
}
