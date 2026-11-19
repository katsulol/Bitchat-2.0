// CRYPTO-REVIEW:
// What it does: Implements Ed25519 cryptographic identity, key generation, signing,
// verification, PeerId (32-byte public key), ShortId (8-byte BLAKE3 prefix), and
// AEAD-protected root identity persistence + crash-safe migration (`ProtectedIdentityStore`).
// Primitives / crates: `ed25519-dalek` (SigningKey, VerifyingKey, Signature), `blake3`
// (`derive_key` with `"bitchat2.root-identity.wrap.v1"`), `chacha20poly1305`, `rand` (`OsRng`), `zeroize`.
// Reviewer checklist:
// 1. Key generation uses `OsRng` (cryptographically secure system randomness).
// 2. Wrapping subkey is purpose-derived via `blake3::derive_key("bitchat2.root-identity.wrap.v1", &dek.key)`
//    (never derived from public identity bytes) and zeroized immediately after cipher setup.
// 3. `BCID` v1 record is bounded at 101 bytes (checked via file metadata BEFORE reading) with AAD
//    authenticating magic, format version, DEK version, and 32-byte Ed25519 public key.
// 4. Crash-safe migration writes `.tmp` -> `fsync` -> rename -> reopen & decrypt-verify -> remove legacy file.
// 5. Missing/wrong KEK, corruption, or dual-file PeerId mismatch ALWAYS fail closed (never regenerate).
// 6. Debug implementations for Identity and KeyStores explicitly redact secret key bytes.

use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Nonce,
};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::{rngs::OsRng, RngCore};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{Error, Result};
use crate::storage::key::{KeyProvider, KeySecurityLevel};

/// PeerId is the 32-byte Ed25519 public key of a node.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PeerId(pub [u8; 32]);

impl PeerId {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Strictly parse a 64-character hexadecimal string into a `PeerId`.
    ///
    /// Permits optional `0x`/`0X` prefix and surrounding whitespace, but strictly rejects:
    /// - Any non-ASCII-hex character (never strips invalid characters)
    /// - Wrong or odd lengths
    /// - All-zero bytes or invalid Ed25519 public key encodings
    pub fn from_hex(s: &str) -> Option<Self> {
        let trimmed = s.trim();
        let clean = trimmed
            .strip_prefix("0x")
            .or_else(|| trimmed.strip_prefix("0X"))
            .unwrap_or(trimmed);

        if clean.len() != 64 || !clean.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }

        let mut bytes = [0u8; 32];
        for (i, slot) in bytes.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16).ok()?;
        }

        if bytes == [0u8; 32] {
            return None;
        }
        VerifyingKey::from_bytes(&bytes).ok()?;
        Some(Self(bytes))
    }

    /// Derive the 8-byte ShortId from blake3(peer_id).
    pub fn short_id(&self) -> ShortId {
        let hash = blake3::hash(&self.0);
        let mut short = [0u8; 8];
        short.copy_from_slice(&hash.as_bytes()[0..8]);
        ShortId(short)
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

    pub fn from_hex(s: &str) -> Option<Self> {
        let trimmed = s.trim();
        let clean = trimmed
            .strip_prefix("0x")
            .or_else(|| trimmed.strip_prefix("0X"))
            .unwrap_or(trimmed);
        if clean.len() != 16 || !clean.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let mut bytes = [0u8; 8];
        for (i, slot) in bytes.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(Self(bytes))
    }

    /// Strictly parse a user-supplied ShortId string.
    ///
    /// Permits optional `0x`/`0X` prefix and an optional single midpoint `-` or `:` separator
    /// (`8 hex chars + '-' + 8 hex chars`), while strictly rejecting:
    /// - Any non-hex characters or placeholder strings (e.g. `"Initializing..."`)
    /// - Wrong or odd lengths
    /// - Broadcast / all-zero ShortId (`0000000000000000`)
    pub fn from_user_input(s: &str) -> Option<Self> {
        let trimmed = s.trim();
        let without_prefix = trimmed
            .strip_prefix("0x")
            .or_else(|| trimmed.strip_prefix("0X"))
            .unwrap_or(trimmed);

        let normalized: String = if without_prefix.len() == 17 {
            let bytes = without_prefix.as_bytes();
            if bytes[8] == b'-' || bytes[8] == b':' {
                let mut combined = String::with_capacity(16);
                combined.push_str(&without_prefix[..8]);
                combined.push_str(&without_prefix[9..]);
                combined
            } else {
                return None;
            }
        } else {
            without_prefix.to_string()
        };

        let parsed = Self::from_hex(&normalized)?;
        if parsed.is_broadcast() {
            return None;
        }
        Some(parsed)
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
        self.peer_id().short_id()
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

pub const PROTECTED_IDENTITY_MAGIC: &[u8; 4] = b"BCID";
pub const PROTECTED_IDENTITY_FORMAT_V1: u8 = 1;
/// Header(41) = magic(4) + fmt_v1(1) + key_ver(4) + pubkey(32), nonce(12), AEAD ciphertext+tag(48).
pub const PROTECTED_IDENTITY_RECORD_LEN: usize = 4 + 1 + 4 + 32 + 12 + 48; // 101 bytes
const ROOT_IDENTITY_WRAP_CONTEXT: &str = "bitchat2.root-identity.wrap.v1";

static IDENTITY_IO_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Actionable, non-silent failure states for protected root identity loading and migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityStorageError {
    KeyUnavailable(String),
    CorruptedRecord(String),
    KeyMismatchOrRestoreFailure(String),
    MigrationMismatch(String),
    IoFailure(String),
}

impl IdentityStorageError {
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::KeyUnavailable(_) => "IDENTITY_KEY_UNAVAILABLE",
            Self::CorruptedRecord(_) => "IDENTITY_CORRUPTED",
            Self::KeyMismatchOrRestoreFailure(_) => "IDENTITY_RESTORE_MISMATCH",
            Self::MigrationMismatch(_) => "IDENTITY_MIGRATION_MISMATCH",
            Self::IoFailure(_) => "IDENTITY_IO_ERROR",
        }
    }
}

impl fmt::Display for IdentityStorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KeyUnavailable(m)
            | Self::CorruptedRecord(m)
            | Self::KeyMismatchOrRestoreFailure(m)
            | Self::MigrationMismatch(m)
            | Self::IoFailure(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for IdentityStorageError {}

/// AEAD-protected root identity persistence (`identity.enc`, `BCID` v1) with crash-safe atomic
/// migration from legacy plaintext `identity.key`.
pub struct ProtectedIdentityStore {
    dir: PathBuf,
    key_provider: Arc<dyn KeyProvider>,
}

impl fmt::Debug for ProtectedIdentityStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProtectedIdentityStore")
            .field("dir", &self.dir)
            .field("security_level", &self.key_provider.security_level())
            .finish()
    }
}

impl ProtectedIdentityStore {
    pub fn new(dir: impl AsRef<Path>, key_provider: Arc<dyn KeyProvider>) -> Self {
        Self {
            dir: dir.as_ref().to_path_buf(),
            key_provider,
        }
    }

    pub fn enc_path(&self) -> PathBuf {
        self.dir.join("identity.enc")
    }

    pub fn tmp_path(&self) -> PathBuf {
        self.dir.join("identity.enc.tmp")
    }

    pub fn legacy_path(&self) -> PathBuf {
        self.dir.join("identity.key")
    }

    pub fn security_level(&self) -> KeySecurityLevel {
        self.key_provider.security_level()
    }

    fn derive_cipher(&self) -> std::result::Result<(u32, ChaCha20Poly1305), IdentityStorageError> {
        let dek = self.key_provider.get_or_create_dek().map_err(|e| {
            IdentityStorageError::KeyUnavailable(format!("Keystore unavailable: {e}"))
        })?;
        let mut wrap_key = blake3::derive_key(ROOT_IDENTITY_WRAP_CONTEXT, &dek.key);
        let cipher_res = ChaCha20Poly1305::new_from_slice(&wrap_key)
            .map_err(|e| IdentityStorageError::KeyUnavailable(format!("Cipher init failed: {e}")));
        wrap_key.zeroize();
        Ok((dek.version, cipher_res?))
    }

    fn read_legacy_identity(&self) -> std::result::Result<Identity, IdentityStorageError> {
        let bytes = fs::read(self.legacy_path()).map_err(|e| {
            IdentityStorageError::IoFailure(format!("Failed to read legacy identity.key: {e}"))
        })?;
        if bytes.len() != 32 {
            return Err(IdentityStorageError::CorruptedRecord(format!(
                "Invalid legacy identity.key length: expected 32 bytes, got {}",
                bytes.len()
            )));
        }
        let mut raw = [0u8; 32];
        raw.copy_from_slice(&bytes);
        let id = Identity::from_bytes(&raw);
        raw.zeroize();
        Ok(id)
    }

    fn read_encrypted_identity(&self) -> std::result::Result<Identity, IdentityStorageError> {
        let enc_path = self.enc_path();
        let meta = fs::metadata(&enc_path).map_err(|e| {
            IdentityStorageError::IoFailure(format!("Failed to inspect identity.enc: {e}"))
        })?;
        if meta.len() != PROTECTED_IDENTITY_RECORD_LEN as u64 {
            return Err(IdentityStorageError::CorruptedRecord(format!(
                "Corrupted identity.enc length: {} bytes (expected {})",
                meta.len(),
                PROTECTED_IDENTITY_RECORD_LEN
            )));
        }

        let raw = fs::read(&enc_path).map_err(|e| {
            IdentityStorageError::IoFailure(format!("Failed to read identity.enc: {e}"))
        })?;
        if raw.len() != PROTECTED_IDENTITY_RECORD_LEN {
            return Err(IdentityStorageError::CorruptedRecord(format!(
                "Truncated/oversized identity.enc read: {} bytes",
                raw.len()
            )));
        }
        if &raw[0..4] != PROTECTED_IDENTITY_MAGIC {
            return Err(IdentityStorageError::CorruptedRecord(
                "Invalid identity.enc magic header".into(),
            ));
        }
        if raw[4] != PROTECTED_IDENTITY_FORMAT_V1 {
            return Err(IdentityStorageError::CorruptedRecord(format!(
                "Unsupported identity.enc format version: {}",
                raw[4]
            )));
        }
        let mut ver_bytes = [0u8; 4];
        ver_bytes.copy_from_slice(&raw[5..9]);
        let record_key_ver = u32::from_be_bytes(ver_bytes);

        let (active_key_ver, cipher) = self.derive_cipher()?;
        if record_key_ver != active_key_ver {
            return Err(IdentityStorageError::KeyMismatchOrRestoreFailure(format!(
                "Root identity key version mismatch (record v{record_key_ver}, keystore v{active_key_ver})"
            )));
        }

        let header_aad = &raw[0..41];
        let mut expected_pubkey = [0u8; 32];
        expected_pubkey.copy_from_slice(&raw[9..41]);
        let nonce = &raw[41..53];
        let ciphertext = &raw[53..PROTECTED_IDENTITY_RECORD_LEN];

        let mut pt = cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: ciphertext,
                    aad: header_aad,
                },
            )
            .map_err(|_| {
                IdentityStorageError::KeyMismatchOrRestoreFailure(
                    "Failed to decrypt root identity (wrong KEK / backup restore mismatch or tampered record)"
                        .into(),
                )
            })?;

        if pt.len() != 32 {
            pt.zeroize();
            return Err(IdentityStorageError::CorruptedRecord(
                "Decrypted root identity length invalid".into(),
            ));
        }
        let mut secret = [0u8; 32];
        secret.copy_from_slice(&pt);
        pt.zeroize();
        let identity = Identity::from_bytes(&secret);
        secret.zeroize();

        if identity.peer_id().0 != expected_pubkey {
            return Err(IdentityStorageError::CorruptedRecord(
                "Decrypted Ed25519 private key does not match header public key".into(),
            ));
        }
        Ok(identity)
    }

    fn write_encrypted_atomic(
        &self,
        identity: &Identity,
    ) -> std::result::Result<(), IdentityStorageError> {
        fs::create_dir_all(&self.dir).map_err(|e| {
            IdentityStorageError::IoFailure(format!("Failed to create storage dir: {e}"))
        })?;

        let (key_ver, cipher) = self.derive_cipher()?;
        let mut nonce = [0u8; 12];
        OsRng.fill_bytes(&mut nonce);

        let mut record = Vec::with_capacity(PROTECTED_IDENTITY_RECORD_LEN);
        record.extend_from_slice(PROTECTED_IDENTITY_MAGIC);
        record.push(PROTECTED_IDENTITY_FORMAT_V1);
        record.extend_from_slice(&key_ver.to_be_bytes());
        record.extend_from_slice(identity.peer_id().as_bytes());

        let mut secret = identity.to_bytes();
        let ct_res = cipher.encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &secret,
                aad: &record[0..41],
            },
        );
        secret.zeroize();
        let ct = ct_res.map_err(|e| {
            IdentityStorageError::IoFailure(format!("Failed to encrypt root identity: {e}"))
        })?;

        record.extend_from_slice(&nonce);
        record.extend_from_slice(&ct);

        let tmp = self.tmp_path();
        {
            let mut file = fs::File::create(&tmp).map_err(|e| {
                IdentityStorageError::IoFailure(format!("Failed to create identity.enc.tmp: {e}"))
            })?;
            file.write_all(&record).map_err(|e| {
                IdentityStorageError::IoFailure(format!("Failed to write identity.enc.tmp: {e}"))
            })?;
            file.sync_all().map_err(|e| {
                IdentityStorageError::IoFailure(format!("Failed to fsync identity.enc.tmp: {e}"))
            })?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
        }

        let enc = self.enc_path();
        fs::rename(&tmp, &enc).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            IdentityStorageError::IoFailure(format!(
                "Failed to atomically commit identity.enc: {e}"
            ))
        })?;

        #[cfg(unix)]
        if let Ok(dir_handle) = fs::File::open(&self.dir) {
            let _ = dir_handle.sync_all();
        }
        Ok(())
    }

    /// Load encrypted root identity or migrate legacy plaintext `identity.key` atomically.
    ///
    /// Crash-safe invariants:
    /// - Orphans left in `identity.enc.tmp` are pruned without removing `identity.key`.
    /// - If BOTH `identity.enc` and `identity.key` exist, both must match public identity; a
    ///   mismatch fails closed without deleting either file.
    /// - Legacy `identity.key` is removed ONLY AFTER writing `identity.enc` atomically AND
    ///   reopening/decrypting it from disk to verify public `PeerId` equivalence.
    /// - Never generates a replacement identity when `identity.enc` exists and cannot decrypt.
    pub fn load_or_migrate(&self) -> std::result::Result<Identity, IdentityStorageError> {
        let lock = IDENTITY_IO_LOCK.get_or_init(|| Mutex::new(()));
        let _guard = lock
            .lock()
            .map_err(|_| IdentityStorageError::IoFailure("Identity lock poisoned".into()))?;

        // Clean up any partial write artifact safely before checking state
        let tmp = self.tmp_path();
        if tmp.exists() {
            let _ = fs::remove_file(&tmp);
        }

        let enc_exists = self.enc_path().exists();
        let legacy_exists = self.legacy_path().exists();

        match (enc_exists, legacy_exists) {
            (true, true) => {
                // Crash occurred after atomic commit of identity.enc but before removing identity.key.
                let enc_id = self.read_encrypted_identity()?;
                let legacy_id = self.read_legacy_identity()?;
                if enc_id.peer_id() != legacy_id.peer_id() {
                    return Err(IdentityStorageError::MigrationMismatch(
                        "Both identity.enc and legacy identity.key exist with different public PeerIds; failing closed"
                            .into(),
                    ));
                }
                fs::remove_file(self.legacy_path()).map_err(|e| {
                    IdentityStorageError::IoFailure(format!(
                        "Failed to remove migrated legacy identity.key: {e}"
                    ))
                })?;
                Ok(enc_id)
            }
            (true, false) => self.read_encrypted_identity(),
            (false, true) => {
                let legacy_id = self.read_legacy_identity()?;
                let expected_peer_id = legacy_id.peer_id();
                self.write_encrypted_atomic(&legacy_id)?;

                // Reopen from disk and decrypt BEFORE removing legacy file
                let verified = self.read_encrypted_identity()?;
                if verified.peer_id() != expected_peer_id {
                    return Err(IdentityStorageError::MigrationMismatch(
                        "Post-commit identity verification mismatch".into(),
                    ));
                }
                fs::remove_file(self.legacy_path()).map_err(|e| {
                    IdentityStorageError::IoFailure(format!(
                        "Failed to clean up legacy identity.key after verified migration: {e}"
                    ))
                })?;
                Ok(verified)
            }
            (false, false) => {
                let fresh = Identity::generate();
                let expected_peer_id = fresh.peer_id();
                self.write_encrypted_atomic(&fresh)?;
                let verified = self.read_encrypted_identity()?;
                if verified.peer_id() != expected_peer_id {
                    return Err(IdentityStorageError::CorruptedRecord(
                        "Newly persisted identity verification failed".into(),
                    ));
                }
                Ok(verified)
            }
        }
    }

    /// Explicit user-confirmed destructive reset of root identity files.
    pub fn destructive_reset_confirmed(
        &self,
        confirm: bool,
    ) -> std::result::Result<(), IdentityStorageError> {
        if !confirm {
            return Err(IdentityStorageError::KeyUnavailable(
                "Destructive identity reset refused without explicit user confirmation".into(),
            ));
        }
        for p in [self.enc_path(), self.tmp_path(), self.legacy_path()] {
            if p.exists() {
                fs::remove_file(&p).map_err(|e| {
                    IdentityStorageError::IoFailure(format!(
                        "Failed to delete {}: {e}",
                        p.display()
                    ))
                })?;
            }
        }
        Ok(())
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

    #[test]
    fn test_strict_short_id_and_peer_id_parsing() {
        let id = Identity::generate();
        let valid_short_hex = id.short_id().to_string();
        let valid_peer_hex = id.peer_id().to_string();

        // 1. Valid inputs (lowercase, uppercase, 0x prefix, single midpoint separator)
        assert_eq!(ShortId::from_hex(&valid_short_hex), Some(id.short_id()));
        assert_eq!(
            ShortId::from_hex(&valid_short_hex.to_uppercase()),
            Some(id.short_id())
        );
        assert_eq!(
            ShortId::from_hex(&format!("0x{valid_short_hex}")),
            Some(id.short_id())
        );
        assert_eq!(
            ShortId::from_user_input(&format!(
                "{}-{}",
                &valid_short_hex[..8],
                &valid_short_hex[8..]
            )),
            Some(id.short_id())
        );
        assert_eq!(
            ShortId::from_user_input(&format!(
                "{}:{}",
                &valid_short_hex[..8],
                &valid_short_hex[8..]
            )),
            Some(id.short_id())
        );
        assert_eq!(PeerId::from_hex(&valid_peer_hex), Some(id.peer_id()));
        assert_eq!(
            PeerId::from_hex(&format!("0X{}", valid_peer_hex.to_uppercase())),
            Some(id.peer_id())
        );

        // 2. Reject placeholder strings and non-hex characters (no character stripping!)
        assert_eq!(ShortId::from_hex("Initializing..."), None);
        assert_eq!(ShortId::from_user_input("Initializing..."), None);
        assert_eq!(ShortId::from_hex("0123456789abcdef_extra_garbage_12"), None);
        assert_eq!(ShortId::from_user_input("0123-4567-89ab-cdef"), None);
        assert_eq!(ShortId::from_user_input("0123456789abcdeg"), None);

        // 3. Reject wrong lengths, odd lengths, empty, and Unicode lookalikes
        assert_eq!(ShortId::from_hex(""), None);
        assert_eq!(ShortId::from_hex("0123456789abcde"), None); // 15 chars
        assert_eq!(ShortId::from_hex("0123456789abcdef0"), None); // 17 chars
        assert_eq!(
            ShortId::from_user_input("０１２３４５６７８９ａｂｃｄｅｆ"),
            None
        ); // fullwidth unicode
        assert_eq!(PeerId::from_hex("012345"), None);

        // 4. Reject all-zero broadcast ShortId in user input and all-zero PeerId
        assert_eq!(ShortId::from_user_input("0000000000000000"), None);
        assert_eq!(PeerId::from_hex(&"0".repeat(64)), None);
    }
}
