//! Scoped peer authorization and bounded one-hop Ed25519 voucher subsystem.

pub mod model;
pub mod policy;
pub mod store;
pub mod voucher;

pub use model::{
    ActiveVoucherGrant, AuthOutcome, AuthReason, AuthorizationContext, AuthorizationDecision,
    Capability, PeerTrustState,
};
pub use policy::TrustPolicy;
pub use store::{TrustQuotaConfig, TrustStore};
pub use voucher::{
    SignedVoucher, SignedVoucherRevocation, MAX_FUTURE_ISSUED_SKEW_SECS, MAX_IMPORT_REQUEST_BYTES,
    MAX_VOUCHER_LIFETIME_SECS, SIGNED_REVOCATION_WIRE_LEN, SIGNED_VOUCHER_WIRE_LEN, VOUCH_DOMAIN,
    VOUCH_REVOKE_DOMAIN, VOUCH_VERSION,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    fn default_ctx(now: Instant, now_unix: Option<u64>) -> AuthorizationContext {
        AuthorizationContext {
            now_monotonic: now,
            now_unix,
            gateway_opted_in: true,
            allow_vouched_gateway_forwarding: false,
        }
    }

    #[test]
    fn test_local_blocked_overrides_all_capabilities_and_vouchers() {
        let local = Identity::generate();
        let introducer = Identity::generate();
        let subject = Identity::generate();
        let now = Instant::now();
        let now_unix = 1_700_000_000;

        let mut store = TrustStore::new(local.peer_id(), TrustQuotaConfig::default());

        // Approve introducer
        let mut intro_caps = HashSet::new();
        intro_caps.insert(Capability::ActAsIntroducer);
        store
            .approve_peer(introducer.peer_id(), intro_caps, now_unix)
            .unwrap();

        // Issue valid voucher for subject
        let voucher = SignedVoucher::issue(
            &introducer,
            subject.peer_id(),
            local.peer_id(),
            Capability::ReceiveDirectMessage,
            now_unix,
            now_unix + 3600,
            1,
        )
        .unwrap();
        store
            .import_voucher(&voucher, now, Some(now_unix))
            .expect("Valid voucher import");

        // Subject is allowed via voucher initially
        let dec1 = TrustPolicy::authorize(
            &mut store,
            &subject.peer_id(),
            Capability::ReceiveDirectMessage,
            &default_ctx(now, Some(now_unix)),
        );
        assert_eq!(dec1.outcome, AuthOutcome::Allow);
        assert_eq!(dec1.reason, AuthReason::ValidOneHopVoucher);

        // Local user blocks subject -> overrides voucher and denies all capabilities
        store.block_peer(subject.peer_id(), now_unix + 10);
        let dec2 = TrustPolicy::authorize(
            &mut store,
            &subject.peer_id(),
            Capability::ReceiveDirectMessage,
            &default_ctx(now, Some(now_unix + 10)),
        );
        assert_eq!(dec2.outcome, AuthOutcome::Deny);
        assert_eq!(dec2.reason, AuthReason::LocalBlocked);

        // Re-importing the voucher while blocked must fail
        assert!(store
            .import_voucher(&voucher, now, Some(now_unix + 10))
            .is_err());

        // Blocking the introduced subject must NOT punish or revoke the introducer
        assert!(store.is_approved_introducer(&introducer.peer_id()));
    }

    #[test]
    fn test_voucher_mutation_of_every_field_and_wrong_audience_rejected() {
        let local = Identity::generate();
        let other_audience = Identity::generate();
        let introducer = Identity::generate();
        let subject = Identity::generate();
        let now_unix = 1_700_000_000;

        let voucher = SignedVoucher::issue(
            &introducer,
            subject.peer_id(),
            local.peer_id(),
            Capability::ReceiveDirectMessage,
            now_unix,
            now_unix + 3600,
            1,
        )
        .unwrap();

        // Valid verification succeeds
        assert!(voucher.verify(&local.peer_id(), Some(now_unix)).is_ok());

        // Wrong audience rejected
        assert!(voucher
            .verify(&other_audience.peer_id(), Some(now_unix))
            .is_err());

        // Mutate every wire byte region and verify signature/structural failure
        let wire = voucher.to_wire_bytes().unwrap();
        for byte_idx in [0usize, 5, 40, 70, 97, 105, 118, 126, 132, 150, 197] {
            let mut mutated = wire;
            mutated[byte_idx] ^= 0x01;
            if let Ok(decoded) = SignedVoucher::from_wire_bytes(&mutated) {
                assert!(
                    decoded.verify(&local.peer_id(), Some(now_unix)).is_err(),
                    "Mutated byte at index {byte_idx} must fail verification"
                );
            }
        }
    }

    #[test]
    fn test_no_privilege_escalation_or_cyclic_vouch_amplification() {
        let local = Identity::generate();
        let intro_a = Identity::generate();
        let intro_b = Identity::generate();
        let subject = Identity::generate();
        let now = Instant::now();
        let now_unix = 1_700_000_000;

        // 1. Self-vouching rejected at issue and verify
        assert!(SignedVoucher::issue(
            &subject,
            subject.peer_id(),
            local.peer_id(),
            Capability::ReceiveDirectMessage,
            now_unix,
            now_unix + 3600,
            1
        )
        .is_err());

        // 2. Non-delegable capabilities (ActAsIntroducer, PermitOutboundGateway, IssueIntroduction) rejected
        for non_delegable in [
            Capability::ActAsIntroducer,
            Capability::PermitOutboundGateway,
            Capability::IssueIntroduction,
        ] {
            assert!(SignedVoucher::issue(
                &intro_a,
                subject.peer_id(),
                local.peer_id(),
                non_delegable,
                now_unix,
                now_unix + 3600,
                1
            )
            .is_err());
        }

        // 3. Multiple introducers and circular A <-> B endorsements never grant gateway forwarding
        //    or introducer privileges when local policy forbids vouched gateway forwarding
        let mut store = TrustStore::new(local.peer_id(), TrustQuotaConfig::default());
        let mut caps = HashSet::new();
        caps.insert(Capability::ActAsIntroducer);
        store
            .approve_peer(intro_a.peer_id(), caps.clone(), now_unix)
            .unwrap();
        store
            .approve_peer(intro_b.peer_id(), caps, now_unix)
            .unwrap();

        let v_a = SignedVoucher::issue(
            &intro_a,
            subject.peer_id(),
            local.peer_id(),
            Capability::ReceiveDirectMessage,
            now_unix,
            now_unix + 3600,
            1,
        )
        .unwrap();
        let v_b = SignedVoucher::issue(
            &intro_b,
            subject.peer_id(),
            local.peer_id(),
            Capability::ReceiveDirectMessage,
            now_unix,
            now_unix + 3600,
            1,
        )
        .unwrap();
        store.import_voucher(&v_a, now, Some(now_unix)).unwrap();
        store.import_voucher(&v_b, now, Some(now_unix)).unwrap();

        // Still denied for gateway forwarding and introducer role
        let dec_gw = TrustPolicy::authorize(
            &mut store,
            &subject.peer_id(),
            Capability::UseGatewayForForwarding,
            &default_ctx(now, Some(now_unix)),
        );
        assert_eq!(dec_gw.outcome, AuthOutcome::Deny);

        let dec_intro = TrustPolicy::authorize(
            &mut store,
            &subject.peer_id(),
            Capability::ActAsIntroducer,
            &default_ctx(now, Some(now_unix)),
        );
        assert_eq!(dec_intro.outcome, AuthOutcome::Deny);
    }

    #[test]
    fn test_revocation_before_voucher_and_issuer_revocation_invalidation() {
        let local = Identity::generate();
        let introducer = Identity::generate();
        let subject = Identity::generate();
        let now = Instant::now();
        let now_unix = 1_700_000_000;

        let mut store = TrustStore::new(local.peer_id(), TrustQuotaConfig::default());
        let mut caps = HashSet::new();
        caps.insert(Capability::ActAsIntroducer);
        store
            .approve_peer(introducer.peer_id(), caps, now_unix)
            .unwrap();

        let voucher = SignedVoucher::issue(
            &introducer,
            subject.peer_id(),
            local.peer_id(),
            Capability::ReceiveDirectMessage,
            now_unix,
            now_unix + 3600,
            1,
        )
        .unwrap();

        let revocation = SignedVoucherRevocation::issue(
            &introducer,
            subject.peer_id(),
            local.peer_id(),
            voucher.credential_id,
            1,
            now_unix + 5,
        )
        .unwrap();

        // 1. Out-of-order: Revocation arrives BEFORE voucher
        store.import_revocation(&revocation, now).unwrap();

        // Delayed voucher with revocation_seq == 1 must be rejected!
        assert!(store.import_voucher(&voucher, now, Some(now_unix)).is_err());

        // 2. Issue fresh voucher #2, import, then revoke introducer -> immediate invalidation
        let voucher2 = SignedVoucher::issue(
            &introducer,
            subject.peer_id(),
            local.peer_id(),
            Capability::ReceiveDirectMessage,
            now_unix,
            now_unix + 3600,
            2,
        )
        .unwrap();
        store
            .import_voucher(&voucher2, now, Some(now_unix))
            .unwrap();
        assert_eq!(
            TrustPolicy::authorize(
                &mut store,
                &subject.peer_id(),
                Capability::ReceiveDirectMessage,
                &default_ctx(now, Some(now_unix)),
            )
            .outcome,
            AuthOutcome::Allow
        );

        // Local user revokes introducer -> subject's voucher is immediately purged
        store.revoke_peer(introducer.peer_id(), now_unix + 20);
        let after_revoke = TrustPolicy::authorize(
            &mut store,
            &subject.peer_id(),
            Capability::ReceiveDirectMessage,
            &default_ctx(now, Some(now_unix + 20)),
        );
        assert_ne!(after_revoke.outcome, AuthOutcome::Allow);
    }

    #[test]
    fn test_monotonic_expiry_and_clock_rollback_protection() {
        let local = Identity::generate();
        let introducer = Identity::generate();
        let subject = Identity::generate();
        let now = Instant::now();
        let now_unix = 1_700_000_000;

        let mut store = TrustStore::new(local.peer_id(), TrustQuotaConfig::default());
        let mut caps = HashSet::new();
        caps.insert(Capability::ActAsIntroducer);
        store
            .approve_peer(introducer.peer_id(), caps, now_unix)
            .unwrap();

        let voucher = SignedVoucher::issue(
            &introducer,
            subject.peer_id(),
            local.peer_id(),
            Capability::ReceiveDirectMessage,
            now_unix,
            now_unix + 60, // 60-second lifetime
            1,
        )
        .unwrap();
        store.import_voucher(&voucher, now, Some(now_unix)).unwrap();

        // Advance monotonic time past 60s -> expires
        let expired_dec = TrustPolicy::authorize(
            &mut store,
            &subject.peer_id(),
            Capability::ReceiveDirectMessage,
            &default_ctx(now + Duration::from_secs(61), Some(now_unix + 61)),
        );
        assert_eq!(expired_dec.outcome, AuthOutcome::Deny);
        assert_eq!(expired_dec.reason, AuthReason::VoucherExpired);

        // Roll back wall clock to now_unix + 10 -> must NOT revive an already-expired voucher!
        let rollback_dec = TrustPolicy::authorize(
            &mut store,
            &subject.peer_id(),
            Capability::ReceiveDirectMessage,
            &default_ctx(now + Duration::from_secs(62), Some(now_unix + 10)),
        );
        assert_eq!(rollback_dec.outcome, AuthOutcome::Deny);
        assert_eq!(rollback_dec.reason, AuthReason::VoucherExpired);

        // Unknown clock validity (`now_unix = None`) refuses automatic voucher elevation
        let subject2 = Identity::generate();
        let voucher2 = SignedVoucher::issue(
            &introducer,
            subject2.peer_id(),
            local.peer_id(),
            Capability::ReceiveDirectMessage,
            now_unix,
            now_unix + 3600,
            1,
        )
        .unwrap();
        store
            .import_voucher(&voucher2, now, Some(now_unix))
            .unwrap();
        let unknown_clock_dec = TrustPolicy::authorize(
            &mut store,
            &subject2.peer_id(),
            Capability::ReceiveDirectMessage,
            &default_ctx(now + Duration::from_secs(1), None),
        );
        assert_eq!(unknown_clock_dec.outcome, AuthOutcome::Deny);
        assert_eq!(unknown_clock_dec.reason, AuthReason::ClockValidityUnknown);
    }

    #[test]
    fn test_quota_exhaustion_and_oversized_batch_rejection() {
        let local = Identity::generate();
        let introducer = Identity::generate();
        let now = Instant::now();
        let now_unix = 1_700_000_000;

        let quotas = TrustQuotaConfig {
            max_approved_introducers: 1,
            max_active_credentials: 5,
            max_import_bytes: 16 * 1024,
            max_vouchers_per_issuer_per_hour: 3,
            max_pending_requests: 2,
            max_cached_revocations: 16,
        };
        let mut store = TrustStore::new(local.peer_id(), quotas);

        let mut caps = HashSet::new();
        caps.insert(Capability::ActAsIntroducer);
        store
            .approve_peer(introducer.peer_id(), caps.clone(), now_unix)
            .unwrap();

        // 2nd introducer exceeds max_approved_introducers (1)
        let extra_intro = Identity::generate();
        assert!(store
            .approve_peer(extra_intro.peer_id(), caps, now_unix)
            .is_err());

        // Per-issuer hourly quota (3): first 3 succeed, 4th rejected
        for i in 0..3 {
            let subj = Identity::generate();
            let v = SignedVoucher::issue(
                &introducer,
                subj.peer_id(),
                local.peer_id(),
                Capability::ReceiveDirectMessage,
                now_unix,
                now_unix + 3600,
                i + 1,
            )
            .unwrap();
            store.import_voucher(&v, now, Some(now_unix)).unwrap();
        }
        let subj_4 = Identity::generate();
        let v_4 = SignedVoucher::issue(
            &introducer,
            subj_4.peer_id(),
            local.peer_id(),
            Capability::ReceiveDirectMessage,
            now_unix,
            now_unix + 3600,
            4,
        )
        .unwrap();
        assert!(store.import_voucher(&v_4, now, Some(now_unix)).is_err());

        // Oversized wire import batch (> 16 KiB) rejected before decoding
        let oversized = vec![0u8; MAX_IMPORT_REQUEST_BYTES + SIGNED_VOUCHER_WIRE_LEN];
        assert!(store
            .import_voucher_batch_wire(&oversized, now, Some(now_unix))
            .is_err());
    }
}
