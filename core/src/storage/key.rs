// CRYPTO-REVIEW:
// What it does: Key provider abstraction and versioned atomic wrapped-DEK lifecycle
// for outbox storage encryption.
// Primitives / crates: `chacha20poly1305` (AEAD wrapping for software/test KEK), `rand::rngs::OsRng`, `zeroize`.
// Reviewer checklist:
// 1. `DataEncryptionKey` zeroizes key material on drop and redacts `Debug` output.
// 2. `WrappedKeyFileProvider` uses a versioned 69-byte record (`b"BC2K"` + format_v1 + key_version + 12B nonce + 48B AEAD ciphertext).
// 3. Writes use atomic write-to-temp + sync + rename so partial writes never corrupt the existing wrapped key.
// 4. If `db_path` exists and wrapped DEK is missing, truncated, oversized, or fails KEK unwrap, `get_or_create_dek`
//    fails closed with `Error::KeyStore` and NEVER silently regenerates a new DEK.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Nonce,
};
use rand::RngCore;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{Error, Result};

pub const WRAPPED_KEY_MAGIC: &[u8; 4] = b"BC2K";
pub const WRAPPED_KEY_FORMAT_V1: u8 = 1;
pub const WRAPPED_KEY_RECORD_LEN: usize = 4 + 1 + 4 + 12 + 48; // 69 bytes
pub const LEGACY_WRAPPED_RECORD_LEN: usize = 12 + 48; // 60 bytes

/// Reported hardware security backing level of the platform KEK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySecurityLevel {
    StrongBox,
    TrustedExecutionEnvironment,
    Software,
    Unknown,
}

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

    /// Report actual hardware backing level of the KEK if known.
    fn security_level(&self) -> KeySecurityLevel {
        KeySecurityLevel::Unknown
    }
}

/// Injected key provider for deterministic testing.
pub struct InjectedKeyProvider {
    dek: DataEncryptionKey,
    level: KeySecurityLevel,
}

impl InjectedKeyProvider {
    pub fn new(key: [u8; 32], version: u32) -> Self {
        Self {
            dek: DataEncryptionKey { version, key },
            level: KeySecurityLevel::Software,
        }
    }

    pub fn random(version: u32) -> Self {
        let mut key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut key);
        Self::new(key, version)
    }
}

impl KeyProvider for InjectedKeyProvider {
    fn get_or_create_dek(&self) -> Result<DataEncryptionKey> {
        Ok(self.dek.clone())
    }

    fn security_level(&self) -> KeySecurityLevel {
        self.level
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

/// Versioned, atomic file-backed wrapped DEK provider enforcing fail-closed semantics
/// when an encrypted SQLite database already exists on disk.
#[derive(ZeroizeOnDrop)]
pub struct WrappedKeyFileProvider {
    #[zeroize(skip)]
    wrapped_dek_path: PathBuf,
    #[zeroize(skip)]
    db_path: PathBuf,
    kek: Option<[u8; 32]>,
    key_version: u32,
    #[zeroize(skip)]
    level: KeySecurityLevel,
}

impl WrappedKeyFileProvider {
    pub fn new(
        wrapped_dek_path: impl AsRef<Path>,
        db_path: impl AsRef<Path>,
        kek: Option<[u8; 32]>,
        key_version: u32,
        level: KeySecurityLevel,
    ) -> Self {
        Self {
            wrapped_dek_path: wrapped_dek_path.as_ref().to_path_buf(),
            db_path: db_path.as_ref().to_path_buf(),
            kek,
            key_version,
            level,
        }
    }

    /// Explicit user-authorized destructive reset wiping wrapped DEK and encrypted DB.
    pub fn destructive_reset(&self) -> Result<()> {
        let _ = fs::remove_file(&self.wrapped_dek_path);
        let _ = fs::remove_file(self.wrapped_dek_path.with_extension("enc.tmp"));
        let _ = fs::remove_file(&self.db_path);
        Ok(())
    }

    fn wrap_v1(kek: &[u8; 32], dek: &[u8; 32], key_version: u32) -> Result<Vec<u8>> {
        let cipher = ChaCha20Poly1305::new_from_slice(kek)
            .map_err(|e| Error::KeyStore(format!("KEK init failed: {e}")))?;
        let mut nonce = [0u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut nonce);

        let mut aad = Vec::with_capacity(9);
        aad.extend_from_slice(WRAPPED_KEY_MAGIC);
        aad.push(WRAPPED_KEY_FORMAT_V1);
        aad.extend_from_slice(&key_version.to_be_bytes());

        let ct = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: dek,
                    aad: &aad,
                },
            )
            .map_err(|e| Error::KeyStore(format!("DEK wrap failed: {e}")))?;

        let mut out = Vec::with_capacity(WRAPPED_KEY_RECORD_LEN);
        out.extend_from_slice(&aad);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    fn unwrap_record(&self, raw: &[u8], kek: &[u8; 32]) -> Result<DataEncryptionKey> {
        let cipher = ChaCha20Poly1305::new_from_slice(kek)
            .map_err(|e| Error::KeyStore(format!("KEK init failed: {e}")))?;

        if raw.len() == WRAPPED_KEY_RECORD_LEN {
            if &raw[0..4] != WRAPPED_KEY_MAGIC {
                return Err(Error::KeyStore("Corrupted wrapped DEK magic header".into()));
            }
            if raw[4] != WRAPPED_KEY_FORMAT_V1 {
                return Err(Error::KeyStore(format!(
                    "Unsupported wrapped DEK format version: {}",
                    raw[4]
                )));
            }
            let mut ver_bytes = [0u8; 4];
            ver_bytes.copy_from_slice(&raw[5..9]);
            let key_version = u32::from_be_bytes(ver_bytes);
            let nonce = &raw[9..21];
            let ct = &raw[21..WRAPPED_KEY_RECORD_LEN];

            let mut pt = cipher
                .decrypt(
                    Nonce::from_slice(nonce),
                    Payload {
                        msg: ct,
                        aad: &raw[0..9],
                    },
                )
                .map_err(|e| Error::KeyStore(format!("Wrapped DEK unwrap failed: {e}")))?;

            if pt.len() != 32 {
                pt.zeroize();
                return Err(Error::KeyStore("Invalid unwrapped DEK length".into()));
            }
            let mut key = [0u8; 32];
            key.copy_from_slice(&pt);
            pt.zeroize();
            Ok(DataEncryptionKey {
                version: key_version,
                key,
            })
        } else {
            Err(Error::KeyStore(format!(
                "Malformed wrapped DEK length: {} bytes (expected {})",
                raw.len(),
                WRAPPED_KEY_RECORD_LEN
            )))
        }
    }

    fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::KeyStore(format!("Dir error: {e}")))?;
        }
        let tmp_path = path.with_extension("enc.tmp");
        {
            let mut f = fs::File::create(&tmp_path)
                .map_err(|e| Error::KeyStore(format!("Temp file create failed: {e}")))?;
            f.write_all(data)
                .map_err(|e| Error::KeyStore(format!("Temp file write failed: {e}")))?;
            f.sync_all()
                .map_err(|e| Error::KeyStore(format!("Temp file sync failed: {e}")))?;
        }
        fs::rename(&tmp_path, path)
            .map_err(|e| Error::KeyStore(format!("Atomic rename failed: {e}")))?;
        Ok(())
    }
}

impl KeyProvider for WrappedKeyFileProvider {
    fn get_or_create_dek(&self) -> Result<DataEncryptionKey> {
        let kek = self
            .kek
            .as_ref()
            .ok_or_else(|| Error::KeyStore("KEK unavailable or deleted in Keystore".into()))?;

        let db_exists = self
            .db_path
            .metadata()
            .map(|m| m.len() > 0)
            .unwrap_or(false);

        if self.wrapped_dek_path.exists() {
            let raw = fs::read(&self.wrapped_dek_path)
                .map_err(|e| Error::KeyStore(format!("Failed to read wrapped DEK: {e}")))?;
            self.unwrap_record(&raw, kek)
        } else {
            // Fail closed if encrypted database already exists!
            if db_exists {
                return Err(Error::KeyStore(
                    "Encrypted outbox DB exists but wrapped DEK file is missing; failing closed"
                        .into(),
                ));
            }
            let mut new_dek = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut new_dek);
            let record = Self::wrap_v1(kek, &new_dek, self.key_version)?;
            Self::write_atomic(&self.wrapped_dek_path, &record)?;
            Ok(DataEncryptionKey {
                version: self.key_version,
                key: new_dek,
            })
        }
    }

    fn security_level(&self) -> KeySecurityLevel {
        self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrapped_key_lifecycle_and_fail_closed_invariants() {
        let dir = std::env::temp_dir().join(format!("bitchat_key_test_{}", rand::random::<u32>()));
        fs::create_dir_all(&dir).unwrap();
        let dek_path = dir.join("bitchat_dek.enc");
        let db_path = dir.join("bitchat_outbox.db");
        let kek = [0x42u8; 32];

        // 1. First use generates and atomically writes 69-byte v1 wrapped DEK
        let provider = WrappedKeyFileProvider::new(
            &dek_path,
            &db_path,
            Some(kek),
            1,
            KeySecurityLevel::TrustedExecutionEnvironment,
        );
        assert_eq!(
            provider.security_level(),
            KeySecurityLevel::TrustedExecutionEnvironment
        );
        let dek1 = provider.get_or_create_dek().unwrap();
        assert_eq!(fs::read(&dek_path).unwrap().len(), WRAPPED_KEY_RECORD_LEN);

        // 2. Cold restart unwraps identical DEK
        let dek2 = provider.get_or_create_dek().unwrap();
        assert_eq!(dek1.key, dek2.key);
        assert_eq!(dek1.version, 1);

        // 3. Simulate existing non-empty database on disk
        fs::write(&db_path, b"SQLite format 3\0...encrypted data").unwrap();

        // 4. Truncated wrapped file -> fails closed (does NOT regenerate DEK!)
        let valid_bytes = fs::read(&dek_path).unwrap();
        fs::write(&dek_path, &valid_bytes[..20]).unwrap();
        assert!(provider.get_or_create_dek().is_err());

        // 5. Oversized wrapped file -> fails closed
        let mut oversized = valid_bytes.clone();
        oversized.extend_from_slice(&[0u8; 10]);
        fs::write(&dek_path, &oversized).unwrap();
        assert!(provider.get_or_create_dek().is_err());

        // 6. Missing wrapped file while DB exists -> fails closed!
        fs::remove_file(&dek_path).unwrap();
        assert!(provider.get_or_create_dek().is_err());

        // 7. Restore valid wrapped file, then test deleted KEK / wrong KEK -> fails closed!
        fs::write(&dek_path, &valid_bytes).unwrap();
        let deleted_kek_provider =
            WrappedKeyFileProvider::new(&dek_path, &db_path, None, 1, KeySecurityLevel::Unknown);
        assert!(deleted_kek_provider.get_or_create_dek().is_err());

        let wrong_kek_provider = WrappedKeyFileProvider::new(
            &dek_path,
            &db_path,
            Some([0x99u8; 32]),
            1,
            KeySecurityLevel::Software,
        );
        assert!(wrong_kek_provider.get_or_create_dek().is_err());

        // 8. Leftover crash temp file does not corrupt valid wrapped file
        fs::write(dek_path.with_extension("enc.tmp"), b"partial crash bytes").unwrap();
        let dek3 = provider.get_or_create_dek().unwrap();
        assert_eq!(dek1.key, dek3.key);

        // 9. Explicit user-authorized destructive reset clears both DB and wrapped key, allowing fresh init
        provider.destructive_reset().unwrap();
        assert!(!dek_path.exists());
        assert!(!db_path.exists());
        let dek_fresh = provider.get_or_create_dek().unwrap();
        assert_ne!(dek1.key, dek_fresh.key);

        let _ = fs::remove_dir_all(&dir);
    }
}
