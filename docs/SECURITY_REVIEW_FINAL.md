# Bitchat 2.0 — Internal Security & Hardening Closure Report (`Task 14`)

> **IMPORTANT SCOPE DISCLOSURE**:
> This document records an **internal engineering security review and adversarial hardening verification** conducted by the project development team and automated verification suite (`core/tests/test_security_hardening.rs`, `core/tests/test_phase6_release_blockers_e2e.rs`, `docs/CRYPTO_REVIEW.md`). **It must NOT be labeled or represented as an independent third-party cryptographic or security audit.** Two Phase 6 cryptographic additions (Task 4 `BCID` v1 protected root identity container & `BC2K` v2 Keystore AAD, and Task 9 `BC2S` v1 `SyncRecord` Ed25519 signatures & `bitchat_sync.db` subkey derivation) remain flagged **Pending Human Cryptographic Review** in `docs/CRYPTO_REVIEW.md`.

---

## 1. Reviewed Subsystems & File Inventory

| Subsystem | Reviewed Source Files | Primary Verification Tests | Outcome |
|---|---|---|---|
| **Root Identity, Keystore AAD & Migration** | `core/src/identity.rs`, `core/src/storage/key.rs`, `app/android/app/src/main/kotlin/com/bitchat/bitchat2/AndroidKeystoreProvider.kt` | `test_phase6_identity_migration_e2e.rs` (`4` tests), `test_s14_secret_redaction_and_zero_plaintext_in_storage_files` | **PASS** (Pending Human Crypto Review on `BCID` v1 / `BC2K` v2) |
| **Noise XX Session Binding, Nonces & Replay** | `core/src/crypto.rs`, `core/src/mesh/peer.rs`, `core/src/envelope.rs` | `core/src/crypto.rs` (`test_send_nonce_exhaustion_refuses_wrap_and_reserved_max`), `test_rb9_nonce_exhaustion_and_replay_window_bounds` | **PASS** |
| **Authenticated Application Framing, ACKs & Dedup** | `core/src/mesh/delivery.rs`, `core/src/api.rs` | `test_delivery_e2e.rs`, `test_rb3_lost_ack_recovery_and_zero_duplicate_ui_display` | **PASS** |
| **Live Authorization, Link Identity States & Quotas** | `core/src/trust/model.rs`, `core/src/trust/policy.rs`, `core/src/mesh/relay.rs`, `core/src/api.rs` | `test_phase6_auth_e2e.rs` (`6` tests), `test_s14_connection_and_identity_churn_bounded`, `test_s14_pending_request_quota_starvation_and_per_sender_isolation`, `test_s14_credential_replay_voucher_revocation_and_non_transitivity` | **PASS** |
| **Encrypted Trust Store & Gateway Consent Persistence** | `core/src/trust/sqlite.rs`, `core/src/gateway/config.rs`, `core/src/gateway/bridge.rs` | `test_phase6_trust_persistence_e2e.rs` (`4` tests), `test_rb5_voucher_revocation_and_gateway_consent_across_restart` | **PASS** |
| **Signed Route Announcements, Epochs & Gateway Wire Bounds** | `core/src/gateway/wire.rs`, `core/src/gateway/routes.rs`, `core/src/gateway/fairness.rs` | `test_s14_malicious_signed_routes_epoch_replay_and_wrong_signer_rejected`, `test_s14_slow_frames_huge_lengths_and_fragment_flood_starvation` | **PASS** |
| **BLE Fragmentation & Resource Budget Enforcement** | `core/src/mesh/fragment.rs`, `core/src/mesh/resource.rs`, `core/src/mesh/policy.rs` | `test_resource_forwarding_e2e.rs` (`7` tests), `test_rb8_ble_mtu_20_fragmentation_loss_and_reassembly_gc` | **PASS** |
| **FFI C-ABI Panic Boundaries, Lengths & Lifecycle** | `core/src/ffi.rs`, `core/src/api.rs`, `core/build.rs`, `MainActivity.kt` | `test_phase6_ffi_lifecycle.rs` (`4` tests), `app/test/phase6_ux_test.dart` (`8` tests) | **PASS** |
| **Volatile Non-Persistence, Backup Rules & Android Hygiene** | `core/src/storage/volatile.rs`, `BitchatBleService.kt`, `backup_rules.xml`, `data_extraction_rules.xml`, `build.gradle.kts` | `test_rb2_volatile_enforcement_and_zero_sqlite_artifacts`, `test_s14_secret_redaction_and_zero_plaintext_in_storage_files` | **PASS** |
| **Experimental Cover Traffic (`BC2C` v1) & Sync (`BC2S` v1)** | `core/src/mesh/cover.rs`, `core/src/sync/{record.rs,merkle.rs,store.rs,protocol.rs}` | `test_phase6_cover_traffic_e2e.rs` (`4` tests), `test_phase6_sync_e2e.rs` (`8` tests), `test_s14_corrupt_cover_and_sync_frames_rejected_cleanly` | **PASS** (Default OFF; Sync Pending Human Crypto Review) |

---

## 2. Adversarial Stress & Hardening Checklist Outcomes

1. **Identity & Session Binding**:
   - Noise XX static X25519 keys are bound via Ed25519 signature over `("bitchat2-handshake-v2", noise_static_pubkey, session_context)`.
   - `PeerLinkIdentityState` enforces `UnboundLink` -> `ClaimedShortId` -> `VerifiedPeer` progression; direct BLE RSSI and single-hop link bindings require `hop_count == 0` and `immediate_relay == Some(origin_sender)`.
2. **Nonces & Replay Protection**:
   - `Session::encrypt` checks `if self.send_nonce >= u64::MAX - 1 { return Err(Error::NonceExhausted); }` **before** encryption or counter increment, preventing wraparound and reserving `u64::MAX`.
   - `SlidingReplayWindow` (128-bit window) rejects duplicate and pre-window nonces in $O(1)$.
3. **Malformed Inputs, Slow Frames, Huge Lengths & Fragment Floods**:
   - `GatewayMessage::read_framed` enforces `MAX_WIRE_FRAME_SIZE = 65,536` bytes and `DEFAULT_READ_TIMEOUT = 10s` via `tokio::select!`.
   - `Reassembler` caps concurrent partial assemblies at `MAX_CONCURRENT_ASSEMBLIES = 128`, total reassembled payload at `MAX_MESSAGE_SIZE = 4,096` bytes, and evicts stale partial slots after `15s`.
   - All `57` C-ABI exports in `core/src/ffi.rs` validate NUL-terminated UTF-8 strings up to `MAX_FFI_STRING_LEN = 64 KiB`, raw frames up to `MAX_FFI_FRAME_LEN = 64 KiB`, and wrap execution in `std::panic::catch_unwind`.
4. **Quota Starvation, Malicious Signed Routes & Credential Replay**:
   - Unapproved pending message requests are capped at `4` per sender and `64` globally (`TrustQuotaConfig::default()`), preventing any single sybil sender from starving the request queue.
   - `RouteSnapshot` verifies Ed25519 signatures over `b"bitchat2/gateway-route-snapshot/v1"`, rejects `> 128` recipients or `> 3600s` leases, and `GatewayRouteTable::apply_snapshot_with_peers` rejects stale epochs, duplicate/decreasing sequences, `u64::MAX` sequence overflow, and mismatched `PeerId` -> `ShortId` bindings.
   - `SignedVoucher` verifies audience binding, expiry, revocations (`SignedVoucherRevocation`), and strict 1-hop non-transitivity (`PeerTrustState::Approved` required for introducer).
5. **Logs, Backups, Storage Encryption & Application-Controlled Non-Persistence**:
   - Custom `Debug` implementations on `Identity`, `Session`, `DekMaterial`, and `GatewayHello` redact secrets (`[REDACTED]`), and secret key buffers implement `ZeroizeOnDrop`.
   - Raw disk scan (`test_s14_secret_redaction_and_zero_plaintext_in_storage_files`) confirms zero occurrences of raw Ed25519 secret bytes, Normal message plaintext, or Volatile message plaintext across `identity.enc`, `bitchat_outbox.db`, `bitchat_trust.db`, and `bitchat_sync.db`.
   - Android `backup_rules.xml` and `data_extraction_rules.xml` exclude `identity.enc`, `identity.key`, `identity. migrating`, `bitchat_outbox.db*`, `bitchat_trust.db*`, `bitchat_sync.db*`, and `bitchat_keystore_prefs.xml` from cloud backup and device-to-device transfer.
   - Foreground service notification sets `NotificationCompat.VISIBILITY_SECRET`; volatile chat enables `WindowManager.LayoutParams.FLAG_SECURE` and disables keyboard `autocorrect`/`enableSuggestions`.
   - `app/android/app/build.gradle.kts` sets `ALLOW_DEV_TOPOLOGY_OVERRIDES = false` in release and fails closed if production release signing secrets are absent.

---

## 3. Findings & Release Recommendation

- **Critical / High Findings**: **0 unresolved** (all release blockers identified in Tasks 1–6 and the 64-step control-frame drain in Task 9 were fixed and verified by regression tests).
- **Medium / Informational Items**:
  1. **Human Cryptographic Sign-Off Pending**: `BCID` v1 / `BC2K` v2 (Task 4) and `BC2S` v1 `SyncRecord` signatures (Task 9) are documented in `docs/CRYPTO_REVIEW.md` for human cryptographic review prior to public production deployment.
  2. **Experimental Features Default OFF**: Cover traffic (`BC2C` v1) and Normal-mode conversation sync (`BC2S` v1) default to **OFF** and must remain opt-in experimental features until physical multi-device evaluation (Task 13) and human crypto review complete.
  3. **Physical Multi-Device Verification Pending**: Physical phone testing (Task 13, `docs/DEVICE_TEST_RESULTS_FINAL.md`) requires human execution with physical Android handsets.
- **Defensible Release Recommendation**: **Approved for Controlled Academic Demonstration & Physical Device Verification Build (`v2.0.6-phase6`, ABI v2)** with Experimental Cover Traffic and Sync **Default OFF**, subject to completing human-run Task 13 physical phone tests and human sign-off on `docs/CRYPTO_REVIEW.md` before wide public production distribution.
