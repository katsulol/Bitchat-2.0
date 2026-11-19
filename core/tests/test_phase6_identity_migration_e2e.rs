use std::path::PathBuf;
use std::sync::Arc;

use bitchat2_core::api::BitchatCoreService;
use bitchat2_core::identity::{
    FileKeyStore, Identity, IdentityStorageError, KeyStore, ProtectedIdentityStore,
    PROTECTED_IDENTITY_RECORD_LEN,
};
use bitchat2_core::storage::key::{
    FailingKeyProvider, InjectedKeyProvider, KeyProvider, KeySecurityLevel, WrappedKeyFileProvider,
};

fn fresh_temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bitchat_p6_id_{label}_{}", rand::random::<u64>()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[tokio::test]
async fn test_t4_first_install_uses_protected_identity_enc_and_preserves_peer_id() {
    let dir = fresh_temp_dir("first_install");
    let dek = [0x11u8; 32];

    let peer_id_1 = {
        let svc = BitchatCoreService::new_with_key(dir.to_str().unwrap().to_string(), Some(dek), 1)
            .await
            .unwrap();
        let diag = svc.get_diagnostics_snapshot().await;
        assert_eq!(diag.key_security_level, "Software");
        svc.my_peer_id()
    };

    assert!(dir.join("identity.enc").exists());
    assert_eq!(
        std::fs::metadata(dir.join("identity.enc")).unwrap().len(),
        PROTECTED_IDENTITY_RECORD_LEN as u64
    );
    assert!(
        !dir.join("identity.key").exists(),
        "First install must never write plaintext identity.key"
    );

    let svc2 = BitchatCoreService::new_with_key(dir.to_str().unwrap().to_string(), Some(dek), 1)
        .await
        .unwrap();
    assert_eq!(svc2.my_peer_id(), peer_id_1);

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn test_t4_legacy_upgrade_preserves_public_key_and_removes_plaintext() {
    let dir = fresh_temp_dir("legacy_upgrade");
    let legacy_id = Identity::generate();
    let expected_peer_id = legacy_id.peer_id();
    let legacy_store = FileKeyStore::new(dir.join("identity.key"));
    legacy_store.save(&legacy_id).unwrap();

    // Calling without platform DEK (`dek = None`) over existing legacy key must fail closed rather than
    // overwriting or migrating with an ephemeral DEK!
    let no_dek_res =
        BitchatCoreService::new_with_key(dir.to_str().unwrap().to_string(), None, 1).await;
    assert!(no_dek_res.is_err());
    assert!(dir.join("identity.key").exists());

    // Upgrade with persistent DEK -> migrates atomically, verifies reopened PeerId, deletes plaintext key
    let dek = [0x22u8; 32];
    let svc = BitchatCoreService::new_with_key(dir.to_str().unwrap().to_string(), Some(dek), 1)
        .await
        .unwrap();
    assert_eq!(svc.my_peer_id(), expected_peer_id);
    assert!(dir.join("identity.enc").exists());
    assert!(!dir.join("identity.key").exists());

    let raw_enc = std::fs::read(dir.join("identity.enc")).unwrap();
    let secret = legacy_id.to_bytes();
    assert!(
        !raw_enc.windows(32).any(|w| w == secret),
        "Encrypted identity.enc must never contain raw secret scalar bytes"
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn test_t4_crash_boundaries_and_failure_modes() {
    let kp: Arc<dyn KeyProvider> = Arc::new(InjectedKeyProvider::new([0x33u8; 32], 1));

    // Crash boundary 1: orphaned truncated identity.enc.tmp + valid legacy identity.key
    {
        let dir = fresh_temp_dir("crash_tmp");
        let legacy_id = Identity::generate();
        FileKeyStore::new(dir.join("identity.key"))
            .save(&legacy_id)
            .unwrap();
        std::fs::write(dir.join("identity.enc.tmp"), b"half-written-crash").unwrap();

        let store = ProtectedIdentityStore::new(&dir, Arc::clone(&kp));
        let loaded = store.load_or_migrate().unwrap();
        assert_eq!(loaded.peer_id(), legacy_id.peer_id());
        assert!(dir.join("identity.enc").exists());
        assert!(!dir.join("identity.enc.tmp").exists());
        assert!(!dir.join("identity.key").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    // Crash boundary 2: BOTH valid identity.enc and matching identity.key present
    {
        let dir = fresh_temp_dir("crash_both_match");
        let id = Identity::generate();
        let store = ProtectedIdentityStore::new(&dir, Arc::clone(&kp));
        // Create identity.enc cleanly by saving legacy first and copying before deletion? Simulate by writing legacy match afterward:
        FileKeyStore::new(dir.join("identity.key"))
            .save(&id)
            .unwrap();
        let migrated = store.load_or_migrate().unwrap();
        assert_eq!(migrated.peer_id(), id.peer_id());
        // Recreate matching identity.key as if crash happened right before unlink
        FileKeyStore::new(dir.join("identity.key"))
            .save(&id)
            .unwrap();
        let recovered = store.load_or_migrate().unwrap();
        assert_eq!(recovered.peer_id(), id.peer_id());
        assert!(!dir.join("identity.key").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    // Crash boundary 3: BOTH identity.enc and MISMATCHED identity.key present -> MigrationMismatch
    {
        let dir = fresh_temp_dir("crash_both_mismatch");
        let id1 = Identity::generate();
        let id2 = Identity::generate();
        FileKeyStore::new(dir.join("identity.key"))
            .save(&id1)
            .unwrap();
        let store = ProtectedIdentityStore::new(&dir, Arc::clone(&kp));
        store.load_or_migrate().unwrap();
        // Put different identity.key alongside identity.enc
        FileKeyStore::new(dir.join("identity.key"))
            .save(&id2)
            .unwrap();
        let err = store.load_or_migrate().unwrap_err();
        assert!(matches!(err, IdentityStorageError::MigrationMismatch(_)));
        assert!(dir.join("identity.enc").exists());
        assert!(dir.join("identity.key").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    // Corrupt / truncated / oversized / wrong-KEK / missing-KEK fail closed without regenerating
    {
        let dir = fresh_temp_dir("corrupt_kek");
        let wrapped_prov: Arc<dyn KeyProvider> = Arc::new(WrappedKeyFileProvider::new(
            dir.join("bitchat_dek.enc"),
            dir.join("bitchat_outbox.db"),
            Some([0x44u8; 32]),
            1,
            KeySecurityLevel::TrustedExecutionEnvironment,
        ));
        let store = ProtectedIdentityStore::new(&dir, wrapped_prov);
        assert_eq!(
            store.security_level(),
            KeySecurityLevel::TrustedExecutionEnvironment
        );
        let original = store.load_or_migrate().unwrap();
        let valid_enc = std::fs::read(dir.join("identity.enc")).unwrap();

        // Missing KEK -> KeyUnavailable
        let missing_prov: Arc<dyn KeyProvider> = Arc::new(FailingKeyProvider::new("KEK missing"));
        let err_missing = ProtectedIdentityStore::new(&dir, missing_prov)
            .load_or_migrate()
            .unwrap_err();
        assert!(matches!(
            err_missing,
            IdentityStorageError::KeyUnavailable(_)
        ));
        assert_eq!(std::fs::read(dir.join("identity.enc")).unwrap(), valid_enc);

        // Wrong KEK / restore on another device -> KeyMismatchOrRestoreFailure
        let wrong_prov: Arc<dyn KeyProvider> = Arc::new(InjectedKeyProvider::new([0x99u8; 32], 1));
        let err_wrong = ProtectedIdentityStore::new(&dir, wrong_prov)
            .load_or_migrate()
            .unwrap_err();
        assert!(matches!(
            err_wrong,
            IdentityStorageError::KeyMismatchOrRestoreFailure(_)
        ));

        // Bit-flipped ciphertext / header -> fails closed
        let mut tampered = valid_enc.clone();
        tampered[15] ^= 0x01; // flip public key byte inside header AAD
        std::fs::write(dir.join("identity.enc"), &tampered).unwrap();
        let err_aad = ProtectedIdentityStore::new(&dir, Arc::clone(&kp))
            .load_or_migrate()
            .unwrap_err();
        assert!(matches!(
            err_aad,
            IdentityStorageError::KeyMismatchOrRestoreFailure(_)
        ));

        // Truncated & oversized -> CorruptedRecord (bounded check before read)
        std::fs::write(dir.join("identity.enc"), &valid_enc[..20]).unwrap();
        assert!(matches!(
            store.load_or_migrate().unwrap_err(),
            IdentityStorageError::CorruptedRecord(_)
        ));
        let mut oversized = valid_enc.clone();
        oversized.extend_from_slice(&[0u8; 50]);
        std::fs::write(dir.join("identity.enc"), &oversized).unwrap();
        assert!(matches!(
            store.load_or_migrate().unwrap_err(),
            IdentityStorageError::CorruptedRecord(_)
        ));

        // Restore valid file -> identical PeerId intact
        std::fs::write(dir.join("identity.enc"), &valid_enc).unwrap();
        assert_eq!(
            store.load_or_migrate().unwrap().peer_id(),
            original.peer_id()
        );

        // Destructive reset requires confirm = true
        assert!(store.destructive_reset_confirmed(false).is_err());
        assert!(dir.join("identity.enc").exists());
        store.destructive_reset_confirmed(true).unwrap();
        assert!(!dir.join("identity.enc").exists());

        // Fresh generation allowed after explicit reset
        let new_id = store.load_or_migrate().unwrap();
        assert_ne!(new_id.peer_id(), original.peer_id());
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn test_t4_concurrent_loads_and_explicit_storage_reset() {
    let dir = fresh_temp_dir("concurrent");
    let kp: Arc<dyn KeyProvider> = Arc::new(InjectedKeyProvider::new([0x55u8; 32], 1));
    let expected = ProtectedIdentityStore::new(&dir, Arc::clone(&kp))
        .load_or_migrate()
        .unwrap()
        .peer_id();

    let mut threads = Vec::new();
    for _ in 0..6 {
        let dir_c = dir.clone();
        let kp_c = Arc::clone(&kp);
        threads.push(std::thread::spawn(move || {
            ProtectedIdentityStore::new(&dir_c, kp_c)
                .load_or_migrate()
                .unwrap()
                .peer_id()
        }));
    }
    for t in threads {
        assert_eq!(t.join().unwrap(), expected);
    }

    assert!(BitchatCoreService::destructive_reset_storage(dir.to_str().unwrap(), false).is_err());
    assert!(dir.join("identity.enc").exists());
    BitchatCoreService::destructive_reset_storage(dir.to_str().unwrap(), true).unwrap();
    assert!(!dir.join("identity.enc").exists());

    let _ = std::fs::remove_dir_all(dir);
}
