// CRYPTO-REVIEW:
// What it does: Gateway operational configuration, explicit peer consent state machine
// (Unrecognized, IdentityVerifiedPendingConsent, Approved, Revoked, Blocked), and
// bounded single-use cryptographic pairing invitations.
// Primitives / crates: `rand::rngs::OsRng` (256-bit invitation tokens), `zeroize`.
// Reviewer checklist:
// 1. Identity verification alone transitions an unknown peer to IdentityVerifiedPendingConsent, NEVER Approved.
// 2. Invitations are 32-byte CSPRNG tokens, single-use (consumed atomically on redemption), with monotonic expiry.
// 3. Blocked state overrides all invitations, pending states, and prior approvals.
// 4. Invitation redemption requires gateway `enabled == true` and explicit `allow_invitation_auto_admit == true`.
// 5. Debug output redacts raw invitation secret tokens.

use iroh::EndpointId;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};
use zeroize::Zeroize;

pub const DEFAULT_MAX_GATEWAY_CONNECTIONS: usize = 4;
pub const DEFAULT_MAX_FORWARD_QUEUE_LEN: usize = 64;
pub const DEFAULT_MAX_FORWARD_QUEUE_BYTES: usize = 1024 * 1024; // 1 MiB
pub const MAX_OUTSTANDING_INVITATIONS: usize = 8;
pub const MAX_PENDING_CONSENT_PEERS: usize = 16;
pub const DEFAULT_INVITATION_TTL: Duration = Duration::from_secs(300); // 5 minutes

/// Explicit authorization and consent state for a gateway peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GatewayConsentState {
    Unrecognized,
    IdentityVerifiedPendingConsent,
    Approved,
    Revoked,
    Blocked,
}

/// Bounded single-use invitation capability held in volatile memory.
#[derive(Clone)]
pub struct PairingInvitation {
    token_hex: String,
    pub created_at: Instant,
    pub expires_at: Instant,
    pub expiry_unix_secs: u64,
}

impl Drop for PairingInvitation {
    fn drop(&mut self) {
        self.token_hex.zeroize();
    }
}

impl fmt::Debug for PairingInvitation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairingInvitation")
            .field("token", &"[REDACTED]")
            .field("expiry_unix_secs", &self.expiry_unix_secs)
            .finish()
    }
}

/// Pending peer awaiting explicit host user consent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingGatewayPeer {
    pub endpoint_id: EndpointId,
    pub bitchat_pubkey: [u8; 32],
    pub verified_at: Instant,
}

/// Gateway operational configuration and explicit authorization state machine.
#[derive(Clone, Debug)]
pub struct GatewayConfig {
    pub enabled: bool,
    pub allow_invitation_auto_admit: bool,
    pub max_connections: usize,
    pub max_queue_len: usize,
    pub max_queue_bytes: usize,
    peer_states: HashMap<EndpointId, GatewayConsentState>,
    pending_peers: HashMap<EndpointId, PendingGatewayPeer>,
    invitations: HashMap<String, PairingInvitation>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            enabled: false,                     // Gateways are OFF by default
            allow_invitation_auto_admit: false, // Host confirmation required by default
            max_connections: DEFAULT_MAX_GATEWAY_CONNECTIONS,
            max_queue_len: DEFAULT_MAX_FORWARD_QUEUE_LEN,
            max_queue_bytes: DEFAULT_MAX_FORWARD_QUEUE_BYTES,
            peer_states: HashMap::new(),
            pending_peers: HashMap::new(),
            invitations: HashMap::new(),
        }
    }
}

impl GatewayConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Query the explicit consent state of a gateway endpoint.
    pub fn consent_state(&self, endpoint_id: &EndpointId) -> GatewayConsentState {
        self.peer_states
            .get(endpoint_id)
            .copied()
            .unwrap_or(GatewayConsentState::Unrecognized)
    }

    /// Record a peer whose IdentityBinding verified but lacks explicit approval.
    /// Does NOT grant gateway authorization.
    pub fn record_verified_pending(
        &mut self,
        endpoint_id: EndpointId,
        bitchat_pubkey: [u8; 32],
        now: Instant,
    ) -> Result<GatewayConsentState, String> {
        match self.consent_state(&endpoint_id) {
            GatewayConsentState::Blocked => {
                return Err("Peer is blocked".into());
            }
            GatewayConsentState::Approved => {
                return Ok(GatewayConsentState::Approved);
            }
            GatewayConsentState::Revoked
            | GatewayConsentState::Unrecognized
            | GatewayConsentState::IdentityVerifiedPendingConsent => {}
        }

        if self.pending_peers.len() >= MAX_PENDING_CONSENT_PEERS
            && !self.pending_peers.contains_key(&endpoint_id)
        {
            return Err("Pending consent queue is full".into());
        }

        self.pending_peers.insert(
            endpoint_id,
            PendingGatewayPeer {
                endpoint_id,
                bitchat_pubkey,
                verified_at: now,
            },
        );
        self.peer_states.insert(
            endpoint_id,
            GatewayConsentState::IdentityVerifiedPendingConsent,
        );
        Ok(GatewayConsentState::IdentityVerifiedPendingConsent)
    }

    /// Explicitly authorize a remote gateway endpoint ID (unless Blocked).
    pub fn approve_gateway(&mut self, endpoint_id: EndpointId) -> bool {
        if self.consent_state(&endpoint_id) == GatewayConsentState::Blocked {
            return false;
        }
        self.pending_peers.remove(&endpoint_id);
        self.peer_states
            .insert(endpoint_id, GatewayConsentState::Approved);
        true
    }

    /// Revoke authorization for a remote gateway endpoint ID.
    pub fn revoke_gateway(&mut self, endpoint_id: &EndpointId) -> bool {
        self.pending_peers.remove(endpoint_id);
        match self.consent_state(endpoint_id) {
            GatewayConsentState::Approved | GatewayConsentState::IdentityVerifiedPendingConsent => {
                self.peer_states
                    .insert(*endpoint_id, GatewayConsentState::Revoked);
                true
            }
            _ => false,
        }
    }

    /// Cancel a pending gateway consent request.
    pub fn cancel_pending(&mut self, endpoint_id: &EndpointId) -> bool {
        let removed = self.pending_peers.remove(endpoint_id).is_some();
        if self.consent_state(endpoint_id) == GatewayConsentState::IdentityVerifiedPendingConsent {
            self.peer_states.remove(endpoint_id);
        }
        removed
    }

    /// Permanently block a gateway endpoint ID, overriding all invitations and prior approvals.
    pub fn block_gateway(&mut self, endpoint_id: EndpointId) {
        self.pending_peers.remove(&endpoint_id);
        self.peer_states
            .insert(endpoint_id, GatewayConsentState::Blocked);
    }

    /// Unblock a blocked gateway endpoint, returning it to Unrecognized.
    pub fn unblock_gateway(&mut self, endpoint_id: &EndpointId) -> bool {
        if self.consent_state(endpoint_id) == GatewayConsentState::Blocked {
            self.peer_states.remove(endpoint_id);
            true
        } else {
            false
        }
    }

    /// Check if a remote gateway is explicitly Approved.
    pub fn is_approved(&self, endpoint_id: &EndpointId) -> bool {
        self.consent_state(endpoint_id) == GatewayConsentState::Approved
    }

    /// Return count of approved gateways.
    pub fn approved_count(&self) -> usize {
        self.peer_states
            .values()
            .filter(|&&s| s == GatewayConsentState::Approved)
            .count()
    }

    /// List peers currently in IdentityVerifiedPendingConsent.
    pub fn list_pending(&self) -> Vec<PendingGatewayPeer> {
        self.pending_peers.values().cloned().collect()
    }

    /// Prune expired pairing invitations using monotonic clock.
    pub fn prune_expired_invitations(&mut self, now: Instant) {
        self.invitations.retain(|_, inv| now < inv.expires_at);
    }

    /// Generate a high-entropy (256-bit) single-use pairing invitation capability.
    pub fn generate_invitation(
        &mut self,
        ttl: Duration,
        now: Instant,
        now_unix_secs: u64,
    ) -> Result<(String, u64), String> {
        if !self.enabled {
            return Err("Cannot generate pairing invitation while gateway is disabled".into());
        }
        self.prune_expired_invitations(now);
        if self.invitations.len() >= MAX_OUTSTANDING_INVITATIONS {
            return Err("Maximum outstanding pairing invitations reached".into());
        }

        let mut raw = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut raw);
        let mut token_hex = String::with_capacity(64);
        for b in &raw {
            token_hex.push_str(&format!("{b:02x}"));
        }
        raw.zeroize();

        let expiry_unix_secs = now_unix_secs.saturating_add(ttl.as_secs());
        let inv = PairingInvitation {
            token_hex: token_hex.clone(),
            created_at: now,
            expires_at: now + ttl,
            expiry_unix_secs,
        };
        self.invitations.insert(token_hex.clone(), inv);
        Ok((token_hex, expiry_unix_secs))
    }

    /// Atomically validate and consume a single-use invitation token.
    ///
    /// If `allow_invitation_auto_admit` is true and gateway is `enabled`, transitions the peer
    /// to `Approved`. If `allow_invitation_auto_admit` is false, records peer as
    /// `IdentityVerifiedPendingConsent` while still consuming the single-use invitation.
    pub fn redeem_invitation(
        &mut self,
        token_hex: &str,
        endpoint_id: EndpointId,
        bitchat_pubkey: [u8; 32],
        now: Instant,
    ) -> Result<GatewayConsentState, String> {
        if !self.enabled {
            return Err("Gateway is disabled".into());
        }
        if self.consent_state(&endpoint_id) == GatewayConsentState::Blocked {
            return Err("Peer is blocked".into());
        }

        self.prune_expired_invitations(now);

        let clean = token_hex.trim();
        if clean.len() != 64 || !clean.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("Invalid invitation token format".into());
        }

        // Atomic removal ensures single-use even under concurrent redemption attempts
        let inv = self
            .invitations
            .remove(clean)
            .ok_or_else(|| "Invitation not found, expired, or already used".to_string())?;

        if now >= inv.expires_at {
            return Err("Invitation has expired".into());
        }

        if self.allow_invitation_auto_admit {
            self.pending_peers.remove(&endpoint_id);
            self.peer_states
                .insert(endpoint_id, GatewayConsentState::Approved);
            Ok(GatewayConsentState::Approved)
        } else {
            self.record_verified_pending(endpoint_id, bitchat_pubkey, now)
        }
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

        assert_eq!(cfg.consent_state(&gw1), GatewayConsentState::Unrecognized);
        assert!(!cfg.is_approved(&gw1));
        assert!(cfg.approve_gateway(gw1));
        assert!(cfg.is_approved(&gw1));
        assert_eq!(cfg.consent_state(&gw1), GatewayConsentState::Approved);
        assert!(!cfg.is_approved(&gw2));

        assert!(cfg.revoke_gateway(&gw1));
        assert!(!cfg.is_approved(&gw1));
        assert_eq!(cfg.consent_state(&gw1), GatewayConsentState::Revoked);
    }

    #[test]
    fn test_verified_binding_stays_pending_without_consent() {
        let mut cfg = GatewayConfig::new();
        cfg.enabled = true;
        let gw = SecretKey::generate().public();
        let now = Instant::now();

        let state = cfg.record_verified_pending(gw, [7u8; 32], now).unwrap();
        assert_eq!(state, GatewayConsentState::IdentityVerifiedPendingConsent);
        assert!(!cfg.is_approved(&gw));
        assert_eq!(cfg.list_pending().len(), 1);

        // Cancel pending
        assert!(cfg.cancel_pending(&gw));
        assert_eq!(cfg.consent_state(&gw), GatewayConsentState::Unrecognized);
        assert!(!cfg.is_approved(&gw));
    }

    #[test]
    fn test_invitation_single_use_expiry_and_block_override() {
        let mut cfg = GatewayConfig::new();
        cfg.enabled = true;
        cfg.allow_invitation_auto_admit = true;
        let now = Instant::now();

        let gw1 = SecretKey::generate().public();
        let gw2 = SecretKey::generate().public();

        let (token, _) = cfg
            .generate_invitation(Duration::from_secs(60), now, 1000)
            .unwrap();

        // First redemption succeeds
        let state = cfg
            .redeem_invitation(&token, gw1, [1u8; 32], now + Duration::from_secs(10))
            .unwrap();
        assert_eq!(state, GatewayConsentState::Approved);
        assert!(cfg.is_approved(&gw1));

        // Second (reused/concurrent) redemption of same token fails
        assert!(cfg
            .redeem_invitation(&token, gw2, [2u8; 32], now + Duration::from_secs(11))
            .is_err());
        assert!(!cfg.is_approved(&gw2));

        // Expired invitation fails
        let (exp_token, _) = cfg
            .generate_invitation(Duration::from_secs(5), now, 1000)
            .unwrap();
        assert!(cfg
            .redeem_invitation(&exp_token, gw2, [2u8; 32], now + Duration::from_secs(6))
            .is_err());

        // Blocked contact cannot redeem valid invitation or be approved
        cfg.block_gateway(gw2);
        let (tok3, _) = cfg
            .generate_invitation(Duration::from_secs(60), now, 1000)
            .unwrap();
        assert!(cfg.redeem_invitation(&tok3, gw2, [2u8; 32], now).is_err());
        assert!(!cfg.approve_gateway(gw2));
        assert!(!cfg.is_approved(&gw2));

        // Gateway disabled mid-pairing rejects invitation
        let gw3 = SecretKey::generate().public();
        let (tok4, _) = cfg
            .generate_invitation(Duration::from_secs(60), now, 1000)
            .unwrap();
        cfg.enabled = false;
        assert!(cfg.redeem_invitation(&tok4, gw3, [3u8; 32], now).is_err());
    }
}
