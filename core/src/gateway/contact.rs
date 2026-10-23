use iroh::{EndpointAddr, EndpointId, RelayUrl};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

use crate::gateway::identity_binding::{BindingError, IdentityBinding};

pub const CONTACT_VERSION: u32 = 1;
pub const MAX_CONTACT_STRING_LEN: usize = 8192;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ContactError {
    #[error("Contact string length {0} exceeds maximum limit of {MAX_CONTACT_STRING_LEN} bytes")]
    Oversized(usize),
    #[error("Malformed contact string: {0}")]
    Malformed(String),
    #[error("Unsupported contact version: {0}")]
    UnsupportedVersion(u32),
    #[error("Invalid endpoint ID hex: {0}")]
    InvalidEndpointId(String),
    #[error("Invalid relay URL: {0}")]
    InvalidRelayUrl(String),
    #[error("Binding verification error: {0}")]
    Binding(#[from] BindingError),
}

/// A shareable gateway contact document used to dial and verify an internet gateway.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayContact {
    pub version: u32,
    pub bitchat_pubkey_hex: String,
    pub iroh_endpoint_id_hex: String,
    pub relay_url: Option<String>,
    pub direct_addresses: Vec<String>,
    pub binding: IdentityBinding,
}

impl GatewayContact {
    /// Create a gateway contact from an EndpointAddr and signed identity binding.
    pub fn new(addr: &EndpointAddr, binding: IdentityBinding) -> Result<Self, ContactError> {
        let relay_url = addr.relay_urls().next().map(|r| r.to_string());
        let direct_addresses = addr.ip_addrs().map(|a| a.to_string()).collect();

        let bitchat_pubkey_hex = binding.bitchat_pubkey_hex.clone();
        let iroh_endpoint_id_hex = binding.iroh_endpoint_id_hex.clone();

        Ok(Self {
            version: CONTACT_VERSION,
            bitchat_pubkey_hex,
            iroh_endpoint_id_hex,
            relay_url,
            direct_addresses,
            binding,
        })
    }

    /// Export contact document as a compact, copyable string.
    pub fn export_to_string(&self) -> Result<String, ContactError> {
        let json = serde_json::to_string(self)
            .map_err(|e| ContactError::Malformed(format!("Serialization error: {e}")))?;
        Ok(format!("bitchat-gw://{}", json))
    }

    /// Import and validate a gateway contact from a copyable string.
    pub fn import_from_str(s: &str) -> Result<Self, ContactError> {
        if s.len() > MAX_CONTACT_STRING_LEN {
            return Err(ContactError::Oversized(s.len()));
        }

        let clean = s.trim().trim_start_matches("bitchat-gw://");
        let contact: GatewayContact = serde_json::from_str(clean)
            .map_err(|e| ContactError::Malformed(format!("Invalid JSON document: {e}")))?;

        if contact.version != CONTACT_VERSION {
            return Err(ContactError::UnsupportedVersion(contact.version));
        }

        // Validate Iroh EndpointId
        let expected_endpoint_id = contact.endpoint_id()?;

        // Verify cryptographically signed identity binding against the claimed endpoint ID
        contact.binding.verify(&expected_endpoint_id)?;

        Ok(contact)
    }

    /// Parse claimed EndpointId from hex.
    pub fn endpoint_id(&self) -> Result<EndpointId, ContactError> {
        let clean = self.iroh_endpoint_id_hex.trim().trim_start_matches("0x");
        if clean.len() != 64 {
            return Err(ContactError::InvalidEndpointId(
                "Expected 64 hex characters".into(),
            ));
        }
        let mut bytes = [0u8; 32];
        for i in 0..32 {
            let slice = &clean[i * 2..i * 2 + 2];
            bytes[i] = u8::from_str_radix(slice, 16).map_err(|e| {
                ContactError::InvalidEndpointId(format!("Hex byte {i} invalid: {e}"))
            })?;
        }
        EndpointId::from_bytes(&bytes)
            .map_err(|e| ContactError::InvalidEndpointId(format!("Key parsing error: {e}")))
    }

    /// Convert to an Iroh EndpointAddr usable for dialing.
    pub fn to_endpoint_addr(&self) -> Result<EndpointAddr, ContactError> {
        let id = self.endpoint_id()?;
        let mut addr = EndpointAddr::new(id);

        if let Some(ref r_url) = self.relay_url {
            let relay = r_url.parse::<RelayUrl>().map_err(|e| {
                ContactError::InvalidRelayUrl(format!("Failed to parse relay url: {e}"))
            })?;
            addr = addr.with_relay_url(relay);
        }

        for addr_str in &self.direct_addresses {
            if let Ok(sock) = addr_str.parse::<SocketAddr>() {
                addr = addr.with_ip_addr(sock);
            }
        }

        Ok(addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use iroh::SecretKey as IrohSecretKey;

    #[test]
    fn test_contact_roundtrip_and_verification() {
        let bitchat_id = Identity::generate();
        let iroh_secret = IrohSecretKey::generate();
        let iroh_id = iroh_secret.public();

        let endpoint_addr =
            EndpointAddr::new(iroh_id).with_ip_addr("127.0.0.1:12345".parse().unwrap());

        let binding = IdentityBinding::sign(&bitchat_id, &iroh_id, 1700000000);
        let contact = GatewayContact::new(&endpoint_addr, binding).unwrap();

        let export_str = contact.export_to_string().unwrap();
        let imported = GatewayContact::import_from_str(&export_str).unwrap();

        assert_eq!(contact, imported);
        assert_eq!(imported.endpoint_id().unwrap(), iroh_id);
    }

    #[test]
    fn test_oversized_contact_rejected() {
        let huge_str = format!("bitchat-gw://{}", "A".repeat(MAX_CONTACT_STRING_LEN + 1));
        let err = GatewayContact::import_from_str(&huge_str).unwrap_err();
        assert_eq!(err, ContactError::Oversized(MAX_CONTACT_STRING_LEN + 14));
    }

    #[test]
    fn test_malformed_contact_rejected() {
        let err = GatewayContact::import_from_str("bitchat-gw://not-json").unwrap_err();
        assert!(matches!(err, ContactError::Malformed(_)));
    }

    #[test]
    fn test_tampered_contact_binding_rejected() {
        let bitchat_id = Identity::generate();
        let iroh_secret = IrohSecretKey::generate();
        let iroh_id = iroh_secret.public();

        let endpoint_addr = EndpointAddr::new(iroh_id);
        let mut binding = IdentityBinding::sign(&bitchat_id, &iroh_id, 1700000000);
        binding.signature_hex = "ff".repeat(64); // Tamper signature

        let contact = GatewayContact::new(&endpoint_addr, binding).unwrap();
        let export_str = contact.export_to_string().unwrap();

        let err = GatewayContact::import_from_str(&export_str).unwrap_err();
        assert_eq!(
            err,
            ContactError::Binding(BindingError::SignatureVerificationFailed)
        );
    }
}
