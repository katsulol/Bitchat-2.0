// CRYPTO-REVIEW:
// What it does: Implements Ed25519 cryptographic identity, key generation, signing,
// verification, PeerId (32-byte public key), and ShortId (8-byte BLAKE3 prefix).
// Primitives / crates: `ed25519-dalek` (SigningKey, VerifyingKey, Signature), `blake3`, `zeroize`.
// Reviewer checklist:
// 1. Key generation uses OsRng (cryptographically secure system randomness).
// 2. Secret key material is zeroized on drop and never cloned into unmanaged buffers.
// 3. Debug implementations for Identity and KeyStores explicitly redact secret key bytes.
// 4. Verification strictly checks signatures and rejects malformed/tampered signatures.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use zeroize::ZeroizeOnDrop;

use crate::error::{Error, Result};

/// PeerId is the 32-byte Ed25519 public key of a node.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PeerId(pub [u8; 32]);

impl PeerId {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PeerId({})", hex_encode(&self.0))
    }
}

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex_encode(&self.0))
    }
}

/// ShortId is the first 8 bytes of blake3(public_key), used for compact routing headers.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct ShortId(pub [u8; 8]);

impl ShortId {
    pub fn as_bytes(&self) -> &[u8; 8] {
        &self.0
    }

    pub fn is_broadcast(&self) -> bool {
        self.0 == [0u8; 8]
    }
}

impl fmt::Debug for ShortId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ShortId({})", hex_encode(&self.0))
    }
}

impl fmt::Display for ShortId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex_encode(&self.0))
    }
}

/// Cryptographic identity wrapping an Ed25519 signing key.
#[derive(ZeroizeOnDrop)]
pub struct Identity {
    #[zeroize(skip)]
    verifying_key: VerifyingKey,
    signing_key: SigningKey,
}

impl Identity {
    /// Generate a new identity using cryptographically secure system randomness.
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        Self {
            verifying_key,
            signing_key,
        }
    }

    /// Load identity from 32-byte seed / secret scalar.
    pub fn from_bytes(bytes: &[u8; 32]) -> Self {
        let signing_key = SigningKey::from_bytes(bytes);
        let verifying_key = signing_key.verifying_key();
        Self {
            verifying_key,
            signing_key,
        }
    }

    /// Export raw 32-byte secret key material.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.signing_key.to_bytes()
    }

    /// Full 32-byte public key identity.
    pub fn peer_id(&self) -> PeerId {
        PeerId(self.verifying_key.to_bytes())
    }

    /// 8-byte compact identifier derived from blake3(public_key).
    pub fn short_id(&self) -> ShortId {
        let hash = blake3::hash(&self.verifying_key.to_bytes());
        let mut short = [0u8; 8];
        short.copy_from_slice(&hash.as_bytes()[0..8]);
        ShortId(short)
    }

    /// Sign an arbitrary message buffer with the Ed25519 identity key.
    pub fn sign(&self, message: &[u8]) -> Signature {
        self.signing_key.sign(message)
    }

    /// Verify an Ed25519 signature against a PeerId public key.
    pub fn verify(peer_id: &PeerId, message: &[u8], signature: &Signature) -> Result<()> {
        let vk = VerifyingKey::from_bytes(&peer_id.0)
            .map_err(|e| Error::Crypto(format!("Invalid public key: {e}")))?;
        vk.verify(message, signature)
            .map_err(|e| Error::Crypto(format!("Signature verification failed: {e}")))
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("peer_id", &self.peer_id())
            .field("short_id", &self.short_id())
            .field("secret_key", &"[REDACTED]")
            .finish()
    }
}

/// Abstract storage interface for long-term identity keys.
pub trait KeyStore: Send + Sync {
    fn load(&self) -> Result<Option<Identity>>;
    fn save(&self, identity: &Identity) -> Result<()>;
}

/// In-memory keystore for tests and ephemeral sessions.
#[derive(Default)]
pub struct MemoryKeyStore {
    inner: RwLock<Option<[u8; 32]>>,
}

impl MemoryKeyStore {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(None),
        }
    }
}

impl KeyStore for MemoryKeyStore {
    fn load(&self) -> Result<Option<Identity>> {
        let guard = self
            .inner
            .read()
            .map_err(|_| Error::KeyStore("Lock poisoned".into()))?;
        Ok(guard.as_ref().map(Identity::from_bytes))
    }

    fn save(&self, identity: &Identity) -> Result<()> {
        let mut guard = self
            .inner
            .write()
            .map_err(|_| Error::KeyStore("Lock poisoned".into()))?;
        *guard = Some(identity.to_bytes());
        Ok(())
    }
}

impl fmt::Debug for MemoryKeyStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MemoryKeyStore([REDACTED])")
    }
}

/// Filesystem keystore for local development and CLI nodes.
///
/// Note: On mobile targets (Android/iOS), this must be replaced by platform secure hardware
/// keystores (Android Keystore / iOS Keychain) in a subsequent phase.
pub struct FileKeyStore {
    path: PathBuf,
}

impl FileKeyStore {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }
}

impl KeyStore for FileKeyStore {
    fn load(&self) -> Result<Option<Identity>> {
        if !self.path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&self.path)?;
        if bytes.len() != 32 {
            return Err(Error::KeyStore(format!(
                "Invalid key file length: expected 32 bytes, got {}",
                bytes.len()
            )));
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        Ok(Some(Identity::from_bytes(&key)))
    }

    fn save(&self, identity: &Identity) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let raw = identity.to_bytes();
        fs::write(&self.path, raw)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o600);
            fs::set_permissions(&self.path, perms)?;
        }

        Ok(())
    }
}

impl fmt::Debug for FileKeyStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileKeyStore")
            .field("path", &self.path)
            .finish()
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identity_generate_sign_verify() {
        let id = Identity::generate();
        let message = b"mesh message authentication";
        let sig = id.sign(message);
        assert!(Identity::verify(&id.peer_id(), message, &sig).is_ok());
    }

    #[test]
    fn test_identity_tampered_message_fails() {
        let id = Identity::generate();
        let message = b"legitimate message";
        let tampered = b"tampered message";
        let sig = id.sign(message);
        let res = Identity::verify(&id.peer_id(), tampered, &sig);
        assert!(res.is_err());
    }

    #[test]
    fn test_short_id_derivation() {
        let id = Identity::generate();
        let short = id.short_id();
        let hash = blake3::hash(&id.peer_id().0);
        assert_eq!(short.0, hash.as_bytes()[0..8]);
    }

    #[test]
    fn test_memory_keystore_roundtrip() {
        let store = MemoryKeyStore::new();
        assert!(store.load().unwrap().is_none());

        let id = Identity::generate();
        store.save(&id).unwrap();

        let loaded = store.load().unwrap().expect("identity must be present");
        assert_eq!(id.peer_id(), loaded.peer_id());
        assert_eq!(id.short_id(), loaded.short_id());
    }

    #[test]
    fn test_file_keystore_roundtrip() {
        let temp_dir = std::env::temp_dir().join(format!("bitchat_test_{}", rand::random::<u64>()));
        let key_path = temp_dir.join("identity.key");
        let store = FileKeyStore::new(&key_path);

        assert!(store.load().unwrap().is_none());

        let id = Identity::generate();
        store.save(&id).unwrap();

        let loaded = store.load().unwrap().expect("identity must load from file");
        assert_eq!(id.peer_id(), loaded.peer_id());

        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn test_debug_redaction_never_prints_secret() {
        let id = Identity::generate();
        let debug_str = format!("{id:?}");
        let secret_bytes = id.to_bytes();
        let secret_hex = hex_encode(&secret_bytes);

        assert!(debug_str.contains("[REDACTED]"));
        assert!(!debug_str.contains(&secret_hex));

        let mem_store = MemoryKeyStore::new();
        mem_store.save(&id).unwrap();
        let mem_debug = format!("{mem_store:?}");
        assert!(mem_debug.contains("[REDACTED]"));
        assert!(!mem_debug.contains(&secret_hex));
    }
}
