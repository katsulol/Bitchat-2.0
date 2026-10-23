use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use iroh::EndpointId;
use serde::{Deserialize, Serialize};

use crate::identity::Identity;

pub const BINDING_DOMAIN_LABEL: &str = "bitchat2/iroh-binding/v1";
pub const BINDING_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BindingError {
    #[error("Invalid domain separation label: expected {expected}, got {got}")]
    InvalidDomain { expected: String, got: String },
    #[error("Unsupported binding version: {0}")]
    UnsupportedVersion(u32),
    #[error("Remote Iroh endpoint key does not match binding claim")]
    EndpointKeyMismatch,
    #[error("Invalid Ed25519 public key in binding")]
    InvalidPublicKey,
    #[error("Invalid Ed25519 signature format")]
    InvalidSignature,
    #[error("Cryptographic signature verification failed")]
    SignatureVerificationFailed,
}

/// Canonical signed binding authorizing an Iroh transport endpoint for a Bitchat identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityBinding {
    pub domain: String,
    pub version: u32,
    pub bitchat_pubkey_hex: String,
    pub iroh_endpoint_id_hex: String,
    pub created_at_secs: u64,
    pub signature_hex: String,
}

impl IdentityBinding {
    /// Canonical binary encoding for signing and verification.
    pub fn canonical_bytes(
        domain: &str,
        version: u32,
        bitchat_pubkey: &[u8; 32],
        iroh_endpoint_id: &[u8; 32],
        created_at_secs: u64,
    ) -> Vec<u8> {
        let mut buf = Vec::with_capacity(domain.len() + 4 + 32 + 32 + 8);
        buf.extend_from_slice(domain.as_bytes());
        buf.extend_from_slice(&version.to_be_bytes());
        buf.extend_from_slice(bitchat_pubkey);
        buf.extend_from_slice(iroh_endpoint_id);
        buf.extend_from_slice(&created_at_secs.to_be_bytes());
        buf
    }

    /// Sign and construct a binding from a local Bitchat identity and Iroh endpoint.
    pub fn sign(
        bitchat_id: &Identity,
        iroh_endpoint_id: &EndpointId,
        created_at_secs: u64,
    ) -> Self {
        let bitchat_pubkey = bitchat_id.peer_id().0;
        let iroh_endpoint_bytes = *iroh_endpoint_id.as_bytes();

        let canonical = Self::canonical_bytes(
            BINDING_DOMAIN_LABEL,
            BINDING_VERSION,
            &bitchat_pubkey,
            &iroh_endpoint_bytes,
            created_at_secs,
        );

        let sig = bitchat_id.sign(&canonical);

        Self {
            domain: BINDING_DOMAIN_LABEL.to_string(),
            version: BINDING_VERSION,
            bitchat_pubkey_hex: hex_encode(bitchat_pubkey),
            iroh_endpoint_id_hex: hex_encode(iroh_endpoint_bytes),
            created_at_secs,
            signature_hex: hex_encode(sig.to_bytes()),
        }
    }

    /// Verify signature and authenticate that the statement matches the remote endpoint.
    pub fn verify(&self, expected_remote_endpoint: &EndpointId) -> Result<(), BindingError> {
        if self.domain != BINDING_DOMAIN_LABEL {
            return Err(BindingError::InvalidDomain {
                expected: BINDING_DOMAIN_LABEL.to_string(),
                got: self.domain.clone(),
            });
        }
        if self.version != BINDING_VERSION {
            return Err(BindingError::UnsupportedVersion(self.version));
        }

        let iroh_bytes = hex_decode_32(&self.iroh_endpoint_id_hex)
            .map_err(|_| BindingError::EndpointKeyMismatch)?;

        if &iroh_bytes != expected_remote_endpoint.as_bytes() {
            return Err(BindingError::EndpointKeyMismatch);
        }

        let bitchat_pubkey_bytes =
            hex_decode_32(&self.bitchat_pubkey_hex).map_err(|_| BindingError::InvalidPublicKey)?;

        let verifying_key = VerifyingKey::from_bytes(&bitchat_pubkey_bytes)
            .map_err(|_| BindingError::InvalidPublicKey)?;

        let sig_bytes =
            hex_decode_64(&self.signature_hex).map_err(|_| BindingError::InvalidSignature)?;

        let sig = Signature::from_bytes(&sig_bytes);

        let canonical = Self::canonical_bytes(
            &self.domain,
            self.version,
            &bitchat_pubkey_bytes,
            &iroh_bytes,
            self.created_at_secs,
        );

        verifying_key
            .verify(&canonical, &sig)
            .map_err(|_| BindingError::SignatureVerificationFailed)?;

        Ok(())
    }
}

fn hex_encode<T: AsRef<[u8]>>(data: T) -> String {
    data.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode_32(s: &str) -> Result<[u8; 32], ()> {
    let clean = s.trim().trim_start_matches("0x");
    if clean.len() != 64 {
        return Err(());
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16).map_err(|_| ())?;
    }
    Ok(out)
}

fn hex_decode_64(s: &str) -> Result<[u8; 64], ()> {
    let clean = s.trim().trim_start_matches("0x");
    if clean.len() != 128 {
        return Err(());
    }
    let mut out = [0u8; 64];
    for i in 0..64 {
        out[i] = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16).map_err(|_| ())?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey as IrohSecretKey;

    #[test]
    fn test_valid_binding_verification() {
        let bitchat_id = Identity::generate();
        let iroh_secret = IrohSecretKey::generate();
        let iroh_endpoint_id = iroh_secret.public();

        let binding = IdentityBinding::sign(&bitchat_id, &iroh_endpoint_id, 1700000000);
        assert!(binding.verify(&iroh_endpoint_id).is_ok());
    }

    #[test]
    fn test_mismatched_endpoint_key_rejected() {
        let bitchat_id = Identity::generate();
        let iroh_secret1 = IrohSecretKey::generate();
        let iroh_secret2 = IrohSecretKey::generate();

        let binding = IdentityBinding::sign(&bitchat_id, &iroh_secret1.public(), 1700000000);
        let err = binding.verify(&iroh_secret2.public()).unwrap_err();
        assert_eq!(err, BindingError::EndpointKeyMismatch);
    }

    #[test]
    fn test_tampered_signature_rejected() {
        let bitchat_id = Identity::generate();
        let iroh_secret = IrohSecretKey::generate();
        let mut binding = IdentityBinding::sign(&bitchat_id, &iroh_secret.public(), 1700000000);

        // Tamper signature
        binding.signature_hex = "00".repeat(64);
        let err = binding.verify(&iroh_secret.public()).unwrap_err();
        assert_eq!(err, BindingError::SignatureVerificationFailed);
    }

    #[test]
    fn test_unsupported_version_rejected() {
        let bitchat_id = Identity::generate();
        let iroh_secret = IrohSecretKey::generate();
        let mut binding = IdentityBinding::sign(&bitchat_id, &iroh_secret.public(), 1700000000);

        binding.version = 99;
        let err = binding.verify(&iroh_secret.public()).unwrap_err();
        assert_eq!(err, BindingError::UnsupportedVersion(99));
    }
}
