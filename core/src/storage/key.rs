//! Key provider abstraction for outbox storage encryption.
//!
//! Production persistence will use platform-backed key storage (Android Keystore / iOS Keychain)
//! implemented in Part B. Part A defines the trait and provides an injected key provider
//! for deterministic testing.

use crate::error::{Error, Result};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Data encryption key used to encrypt persistent outbox records.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct DataEncryptionKey {
    pub version: u32,
    pub key: [u8; 32],
}

impl std::fmt::Debug for DataEncryptionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataEncryptionKey")
            .field("version", &self.version)
            .field("key", &"[REDACTED]")
            .finish()
    }
}

/// Abstract contract for storage key providers.
pub trait KeyProvider: Send + Sync {
    /// Retrieve or initialize the active data encryption key.
    fn get_or_create_dek(&self) -> Result<DataEncryptionKey>;
}

/// Injected key provider for deterministic testing in Part A.
pub struct InjectedKeyProvider {
    dek: DataEncryptionKey,
}

impl InjectedKeyProvider {
    pub fn new(key: [u8; 32], version: u32) -> Self {
        Self {
            dek: DataEncryptionKey { version, key },
        }
    }

    pub fn random(version: u32) -> Self {
        use rand::RngCore;
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        Self::new(key, version)
    }
}

impl KeyProvider for InjectedKeyProvider {
    fn get_or_create_dek(&self) -> Result<DataEncryptionKey> {
        Ok(self.dek.clone())
    }
}

/// Key provider that simulates failures (e.g. locked keystore).
pub struct FailingKeyProvider {
    message: String,
}

impl FailingKeyProvider {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl KeyProvider for FailingKeyProvider {
    fn get_or_create_dek(&self) -> Result<DataEncryptionKey> {
        Err(Error::KeyStore(self.message.clone()))
    }
}
