use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::identity::PeerId;
use crate::trust::model::{ActiveVoucherGrant, Capability, PeerTrustState};
use crate::trust::voucher::{
    SignedVoucher, SignedVoucherRevocation, MAX_IMPORT_REQUEST_BYTES, SIGNED_VOUCHER_WIRE_LEN,
};

pub const DEFAULT_MAX_APPROVED_INTRODUCERS: usize = 64;
pub const DEFAULT_MAX_ACTIVE_CREDENTIALS: usize = 1024;
pub const DEFAULT_MAX_VOUCHERS_PER_ISSUER_PER_HOUR: usize = 10;
pub const DEFAULT_MAX_PENDING_REQUESTS: usize = 32;
pub const DEFAULT_MAX_CACHED_REVOCATIONS: usize = 2048;

/// Configurable resource and admission bounds for the trust subsystem.
#[derive(Debug, Clone)]
pub struct TrustQuotaConfig {
    pub max_approved_introducers: usize,
    pub max_active_credentials: usize,
    pub max_import_bytes: usize,
    pub max_vouchers_per_issuer_per_hour: usize,
    pub max_pending_requests: usize,
    pub max_cached_revocations: usize,
}

impl Default for TrustQuotaConfig {
    fn default() -> Self {
        Self {
            max_approved_introducers: DEFAULT_MAX_APPROVED_INTRODUCERS,
            max_active_credentials: DEFAULT_MAX_ACTIVE_CREDENTIALS,
            max_import_bytes: MAX_IMPORT_REQUEST_BYTES,
            max_vouchers_per_issuer_per_hour: DEFAULT_MAX_VOUCHERS_PER_ISSUER_PER_HOUR,
            max_pending_requests: DEFAULT_MAX_PENDING_REQUESTS,
            max_cached_revocations: DEFAULT_MAX_CACHED_REVOCATIONS,
        }
    }
}

/// Bounded local trust and voucher store.
///
/// Enforces:
/// - Explicit local user consent (`Approved`, `Revoked`, `Blocked`)
/// - One-hop introducer verification (`ActAsIntroducer`)
/// - Dual monotonic + wall-clock credential expiry (preventing clock-rollback resurrection)
/// - Revocation caching (supporting out-of-order revocation-before-voucher arrival)
/// - Strict per-issuer and global quota caps
#[derive(Debug, Clone)]
pub struct TrustStore {
    local_peer_id: PeerId,
    quotas: TrustQuotaConfig,
    peer_states: HashMap<PeerId, PeerTrustState>,
    active_vouchers: HashMap<[u8; 16], ActiveVoucherGrant>,
    /// Maps `(issuer, credential_id)` -> `(highest_revocation_seq, recorded_at_monotonic)`.
    revoked_credentials: HashMap<(PeerId, [u8; 16]), (u32, Instant)>,
    /// Tracks `(window_start_monotonic, count_in_hour)` per issuer.
    issuer_hourly_rates: HashMap<PeerId, (Instant, usize)>,
}

impl TrustStore {
    pub fn new(local_peer_id: PeerId, quotas: TrustQuotaConfig) -> Self {
        Self {
            local_peer_id,
            quotas,
            peer_states: HashMap::new(),
            active_vouchers: HashMap::new(),
            revoked_credentials: HashMap::new(),
            issuer_hourly_rates: HashMap::new(),
        }
    }

    pub fn local_peer_id(&self) -> PeerId {
        self.local_peer_id
    }

    pub fn quotas(&self) -> &TrustQuotaConfig {
        &self.quotas
    }

    pub fn peer_state(&self, peer: &PeerId) -> PeerTrustState {
        self.peer_states.get(peer).cloned().unwrap_or_default()
    }

    pub fn approved_introducer_count(&self) -> usize {
        self.peer_states
            .values()
            .filter(|st| {
                matches!(
                    st,
                    PeerTrustState::Approved { capabilities, .. }
                        if capabilities.contains(&Capability::ActAsIntroducer)
                )
            })
            .count()
    }

    pub fn pending_request_count(&self) -> usize {
        self.peer_states
            .values()
            .filter(|st| matches!(st, PeerTrustState::IdentityVerifiedPendingConsent))
            .count()
    }

    pub fn active_credential_count(&self) -> usize {
        self.active_vouchers
            .values()
            .filter(|v| !v.permanently_expired)
            .count()
    }

    /// Record that a peer has proven cryptographic ownership of `peer` (e.g. via Noise XX).
    ///
    /// Transitions `Unrecognized -> IdentityVerifiedPendingConsent` within global pending bounds.
    /// Never overrides `Approved`, `Revoked`, or `Blocked`.
    pub fn record_identity_verified(&mut self, peer: PeerId) -> Result<PeerTrustState> {
        if peer == self.local_peer_id {
            return Err(Error::Crypto("Cannot record trust state for self".into()));
        }
        match self.peer_states.get(&peer) {
            Some(existing) => Ok(existing.clone()),
            None => {
                if self.pending_request_count() >= self.quotas.max_pending_requests {
                    return Err(Error::Storage(format!(
                        "Pending trust request quota ({}) exceeded",
                        self.quotas.max_pending_requests
                    )));
                }
                let st = PeerTrustState::IdentityVerifiedPendingConsent;
                self.peer_states.insert(peer, st.clone());
                Ok(st)
            }
        }
    }

    /// Explicitly approve a peer for the specified set of capabilities.
    pub fn approve_peer(
        &mut self,
        peer: PeerId,
        capabilities: HashSet<Capability>,
        now_unix: u64,
    ) -> Result<()> {
        if peer == self.local_peer_id {
            return Err(Error::Storage("Cannot approve self".into()));
        }
        let already_introducer = matches!(
            self.peer_states.get(&peer),
            Some(PeerTrustState::Approved { capabilities: existing, .. })
                if existing.contains(&Capability::ActAsIntroducer)
        );
        if capabilities.contains(&Capability::ActAsIntroducer)
            && !already_introducer
            && self.approved_introducer_count() >= self.quotas.max_approved_introducers
        {
            return Err(Error::Storage(format!(
                "Maximum approved introducers ({}) reached",
                self.quotas.max_approved_introducers
            )));
        }

        self.peer_states.insert(
            peer,
            PeerTrustState::Approved {
                capabilities,
                granted_at_unix: now_unix,
            },
        );
        Ok(())
    }

    /// Revoke local approval for `peer`.
    ///
    /// If `peer` was acting as an introducer, all active vouchers issued by `peer` are
    /// immediately invalidated.
    pub fn revoke_peer(&mut self, peer: PeerId, now_unix: u64) {
        self.peer_states.insert(
            peer,
            PeerTrustState::Revoked {
                revoked_at_unix: now_unix,
            },
        );
        self.active_vouchers.retain(|_, v| v.issuer != peer);
    }

    /// Explicitly block `peer`.
    ///
    /// Overrides all capabilities, invitations, and vouchers. Removes any active vouchers
    /// where `peer` is the subject or issuer, while never punishing an introducer if an
    /// introduced `subject` is blocked.
    pub fn block_peer(&mut self, peer: PeerId, now_unix: u64) {
        self.peer_states.insert(
            peer,
            PeerTrustState::Blocked {
                blocked_at_unix: now_unix,
            },
        );
        self.active_vouchers
            .retain(|_, v| v.subject != peer && v.issuer != peer);
    }

    /// Returns true if `issuer` is currently locally approved with `Capability::ActAsIntroducer`.
    pub fn is_approved_introducer(&self, issuer: &PeerId) -> bool {
        matches!(
            self.peer_states.get(issuer),
            Some(PeerTrustState::Approved { capabilities, .. })
                if capabilities.contains(&Capability::ActAsIntroducer)
        )
    }

    /// Import and verify a single `SignedVoucher`.
    pub fn import_voucher(
        &mut self,
        voucher: &SignedVoucher,
        now_monotonic: Instant,
        now_unix: Option<u64>,
    ) -> Result<()> {
        if voucher.subject == self.local_peer_id {
            return Err(Error::Crypto("Voucher subject cannot be local peer".into()));
        }
        if matches!(
            self.peer_states.get(&voucher.subject),
            Some(PeerTrustState::Blocked { .. })
        ) {
            return Err(Error::Crypto("Voucher subject is locally blocked".into()));
        }
        if !self.is_approved_introducer(&voucher.issuer) {
            return Err(Error::Crypto(format!(
                "Voucher issuer {:?} is not a locally approved introducer",
                voucher.issuer
            )));
        }

        // Check if this credential was already revoked (supports revocation-before-voucher)
        if let Some(&(rev_seq, _)) = self
            .revoked_credentials
            .get(&(voucher.issuer, voucher.credential_id))
        {
            if voucher.revocation_seq <= rev_seq {
                return Err(Error::Crypto(
                    "Voucher credential has already been revoked".into(),
                ));
            }
        }

        // Reject duplicate/replayed credential_id or older revocation_seq
        if let Some(existing) = self.active_vouchers.get(&voucher.credential_id) {
            if existing.permanently_expired || voucher.revocation_seq <= existing.revocation_seq {
                return Err(Error::Crypto(
                    "Duplicate or replayed voucher credential rejected".into(),
                ));
            }
        }

        // Verify signature, audience binding, capability eligibility, and wall-clock bounds
        voucher.verify(&self.local_peer_id, now_unix)?;
        let now_u64 = now_unix.ok_or_else(|| {
            Error::Crypto("Clock validity unknown: refusing voucher import".into())
        })?;

        // Enforce global active credential cap (pruning expired first)
        self.prune_expired_vouchers(now_monotonic, now_unix);
        if !self.active_vouchers.contains_key(&voucher.credential_id)
            && self.active_credential_count() >= self.quotas.max_active_credentials
        {
            return Err(Error::Storage(format!(
                "Global active voucher credential limit ({}) reached",
                self.quotas.max_active_credentials
            )));
        }

        // Enforce per-issuer hourly rate limit
        let max_hourly = self.quotas.max_vouchers_per_issuer_per_hour;
        let entry = self
            .issuer_hourly_rates
            .entry(voucher.issuer)
            .or_insert((now_monotonic, 0));
        if now_monotonic.saturating_duration_since(entry.0) >= Duration::from_secs(3600) {
            *entry = (now_monotonic, 0);
        }
        if entry.1 >= max_hourly {
            return Err(Error::Storage(format!(
                "Introducer {:?} exceeded hourly voucher quota ({})",
                voucher.issuer, max_hourly
            )));
        }
        entry.1 = entry.1.saturating_add(1);

        let lease_secs = voucher.expires_at_unix.saturating_sub(now_u64);
        let expires_at_monotonic = now_monotonic + Duration::from_secs(lease_secs);

        self.active_vouchers.insert(
            voucher.credential_id,
            ActiveVoucherGrant {
                credential_id: voucher.credential_id,
                issuer: voucher.issuer,
                subject: voucher.subject,
                audience: voucher.audience,
                capability: voucher.capability,
                issued_at_unix: voucher.issued_at_unix,
                expires_at_unix: voucher.expires_at_unix,
                expires_at_monotonic,
                revocation_seq: voucher.revocation_seq,
                permanently_expired: false,
            },
        );

        Ok(())
    }

    /// Import a wire-encoded batch of `SignedVoucher` records, enforcing `max_import_bytes` (16 KiB)
    /// before decoding or signature verification.
    pub fn import_voucher_batch_wire(
        &mut self,
        wire_batch: &[u8],
        now_monotonic: Instant,
        now_unix: Option<u64>,
    ) -> Result<usize> {
        if wire_batch.len() > self.quotas.max_import_bytes {
            return Err(Error::Storage(format!(
                "Voucher import batch ({} bytes) exceeds max_import_bytes ({})",
                wire_batch.len(),
                self.quotas.max_import_bytes
            )));
        }
        if !wire_batch.len().is_multiple_of(SIGNED_VOUCHER_WIRE_LEN) {
            return Err(Error::Crypto(format!(
                "Voucher batch length {} is not a multiple of {}",
                wire_batch.len(),
                SIGNED_VOUCHER_WIRE_LEN
            )));
        }

        let mut imported = 0;
        for chunk in wire_batch.as_chunks::<SIGNED_VOUCHER_WIRE_LEN>().0 {
            let voucher = SignedVoucher::from_wire_bytes(chunk)?;
            self.import_voucher(&voucher, now_monotonic, now_unix)?;
            imported += 1;
        }
        Ok(imported)
    }

    /// Process a signed voucher revocation from an introducer.
    ///
    /// Immediately removes any matching active voucher and caches the revocation so
    /// out-of-order or replayed vouchers with `revocation_seq <= rev.revocation_seq` are rejected.
    pub fn import_revocation(
        &mut self,
        rev: &SignedVoucherRevocation,
        now_monotonic: Instant,
    ) -> Result<()> {
        if matches!(
            self.peer_states.get(&rev.issuer),
            Some(PeerTrustState::Blocked { .. })
        ) {
            return Err(Error::Crypto("Revocation issuer is blocked".into()));
        }
        rev.verify(&self.local_peer_id)?;

        // Bound revocation cache size
        if self.revoked_credentials.len() >= self.quotas.max_cached_revocations {
            if let Some((oldest_key, _)) = self
                .revoked_credentials
                .iter()
                .min_by_key(|(_, (_, ts))| *ts)
                .map(|(k, v)| (*k, *v))
            {
                self.revoked_credentials.remove(&oldest_key);
            }
        }

        let key = (rev.issuer, rev.credential_id);
        let should_update = match self.revoked_credentials.get(&key) {
            Some(&(prev_seq, _)) => rev.revocation_seq >= prev_seq,
            None => true,
        };
        if should_update {
            self.revoked_credentials
                .insert(key, (rev.revocation_seq, now_monotonic));
        }

        if let Some(active) = self.active_vouchers.get(&rev.credential_id) {
            if active.issuer == rev.issuer && active.revocation_seq <= rev.revocation_seq {
                self.active_vouchers.remove(&rev.credential_id);
            }
        }

        Ok(())
    }

    /// Marks expired vouchers as permanently expired so wall-clock rollback cannot revive them.
    pub fn prune_expired_vouchers(&mut self, now_monotonic: Instant, now_unix: Option<u64>) {
        for grant in self.active_vouchers.values_mut() {
            if now_monotonic >= grant.expires_at_monotonic {
                grant.permanently_expired = true;
            }
            if let Some(u) = now_unix {
                if u >= grant.expires_at_unix {
                    grant.permanently_expired = true;
                }
            }
        }
    }

    /// Find a valid, non-expired one-hop voucher granting `capability` to `subject`.
    pub fn find_valid_voucher(
        &mut self,
        subject: &PeerId,
        capability: Capability,
        now_monotonic: Instant,
        now_unix: Option<u64>,
    ) -> Result<Option<ActiveVoucherGrant>> {
        self.prune_expired_vouchers(now_monotonic, now_unix);

        // If wall-clock validity is unknown, refuse automatic voucher elevation
        let Some(_now_u64) = now_unix else {
            return Err(Error::Crypto("Wall-clock validity unknown".into()));
        };

        let mut best: Option<ActiveVoucherGrant> = None;
        for grant in self.active_vouchers.values() {
            if &grant.subject != subject || grant.capability != capability {
                continue;
            }
            if grant.permanently_expired {
                continue;
            }
            if !self.is_approved_introducer(&grant.issuer) {
                continue;
            }
            match &best {
                Some(prev) if prev.expires_at_monotonic >= grant.expires_at_monotonic => {}
                _ => best = Some(grant.clone()),
            }
        }
        Ok(best)
    }

    /// Returns true if any voucher (even if now expired) exists for `(subject, capability)`.
    pub fn has_expired_voucher_for(&self, subject: &PeerId, capability: Capability) -> bool {
        self.active_vouchers
            .values()
            .any(|g| &g.subject == subject && g.capability == capability && g.permanently_expired)
    }
}
