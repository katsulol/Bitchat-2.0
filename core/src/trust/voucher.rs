// CRYPTO-REVIEW:
// What it does: Implements canonical domain-separated Ed25519 one-hop vouchers (`bitchat2/vouch/v1`)
// and signed revocations (`bitchat2/vouch-revoke/v1`) with strict audience binding and lifetime caps.
// Primitives / crates: `ed25519-dalek` (Signature), `rand` (OsRng).
// Reviewer checklist:
// 1. Canonical fixed-size binary layout prevents ambiguous field concatenation or JSON malleability.
// 2. Domain separation (`bitchat2/vouch/v1` vs `bitchat2/vouch-revoke/v1`) prevents cross-protocol replay.
// 3. Audience binding (`audience == local_peer_id`) prevents replaying private introductions into other communities.
// 4. Self-vouching (`issuer == subject`) and non-delegable capabilities (`ActAsIntroducer`, `PermitOutboundGateway`, `IssueIntroduction`) are rejected.
// 5. All input lengths and enum tags are validated before signature verification.

use ed25519_dalek::Signature;
use rand::rngs::OsRng;
use rand::RngCore;

use crate::error::{Error, Result};
use crate::identity::{Identity, PeerId};
use crate::trust::model::Capability;

pub const VOUCH_DOMAIN: &[u8] = b"bitchat2/vouch/v1";
pub const VOUCH_REVOKE_DOMAIN: &[u8] = b"bitchat2/vouch-revoke/v1";
pub const VOUCH_VERSION: u8 = 1;

/// Fixed binary wire length of a `SignedVoucher` (1 + 32 + 32 + 32 + 1 + 16 + 8 + 8 + 4 + 64 = 198 bytes).
pub const SIGNED_VOUCHER_WIRE_LEN: usize = 198;
/// Fixed binary wire length of a `SignedVoucherRevocation` (1 + 32 + 32 + 32 + 16 + 4 + 8 + 64 = 189 bytes).
pub const SIGNED_REVOCATION_WIRE_LEN: usize = 189;
/// Maximum allowed voucher lifetime (7 days = 604,800 seconds).
pub const MAX_VOUCHER_LIFETIME_SECS: u64 = 7 * 24 * 3600;
/// Maximum allowed clock skew for newly issued vouchers (5 minutes = 300 seconds).
pub const MAX_FUTURE_ISSUED_SKEW_SECS: u64 = 300;
/// Maximum total bytes accepted in a single credential import request (16 KiB).
pub const MAX_IMPORT_REQUEST_BYTES: usize = 16 * 1024;

/// One-hop Ed25519 signed endorsement from an approved introducer (`issuer`) for `subject`,
/// bound to a specific intended `audience` verifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedVoucher {
    pub version: u8,
    pub issuer: PeerId,
    pub subject: PeerId,
    pub audience: PeerId,
    pub capability: Capability,
    pub credential_id: [u8; 16],
    pub issued_at_unix: u64,
    pub expires_at_unix: u64,
    pub revocation_seq: u32,
    pub signature: Vec<u8>,
}

impl SignedVoucher {
    /// Issue and sign a new one-hop voucher.
    pub fn issue(
        issuer_identity: &Identity,
        subject: PeerId,
        audience: PeerId,
        capability: Capability,
        issued_at_unix: u64,
        expires_at_unix: u64,
        revocation_seq: u32,
    ) -> Result<Self> {
        let issuer = issuer_identity.peer_id();
        if issuer == subject {
            return Err(Error::Crypto("Self-vouching is prohibited".into()));
        }
        if !capability.is_delegable_by_voucher() {
            return Err(Error::Crypto(format!(
                "Capability {capability:?} cannot be delegated via voucher"
            )));
        }
        if expires_at_unix <= issued_at_unix {
            return Err(Error::Crypto(
                "Voucher expires_at_unix must be strictly greater than issued_at_unix".into(),
            ));
        }
        if expires_at_unix.saturating_sub(issued_at_unix) > MAX_VOUCHER_LIFETIME_SECS {
            return Err(Error::Crypto(format!(
                "Voucher lifetime exceeds maximum {} seconds",
                MAX_VOUCHER_LIFETIME_SECS
            )));
        }

        let mut credential_id = [0u8; 16];
        OsRng.fill_bytes(&mut credential_id);

        let mut voucher = Self {
            version: VOUCH_VERSION,
            issuer,
            subject,
            audience,
            capability,
            credential_id,
            issued_at_unix,
            expires_at_unix,
            revocation_seq,
            signature: Vec::new(),
        };

        let msg = voucher.canonical_signing_bytes();
        let sig = issuer_identity.sign(&msg);
        voucher.signature = sig.to_bytes().to_vec();
        Ok(voucher)
    }

    /// Canonical domain-separated signing payload.
    pub fn canonical_signing_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(VOUCH_DOMAIN.len() + 134);
        buf.extend_from_slice(VOUCH_DOMAIN);
        buf.push(self.version);
        buf.extend_from_slice(self.issuer.as_bytes());
        buf.extend_from_slice(self.subject.as_bytes());
        buf.extend_from_slice(self.audience.as_bytes());
        buf.push(self.capability as u8);
        buf.extend_from_slice(&self.credential_id);
        buf.extend_from_slice(&self.issued_at_unix.to_be_bytes());
        buf.extend_from_slice(&self.expires_at_unix.to_be_bytes());
        buf.extend_from_slice(&self.revocation_seq.to_be_bytes());
        buf
    }

    /// Verify structural invariants, audience binding, time bounds, and Ed25519 signature.
    pub fn verify(&self, expected_audience: &PeerId, now_unix: Option<u64>) -> Result<()> {
        if self.version != VOUCH_VERSION {
            return Err(Error::Crypto(format!(
                "Unsupported voucher version: {}",
                self.version
            )));
        }
        if self.issuer.as_bytes() == &[0u8; 32]
            || self.subject.as_bytes() == &[0u8; 32]
            || self.audience.as_bytes() == &[0u8; 32]
        {
            return Err(Error::Crypto("Zero PeerId in voucher is prohibited".into()));
        }
        if self.issuer == self.subject {
            return Err(Error::Crypto("Self-vouching is prohibited".into()));
        }
        if &self.audience != expected_audience {
            return Err(Error::Crypto(format!(
                "Voucher audience {:?} does not match local peer {:?}",
                self.audience, expected_audience
            )));
        }
        if !self.capability.is_delegable_by_voucher() {
            return Err(Error::Crypto(format!(
                "Capability {:?} is not delegable via voucher",
                self.capability
            )));
        }
        if self.expires_at_unix <= self.issued_at_unix {
            return Err(Error::Crypto(
                "Voucher expires_at_unix <= issued_at_unix".into(),
            ));
        }
        if self.expires_at_unix.saturating_sub(self.issued_at_unix) > MAX_VOUCHER_LIFETIME_SECS {
            return Err(Error::Crypto("Voucher lifetime exceeds maximum".into()));
        }

        let now = now_unix.ok_or_else(|| {
            Error::Crypto("Clock validity unknown: refusing automatic voucher verification".into())
        })?;

        if self.issued_at_unix > now.saturating_add(MAX_FUTURE_ISSUED_SKEW_SECS) {
            return Err(Error::Crypto(
                "Voucher issued_at_unix is too far in the future".into(),
            ));
        }
        if now >= self.expires_at_unix {
            return Err(Error::Crypto("Voucher has expired".into()));
        }

        if self.signature.len() != 64 {
            return Err(Error::Crypto(format!(
                "Invalid voucher signature length: {}",
                self.signature.len()
            )));
        }
        let mut sig_bytes = [0u8; 64];
        sig_bytes.copy_from_slice(&self.signature);
        let sig = Signature::from_bytes(&sig_bytes);

        let msg = self.canonical_signing_bytes();
        Identity::verify(&self.issuer, &msg, &sig)
    }

    /// Fixed-length 198-byte canonical binary encoding.
    pub fn to_wire_bytes(&self) -> Result<[u8; SIGNED_VOUCHER_WIRE_LEN]> {
        if self.signature.len() != 64 {
            return Err(Error::Crypto("Invalid voucher signature length".into()));
        }
        let mut out = [0u8; SIGNED_VOUCHER_WIRE_LEN];
        out[0] = self.version;
        out[1..33].copy_from_slice(self.issuer.as_bytes());
        out[33..65].copy_from_slice(self.subject.as_bytes());
        out[65..97].copy_from_slice(self.audience.as_bytes());
        out[97] = self.capability as u8;
        out[98..114].copy_from_slice(&self.credential_id);
        out[114..122].copy_from_slice(&self.issued_at_unix.to_be_bytes());
        out[122..130].copy_from_slice(&self.expires_at_unix.to_be_bytes());
        out[130..134].copy_from_slice(&self.revocation_seq.to_be_bytes());
        out[134..198].copy_from_slice(&self.signature);
        Ok(out)
    }

    /// Decode from fixed-length 198-byte canonical binary encoding.
    pub fn from_wire_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != SIGNED_VOUCHER_WIRE_LEN {
            return Err(Error::Crypto(format!(
                "Invalid SignedVoucher wire length: expected {}, got {}",
                SIGNED_VOUCHER_WIRE_LEN,
                bytes.len()
            )));
        }
        let version = bytes[0];
        if version != VOUCH_VERSION {
            return Err(Error::Crypto(format!(
                "Unsupported SignedVoucher version: {version}"
            )));
        }
        let mut issuer = [0u8; 32];
        issuer.copy_from_slice(&bytes[1..33]);
        let mut subject = [0u8; 32];
        subject.copy_from_slice(&bytes[33..65]);
        let mut audience = [0u8; 32];
        audience.copy_from_slice(&bytes[65..97]);
        let capability = Capability::from_u8(bytes[97])?;
        let mut credential_id = [0u8; 16];
        credential_id.copy_from_slice(&bytes[98..114]);

        let mut issued_bytes = [0u8; 8];
        issued_bytes.copy_from_slice(&bytes[114..122]);
        let issued_at_unix = u64::from_be_bytes(issued_bytes);

        let mut expires_bytes = [0u8; 8];
        expires_bytes.copy_from_slice(&bytes[122..130]);
        let expires_at_unix = u64::from_be_bytes(expires_bytes);

        let mut seq_bytes = [0u8; 4];
        seq_bytes.copy_from_slice(&bytes[130..134]);
        let revocation_seq = u32::from_be_bytes(seq_bytes);

        let signature = bytes[134..198].to_vec();

        Ok(Self {
            version,
            issuer: PeerId(issuer),
            subject: PeerId(subject),
            audience: PeerId(audience),
            capability,
            credential_id,
            issued_at_unix,
            expires_at_unix,
            revocation_seq,
            signature,
        })
    }
}

/// Signed revocation issued by an introducer (`issuer`) to revoke a previously issued credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedVoucherRevocation {
    pub version: u8,
    pub issuer: PeerId,
    pub subject: PeerId,
    pub audience: PeerId,
    pub credential_id: [u8; 16],
    pub revocation_seq: u32,
    pub revoked_at_unix: u64,
    pub signature: Vec<u8>,
}

impl SignedVoucherRevocation {
    /// Sign a revocation for a specific `SignedVoucher`.
    pub fn issue(
        issuer_identity: &Identity,
        subject: PeerId,
        audience: PeerId,
        credential_id: [u8; 16],
        revocation_seq: u32,
        revoked_at_unix: u64,
    ) -> Result<Self> {
        let issuer = issuer_identity.peer_id();
        let mut rev = Self {
            version: VOUCH_VERSION,
            issuer,
            subject,
            audience,
            credential_id,
            revocation_seq,
            revoked_at_unix,
            signature: Vec::new(),
        };
        let msg = rev.canonical_signing_bytes();
        let sig = issuer_identity.sign(&msg);
        rev.signature = sig.to_bytes().to_vec();
        Ok(rev)
    }

    pub fn canonical_signing_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(VOUCH_REVOKE_DOMAIN.len() + 125);
        buf.extend_from_slice(VOUCH_REVOKE_DOMAIN);
        buf.push(self.version);
        buf.extend_from_slice(self.issuer.as_bytes());
        buf.extend_from_slice(self.subject.as_bytes());
        buf.extend_from_slice(self.audience.as_bytes());
        buf.extend_from_slice(&self.credential_id);
        buf.extend_from_slice(&self.revocation_seq.to_be_bytes());
        buf.extend_from_slice(&self.revoked_at_unix.to_be_bytes());
        buf
    }

    pub fn verify(&self, expected_audience: &PeerId) -> Result<()> {
        if self.version != VOUCH_VERSION {
            return Err(Error::Crypto(format!(
                "Unsupported revocation version: {}",
                self.version
            )));
        }
        if &self.audience != expected_audience {
            return Err(Error::Crypto(format!(
                "Revocation audience {:?} does not match local peer {:?}",
                self.audience, expected_audience
            )));
        }
        if self.signature.len() != 64 {
            return Err(Error::Crypto(format!(
                "Invalid revocation signature length: {}",
                self.signature.len()
            )));
        }
        let mut sig_bytes = [0u8; 64];
        sig_bytes.copy_from_slice(&self.signature);
        let sig = Signature::from_bytes(&sig_bytes);
        let msg = self.canonical_signing_bytes();
        Identity::verify(&self.issuer, &msg, &sig)
    }

    pub fn to_wire_bytes(&self) -> Result<[u8; SIGNED_REVOCATION_WIRE_LEN]> {
        if self.signature.len() != 64 {
            return Err(Error::Crypto("Invalid revocation signature length".into()));
        }
        let mut out = [0u8; SIGNED_REVOCATION_WIRE_LEN];
        out[0] = self.version;
        out[1..33].copy_from_slice(self.issuer.as_bytes());
        out[33..65].copy_from_slice(self.subject.as_bytes());
        out[65..97].copy_from_slice(self.audience.as_bytes());
        out[97..113].copy_from_slice(&self.credential_id);
        out[113..117].copy_from_slice(&self.revocation_seq.to_be_bytes());
        out[117..125].copy_from_slice(&self.revoked_at_unix.to_be_bytes());
        out[125..189].copy_from_slice(&self.signature);
        Ok(out)
    }

    pub fn from_wire_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != SIGNED_REVOCATION_WIRE_LEN {
            return Err(Error::Crypto(format!(
                "Invalid SignedVoucherRevocation wire length: expected {}, got {}",
                SIGNED_REVOCATION_WIRE_LEN,
                bytes.len()
            )));
        }
        let version = bytes[0];
        if version != VOUCH_VERSION {
            return Err(Error::Crypto(format!(
                "Unsupported SignedVoucherRevocation version: {version}"
            )));
        }
        let mut issuer = [0u8; 32];
        issuer.copy_from_slice(&bytes[1..33]);
        let mut subject = [0u8; 32];
        subject.copy_from_slice(&bytes[33..65]);
        let mut audience = [0u8; 32];
        audience.copy_from_slice(&bytes[65..97]);
        let mut credential_id = [0u8; 16];
        credential_id.copy_from_slice(&bytes[97..113]);
        let mut seq_bytes = [0u8; 4];
        seq_bytes.copy_from_slice(&bytes[113..117]);
        let revocation_seq = u32::from_be_bytes(seq_bytes);
        let mut rev_bytes = [0u8; 8];
        rev_bytes.copy_from_slice(&bytes[117..125]);
        let revoked_at_unix = u64::from_be_bytes(rev_bytes);
        let signature = bytes[125..189].to_vec();

        Ok(Self {
            version,
            issuer: PeerId(issuer),
            subject: PeerId(subject),
            audience: PeerId(audience),
            credential_id,
            revocation_seq,
            revoked_at_unix,
            signature,
        })
    }
}
