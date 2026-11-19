# Bitchat 2.0 — Final Release Report, Academic Summary, 7-Minute Demo Script & Project Handoff

- **Version / Build**: `2.0.6+6` (`versionName: "2.0.6"`, `versionCode: 6`, `CORE_ABI_VERSION: 2`)
- **Primary Target Platform**: Android ARM64 (`aarch64-linux-android`, API 26–35)
- **Host Verification Targets**: Windows / Linux `x86_64` (`cargo test --all`, `flutter test`)
- **Branch**: `phase-6` (merged cleanly into `main`)

---

## 1. Executive Summary & Novelty Framing

**Bitchat 2.0** is an offline-first, hybrid peer-to-peer mesh messaging system combining a multi-hop **Bluetooth Low Energy (BLE)** local mesh with opt-in **Iroh QUIC** internet gateway bridges, end-to-end **identity-bound Noise_XX** sessions, fail-closed **Android Keystore (`BC2K` v2 / `BCID` v1)** persistence, **1-hop Ed25519 capability vouchers**, and **coverage-aware, resource-budgeted relay scheduling**.

### Honest Novelty & Contribution Scope
1. **What Bitchat 2.0 Contributes**:
   - **Systems & Security Integration**: End-to-end binding of long-term Ed25519 identities inside `Noise_XX_25519_ChaChaPoly_BLAKE2s` handshakes across untrusted multi-hop BLE relays and opt-in Iroh QUIC gateways, with explicit link identity states (`UnboundLink` → `ClaimedShortId` → `VerifiedPeer`) and origin vs. immediate-relay separation.
   - **Evaluated Coverage-Aware & Resource-Budgeted Mesh Delivery**: A quantitatively benchmarked `CoverageAwareFlood` relay policy with fragmented BLE wire-byte metering (`estimate_fragmented_wire_bytes`), 20% reserved control-frame budgets, and bounded store-and-forward retry scheduling (`ApplicationFrame::Ack`).
   - **Fail-Closed Mobile Key & Trust Architecture**: Hardware-backed `BC2K` v2 DEK wrapping (binding key version + local `PeerId` as AES-GCM AAD), crash-safe atomic migration of legacy `identity.key` to `BCID` v1 `identity.enc`, encrypted SQLite persistence for both outbox (`bitchat_outbox.db`) and trust (`bitchat_trust.db`), and non-transitive 1-hop `SignedVoucher` delegation.
   - **Bounded Experimental Extensions**: Bilateral opt-in link cover traffic (`BC2C` v1, Default OFF) and depth-4 `blake3` Merkle conversation synchronization (`BC2S` v1, Default OFF) benchmarked against an `IdInventory` baseline.
2. **What Bitchat 2.0 Does NOT Claim**:
   - It does **not** claim invention of BLE GATT, QUIC/Iroh, the Noise Protocol Framework, or Merkle trees.
   - It does **not** claim unbounded "guaranteed delivery" (delivery is strictly bounded by TTL, retry limits, and the 7-hop policy horizon).
   - It does **not** claim numerical superiority over external competitor applications (e.g., native `bitchat`, Briar, Bridgefy) without matched head-to-head physical benchmarks; our quantitative baseline comparator is our own `NaiveFlood` and legacy `AdaptiveFlood` under identical simulator conditions.
   - It does **not** claim an independent third-party security audit (`docs/SECURITY_REVIEW_FINAL.md` is an internal engineering security review) or fabricated physical multi-phone completion (`docs/DEVICE_TEST_RESULTS_FINAL.md` marks the 12 physical phone scenarios `Pending` for human execution).

---

## 2. Feature Classification Matrix

Per Task 15 rules, every subsystem is classified across six explicit verification tiers rather than a blanket "100% complete" claim:

| Subsystem / Feature | Implemented | Simulator-Tested | Platform-Tested (Unit / Widget / FFI) | Physical-Tested (Multi-Phone Hardware) | Status Tier |
|---|---|---|---|---|---|
| **Ed25519 Identity + `BCID` v1 Encrypted Root Key (`identity.enc`)** | Yes | Yes | Yes (`test_phase6_identity_migration_e2e.rs`) | Pending (`S10`) | **Implemented / Platform-Tested** |
| **Android Keystore `BC2K` v2 AAD Wrapping & Backup Exclusion** | Yes | N/A | Yes (`AndroidKeystoreProvider.kt`, XML rules) | Pending (`S10`) | **Implemented / Platform-Tested** |
| **Noise_XX Identity-Bound E2E Sessions & `u64::MAX` Nonce Guard** | Yes | Yes | Yes (`crypto.rs`, `test_phase6_release_blockers_e2e.rs`) | Pending (`S1`) | **Implemented / Simulator-Tested** |
| **Explicit Link Identity States (`UnboundLink` / `ClaimedShortId` / `VerifiedPeer`)** | Yes | Yes | Yes (`test_phase6_auth_e2e.rs`) | Pending (`S1`, `S6`) | **Implemented / Simulator-Tested** |
| **Encrypted Trust Store (`bitchat_trust.db`) & 1-Hop `SignedVoucher`** | Yes | Yes | Yes (`test_phase6_trust_persistence_e2e.rs`, `test_security_hardening.rs`) | Pending (`S6`) | **Implemented / Simulator-Tested** |
| **Coverage-Aware BLE Flooding & 4-Byte MTU Fragmentation** | Yes | Yes (`600` runs) | Yes (`test_phase6_release_blockers_e2e.rs`, `fragment.rs`) | Pending (`S2`, `S8`) | **Implemented / Simulator-Tested** |
| **Live Fragmented Wire-Byte Resource Metering & Control Reserve** | Yes | Yes | Yes (`test_resource_forwarding_e2e.rs`) | Pending (`S7`) | **Implemented / Simulator-Tested** |
| **Bounded Store-and-Forward Outbox (`bitchat_outbox.db`) & E2E ACKs** | Yes | Yes (`160` runs) | Yes (`test_phase6_release_blockers_e2e.rs`) | Pending (`S5`) | **Implemented / Simulator-Tested** |
| **Iroh QUIC Gateway Bridge, Consent Pairing & Live `Connection::paths()`** | Yes | Yes | Yes (`test_gateway_e2e.rs`, `test_phase6_ffi_lifecycle.rs`) | Pending (`S3`, `S4`) | **Implemented / Platform-Tested** |
| **Volatile Ephemeral Sessions (RAM-Only, No-Downgrade, `FLAG_SECURE`)** | Yes | Yes | Yes (`test_phase6_release_blockers_e2e.rs`, `phase6_ux_test.dart`) | Pending (`S11`) | **Implemented / Platform-Tested** |
| **Bilateral Link Cover Traffic (`BC2C` v1, `[32, 96, 224]` B Dummies)** | Yes | Yes (`40` runs) | Yes (`test_phase6_cover_traffic_e2e.rs`) | Pending (`S12`) | **Experimental (Default OFF)** |
| **Bounded Depth-4 `blake3` Merkle Conversation Sync (`BC2S` v1)** | Yes | Yes (`21` cells) | Yes (`test_phase6_sync_e2e.rs`) | Pending (`S12`) | **Experimental (Default OFF)** |
| **Flutter Mobile UX (ABI v2 Guard, Requests, Key Recovery, Responsive UI)** | Yes | N/A | Yes (`13/13` Flutter widget tests, `0` analyzer issues) | Pending (`S1`–`S12`) | **Implemented / Platform-Tested** |
| **Multi-Hop Transitive Web-of-Trust (>1 Hop) & Group MLS Channels** | No | No | No | No | **Deferred (Out of Scope)** |

---

## 3. Reconciled System Architecture & Protocol Flows

### 3.1 Layered Architecture
- **Flutter UI & Dart Bridge (`app/lib/main.dart`, `mesh_bridge.dart`, `core_ffi.dart`)**:
  - Verifies `bitchat_core_abi_version() == 2` and `bitchat_build_info()` before starting the mesh polling loop.
  - Exposes mode selector (`Normal`, `Privacy`, `Volatile`), pending message request approval/blocking, 1-hop voucher QR generation/import, resource profile selector (`Auto`, `Full`, `Balanced`, `Minimal`, `Paused`), key recovery card on `KEYSTORE_UNAVAILABLE`, and gated experimental Cover/Sync toggles (Default OFF).
- **Android Native Host (`MainActivity.kt`, `BitchatBleService.kt`, `AndroidKeystoreProvider.kt`)**:
  - Unlocks or initializes the 32-byte random DEK wrapped in `bitchat_dek.enc` (`BC2K` v2 format with `version || local_peer_id` AAD) via Android Keystore hardware-backed AES-256-GCM (`bitchat_master_kek_v1`).
  - Runs dual-role BLE GATT Server + Client (`0xFD6F` service, `0x2A06` characteristic) inside a Foreground Service with `NotificationCompat.VISIBILITY_SECRET` lock-screen redaction and `uiGeneration` stale-callback filtering.
- **Rust Core (`bitchat2_core`, `core/src/`)**:
  - All 57 C-ABI exports in `core/src/ffi.rs` are wrapped in `std::panic::catch_unwind` with strict 64 KiB string/payload bounds and `runtime_generation` lifecycle fencing.

### 3.2 Operating Mode Matrix

| Dimension | Normal Mode (Default) | Privacy Mode | Volatile Session Mode |
|---|---|---|---|
| **Local BLE Mesh Routing** | Enabled (up to 7 hops) | Enabled (up to 7 hops) | Enabled (requires active E2E Noise session) |
| **Iroh Internet Gateway** | Opt-in (`gateway_opted_in` + approved gateway) | **Hard-Blocked** (`PrivacyModeGatewayBlocked`) | **Hard-Blocked** for volatile traffic |
| **Envelope Size Padding** | Reversible bucket padding (`128/256/512/1024` B) | Reversible bucket padding (`128/256/512/1024` B) | Reversible bucket padding (`128/256/512/1024` B) |
| **SQLite Outbox Persistence** | Encrypted in `bitchat_outbox.db` | Encrypted in `bitchat_outbox.db` (BLE only) | **Prohibited** (RAM-only `MemoryOutboxStore`; never downgrades to SQLite) |
| **Conversation Sync (`BC2S`)** | Opt-in (Default OFF; encrypted `bitchat_sync.db`) | **Hard-Blocked** | **Hard-Blocked** (never archived or synced) |
| **Android Screen & Notification Privacy** | Lock-screen notification `VISIBILITY_SECRET` | Lock-screen notification `VISIBILITY_SECRET` | `FLAG_SECURE` window shield + zero-preview notifications + timer purge |

### 3.3 Trust, Authorization & Gateway Model
- **Three-State Link Identity (`PeerLinkIdentityState`)**:
  1. `UnboundLink`: Raw BLE GATT connection before frame exchange.
  2. `ClaimedShortId(ShortId)`: Unverified link-layer `ShortId` from beacon/outer header. Can **never** unlock `TrustPolicy`, proximity display (`get_peer_proximity` returns `"Unknown"`), or gateway routing.
  3. `VerifiedPeer { short_id, peer_id }`: Promoted only after `PeerSessionManager` verifies the Ed25519 signature binding the peer's Noise_XX static key to `PeerId` (`blake3(PeerId)[0..8] == ShortId`).
- **Origin vs. Immediate Relay Separation**:
  - When Relay B forwards Origin A's frame to Recipient C, C attributes link bytes to `immediate_relay_short_id = B` and authorizes application delivery against `authenticated_origin_peer_id = A`.
- **1-Hop Capability Vouchers (`SignedVoucher`, `bitchat2/vouch/v1`)**:
  - Fixed 198-byte canonical binary layout signed by an explicitly approved introducer (`Capability::ActAsIntroducer`).
  - Audience-bound (`audience == local_peer_id`), lifetime-capped ($\le 7$ days), dual monotonic + wall-clock expiry, and strictly non-transitive (vouched peers cannot issue vouchers).
- **Gateway Pairing & Fail-Closed Startup**:
  - Gateways require explicit user opt-in and single-use 5-minute pairing tokens (`GatewayInviteToken`).
  - `bitchat_trust.db` is loaded **before** `GatewayBridge` or `OutboxManager` starts. Unapproved or revoked gateways are rejected before any route snapshot or tunnel frame is processed.

### 3.4 Storage, Upgrade & Rollback Model
- **Encrypted At-Rest Files**:
  - `identity.enc`: `BCID` v1 AES-256-GCM encrypted Ed25519 root key (`aad = "BCID\x01" || key_version_be32`). Automatically migrates legacy plaintext `identity.key` on first startup via temp file + `sync_all` + atomic rename + zero-overwrite shred.
  - `bitchat_dek.enc`: `BC2K` v2 Keystore-wrapped 32-byte DEK (`aad = "BC2K\x02" || key_version_be32 || peer_id_bytes`). Legacy `BC2K` v1 blobs auto-upgrade in place on unlock.
  - `bitchat_outbox.db`, `bitchat_trust.db`, `bitchat_sync.db`: SQLite WAL databases encrypting sensitive columns with per-record random 96-bit nonces and table/row AAD.
- **Upgrade & Rollback Behavior (No Silent Data Wipe)**:
  - **Upgrades (`v2.0.x` → `v2.0.6+6`)**: Preserves the existing Ed25519 `PeerId`, migrates `identity.key` → `identity.enc` atomically, upgrades `BC2K` v1 → v2 AAD, and retains all SQLite databases.
  - **Fail-Closed Unlock Error**: If Keystore hardware keys are invalidated or corrupted, `BitchatCoreService` refuses to silently generate a new identity and surfaces `KEYSTORE_UNAVAILABLE` so the user must explicitly choose retry or `reset_Application_data`.
  - **Rollback Limitation**: Rolling back to a pre-Phase-6 binary after `identity.key` has been migrated to `identity.enc` and `BC2K` v2 is unsupported because pre-Phase-6 binaries do not understand `BCID` v1 or `BC2K` v2 headers; operators must keep `v2.0.6+6` or higher once migrated.

---

## 4. Quantitative Evaluation, Physical Verification & Security Review Summary

### 4.1 Simulator Evaluation (`docs/EVALUATION_FINAL.md`)
Reproducible command:
```powershell
cargo run --manifest-path core/Cargo.toml --release --example evaluate_phase6
```
- **5-Topology Flood Matrix (`600` seeded runs, `20` nodes, `10` seeds $\times 5$ topologies $\times 4$ losses $\times 3$ policies)**:
  - In `DenseCluster` at 0% loss, `CoverageAwareFlood` achieves **100.0% $\pm$ 0.0%** reachable delivery while cutting total transmissions from **600.0** (`NaiveFlood`) to **273.3 $\pm$ 8.5** (**54.5% fewer transmissions**).
  - In `SparseLine` (chain of 20 nodes, 7-hop TTL cap), raw delivery across all 20 nodes is **36.8%** (`7 / 19` destinations), while **hop-budget-reachable delivery ($\le 7$ hops) is 100.0% $\pm$ 0.0%** at 0% loss—separating policy horizon from random packet loss.
  - In `BridgeBottleneck` at 10% link loss, `LegacyAdaptive` (`k=2`) drops to **45.3% $\pm$ 26.8%** delivery due to pre-bridge duplicate suppression, whereas `CoverageAwareFlood` (`k=3`, unverified-coverage floor) improves delivery to **72.1% $\pm$ 26.2%** (+26.8 percentage points).
- **Store-and-Forward Retry Ablation (`160` runs across `SparseLine` & `BridgeBottleneck`)**:
  - At 15% per-hop loss in `SparseLine`, bounded E2E retries (`max_retries=3`) lift reachable delivery from **62.9% $\pm$ 22.7%** (one-shot) to **97.1% $\pm$ 6.1%** (+34.2 percentage points) with **0** duplicate displays (`endpoint_dup_displays = 0` across all 160 runs).
- **Experimental Cover Traffic (`40` runs, Default OFF)**:
  - At `rate=2/min` over a 60s window with 5 active neighbors, cover traffic adds `11,200` B (~`186.7` B/s) of constant-bucket (`[32, 96, 224]` B) dummy traffic and shifts packet-size distribution without degrading chat delivery (**100.0%** at 0% loss).
- **Experimental Merkle vs. ID-Inventory Sync (`21` cells, Default OFF)**:
  - At `N=512` records (100% overlap), `MerkleTreeV1` exchanges **228 bytes** vs. **16,522 bytes** for `IdInventory` (**98.6% bandwidth reduction**).
  - At `N=512` records (95% overlap), `MerkleTreeV1` exchanges **7,908 bytes** vs. **21,490 bytes** (**63.2% bandwidth reduction**).
  - At `N=512` records (0% overlap), the `256 KiB` byte budget triggers a clean `QuotaExceeded` abort at **40,393 bytes** after persisting **201 verified records**, proving hard resource bounds.

### 4.2 Physical Device Verification Status (`docs/DEVICE_TEST_RESULTS_FINAL.md`)
- All 12 mandatory physical hardware scenarios (`S1`–`S12`) and the matched battery telemetry protocol are documented in [DEVICE_TEST_RESULTS_FINAL.md](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/docs/DEVICE_TEST_RESULTS_FINAL.md) with automated ADB log capture via [capture_device_logs.ps1](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/docs/capture_device_logs.ps1).
- Because no physical Android phones are attached to this automated CLI environment, all 12 physical scenarios are recorded as **`Pending`** (0 fabricated hardware pass claims).

### 4.3 Internal Security & Cryptographic Review (`docs/SECURITY_REVIEW_FINAL.md`, `docs/CRYPTO_REVIEW.md`)
- All 7 adversarial stress tests in [test_security_hardening.rs](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/tests/test_security_hardening.rs) pass:
  1. `test_s14_connection_and_identity_churn_bounded`
  2. `test_s14_slow_frames_huge_lengths_and_fragment_flood_starvation`
  3. `test_s14_pending_request_quota_starvation_and_per_sender_isolation`
  4. `test_s14_malicious_signed_routes_epoch_replay_and_wrong_signer_rejected`
  5. `test_s14_credential_replay_voucher_revocation_and_non_transitivity`
  6. `test_s14_corrupt_cover_and_sync_frames_rejected_cleanly`
  7. `test_s14_secret_redaction_and_zero_plaintext_in_storage_files`
- All 5 mandatory threat-model boundaries are explicitly stated in [THREAT_MODEL.md](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/docs/THREAT_MODEL.md#L118-L148):
  1. No guarantee of unique human beings (Sybil identities are cheap; bounded by consent & quotas).
  2. Dishonest approved gateways can drop or delay ciphertexts (mitigated by E2E recipient ACKs and multi-gateway failover, not prevented at transport layer).
  3. BLE RSSI categories (`Immediate`, `Near`, `Far`) are coarse UX hints, never secure distance bounding.
  4. Remote recipients can retain or photograph decrypted messages outside local `FLAG_SECURE`/volatile controls.
  5. Outer 31-byte routing headers (`ShortId`, `msg_id`, `hop_count`) and RF/IP metadata remain observable to local radio sniffers and gateway operators.

---

## 5. Build Provenance, Signing & Installation Guide

### 5.1 Artifact Provenance Ledger (`v2.0.6+6`)
See [BUILD_PROVENANCE.md](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/docs/BUILD_PROVENANCE.md) and [RELEASE_SIGNING.md](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/docs/RELEASE_SIGNING.md) for full details:
- **Application ID**: `com.example.bitchat_app`
- **Version**: `2.0.6+6` (`CORE_ABI_VERSION = 2`)
- **Native ARM64 Shared Library**: `app/android/app/src/main/jniLibs/arm64-v8a/libbitchat2_core.so`
  - Size: `25,426,160` bytes
  - SHA-256: `DA4787A18BB51034B4538308C0CA8FF8FC91D9AA639189FAB0628DEC13BB6473`
- **Labeled Internal Test APK (`-PallowDebugTestSigning=true`)**: `app/build/app/outputs/flutter-apk/app-release.apk`
  - Size: `84,132,881` bytes
  - SHA-256: `3BFF46D8A6A3FEB55550E8A9CB9B34791979F631E7144BEAD11F5AEAFFECCAC4`
- **Fail-Closed Release Signing Policy**:
  - Running `flutter build apk --release` without external keystore secrets (`BITCHAT_KEYSTORE_PATH` or `app/android/key.properties`) immediately aborts with `GradleException("FAIL-CLOSED RELEASE SIGNING: ...")`.
  - `BuildConfig.ALLOW_DEV_TOPOLOGY_OVERRIDES` is compiled as `false` in release builds.
  - Zero `.jks`/`.keystore` files or signing secrets exist in the repository or public artifacts.

### 5.2 Installation, Upgrade & Owner Backup Instructions
1. **Fresh Install or In-Place Upgrade via ADB**:
   ```powershell
   adb install -r app/build/app/outputs/flutter-apk/app-release.apk
   ```
   - `-r` replaces the existing APK while preserving `/data/data/com.example.bitchat_app/files/` (`identity.enc`, `bitchat_dek.enc`, `bitchat_outbox.db`, `bitchat_trust.db`, `bitchat_sync.db`).
2. **Switching Signing Keys (Debug-Signed Test APK ↔ Production Release Keystore)**:
   - Android enforces signature matching on in-place upgrades. If switching from a test-signed APK to an owner-keystore-signed APK, uninstall the test APK first (`adb uninstall com.example.bitchat_app`), which clears app sandbox storage and generates a fresh `PeerId` on next launch.
3. **Owner Keystore Backup (Private — Never Commit to Git)**:
   - Store the production `.jks` file and `key.properties` passphrase in an offline encrypted password manager (e.g., 1Password / Bitwarden / KeePassXC) plus an encrypted USB backup separate from the source repository.

---

## 6. Seven-Minute Live / Recorded Demo Script (Viva & Evaluation Defense)

> [!IMPORTANT]
> **Live vs. Recorded Honesty Rule**: Always state upfront whether a segment is running live on physical phones or showing a labeled recorded fallback / deterministic simulator run. Never present prerecorded video or simulator output as live hardware radio evidence.

### Minute 0:00–1:00 — Architecture, Novelty & Security Boundaries
- **Show**: Architecture diagram in [ARCHITECTURE.md](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/docs/ARCHITECTURE.md) and the Runtime Provenance banner in the app (`ABI v2`, `PeerId`, `ShortId`, Keystore security level).
- **Say**:
  - "Bitchat 2.0 integrates a Rust native core (`libbitchat2_core.so`, ABI v2) with an Android dual-role BLE GATT foreground service and Iroh QUIC gateways."
  - "Long-term Ed25519 keys are encrypted at rest (`BCID` v1) under an Android Keystore hardware-wrapped DEK (`BC2K` v2 with `PeerId` AAD) and bound inside every `Noise_XX` handshake."
  - "We separate unverified link headers (`ClaimedShortId`) from cryptographically verified identities (`VerifiedPeer`)."

### Minute 1:00–2:15 — Local Encrypted BLE Chat & Pending Message Requests
- **Show**: Phones A and B discovering each other over BLE (`0xFD6F`). Phone A sends a first message to Phone B.
- **Observe**:
  - Before Noise_XX completes, proximity shows `Unknown`. Once `VerifiedPeer` is established, coarse proximity (`Immediate` / `Near` / `Far`) appears.
  - On Phone B, the first message from unapproved Phone A enters the **Pending Message Requests** banner (bounded at 32 global slots).
  - Tap **Approve** on Phone B → message moves to the active chat feed and Phone A receives an authenticated end-to-end `ApplicationFrame::Ack` (`DELIVERED`).

### Minute 2:15–3:15 — Offline Outbox Queueing & Authenticated Retry
- **Show**: Toggle Bluetooth OFF on Phone B. Send a message from Phone A (`"Offline queued test"`).
- **Observe**:
  - Message transitions to `QUEUED` in `bitchat_outbox.db` (encrypted at rest with `BC2O` v1 AAD).
  - Toggle Bluetooth ON on Phone B → Phone A's outbox wakes on peer reconnection, transmits with a fresh transport `packet_id` and persistent `logical_id`, receives Phone B's authenticated E2E ACK, and marks `DELIVERED` with zero duplicate display on Phone B.

### Minute 3:15–4:15 — Enforced 3-Node Multi-Hop Relay & Resource Policy
- **Show**: 3-node line topology `A <-> B <-> C` (using physical separation or debug build software adjacency filter, explicitly labeled as software-enforced if on a desk).
- **Observe**:
  - Phone A sends to Phone C; Relay B forwards the 31-byte outer envelope (`hop_count = 1`, `[1h]` badge on C) without reading the Noise ciphertext.
  - Switch Relay B's resource profile chip from `Balanced` to `Minimal` or `Paused` → observe transit forwarding pause/throttle on Relay B while control/ACK reserve (20%) remains protected.

### Minute 4:15–5:15 — Opt-In Iroh Gateway Bridge & Privacy Mode Block
- **Show**: Open the **Iroh P2P Gateway** dialog on Gateway B and Gateway C.
- **Observe**:
  - Generate a 5-minute single-use `GatewayInviteToken` on B, import/approve on C, and inspect live transport path telemetry (`DIRECT` / `RELAYED` / `UNKNOWN` from `iroh::Connection::paths()`).
  - Toggle **Privacy Mode** ON → attempt a gateway-routed send and show that `BitchatCoreService` blocks internet egress (`PrivacyModeGatewayBlocked`), keeping traffic strictly on the local BLE mesh.

### Minute 5:15–6:00 — Volatile Mode (`FLAG_SECURE`) & 1-Hop Capability Vouchers
- **Show**: Enable **Volatile Chat (5m TTL)** between A and B.
- **Observe**:
  - Banner states: *"RAM-only. Never falls back to SQLite. Lost if peer unreachable or app stops."* Android `FLAG_SECURE` blocks screenshots.
  - Open **Trust & 1-Hop Vouchers**, issue a `SignedVoucher` from Introducer B for Subject A bound to Audience C, and note the UI disclosure: *"1-Hop Only: Vouched peers can message you directly, but cannot vouch for others."*

### Minute 6:00–7:00 — Quantitative Simulator Results & Honest Limitations
- **Show**: SVG charts [chart_flood_tx_vs_delivery.svg](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/docs/eval/chart_flood_tx_vs_delivery.svg) and [chart_sync_bytes.svg](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/docs/eval/chart_sync_bytes.svg) from [EVALUATION_FINAL.md](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/docs/EVALUATION_FINAL.md).
- **Say**:
  - "Across 600 matched multi-seed simulator runs, `CoverageAwareFlood` cuts dense-cluster transmissions by 54.5% while improving bridge-bottleneck delivery under 10% loss from 45.3% to 72.1%, and bounded E2E retries raise 15%-loss sparse-line delivery from 62.9% to 97.1%."
  - "Across 21 sync benchmark cells, depth-4 `blake3` Merkle reconciliation cuts 512-record steady-state sync bandwidth by 98.6% (`228` B vs `16,522` B)."
  - "Key limitations: 7-hop policy horizon, non-transitive 1-hop trust, coarse non-cryptographic RSSI proximity, and outer header metadata visibility to local RF observers."

---

## 7. Complete Project Handoff & Operator Reference

### 7.1 Repository Map
- `core/build.rs` — Embeds git commit hash, target triple, profile, and ABI v2 metadata at compile time.
- `core/src/identity.rs` — Ed25519 `Identity`, `PeerId`, `ShortId`, and `ProtectedIdentityStore` (`BCID` v1 atomic migration).
- `core/src/envelope.rs` — 31-byte binary wire header, `auth_bytes` (`hop_count` exclusion), and `[128, 256, 512, 1024]` B reversible padding.
- `core/src/crypto.rs` — Identity-bound `Noise_XX` handshakes, `StatelessTransportState`, 64-packet replay window, and `u64::MAX` nonce exhaustion guard.
- `core/src/mesh/` — `fragment.rs` (4B header fragmentation, 64-frame LRU reassembler), `seen.rs` (LRU + rotating Bloom caches), `relay.rs` (`MeshNode`, `CoverageAwareFlood`, `PeerLinkIdentityState`), `delivery.rs` (`ApplicationFrame`, `DeliveryCoordinator`, `InboxDeduplicator`), `resource.rs` (`ResourceBudgetManager`, fragmented wire-byte metering), `proximity.rs` (coarse RSSI buckets), `cover.rs` (`BC2C` v1 experimental cover traffic).
- `core/src/trust/` — `model.rs`, `voucher.rs` (`SignedVoucher`, `SignedVoucherRevocation`), `store.rs` (`TrustStore`), `policy.rs` (`TrustPolicy`), `sqlite.rs` (`SqliteTrustStore`, `bitchat_trust.db`).
- `core/src/storage/` — `key.rs` (`KeyProvider`, `InjectedKeyProvider`), `db.rs` (`SqliteOutboxStore`, `bitchat_outbox.db`), `volatile.rs` (`MemoryOutboxStore`), `outbox.rs` (`OutboxManager`).
- `core/src/sync/` — `record.rs` (`SyncRecord`, `bitchat2/sync-record/v1`), `merkle.rs` (`MerkleTreeV1`), `store.rs` (`SqliteSyncStore`, `bitchat_sync.db`), `protocol.rs` (`SyncSessionManager`, `BC2S` v1).
- `core/src/gateway/` — `wire.rs` (`GatewayMessage`, `RouteSnapshot`), `routes.rs` (`GatewayRouteTable`), `fairness.rs` (`GatewayRateLimiter`), `consent.rs` (`GatewayConsentStore`, `GatewayInviteToken`), `bridge.rs` (`GatewayBridge`, live `iroh::Connection::paths()`).
- `core/src/api.rs` & `core/src/ffi.rs` — Unified `BitchatCoreService` runtime and 57 panic-guarded C-ABI exports.
- `core/examples/evaluate_phase6.rs` — Reproducible Phase 6 multi-seed evaluation suite (`docs/eval/`).
- `core/tests/` — 16 integration test suites (`72` integration tests total), including `test_phase6_auth_e2e.rs`, `test_phase6_trust_persistence_e2e.rs`, `test_resource_forwarding_e2e.rs`, `test_phase6_identity_migration_e2e.rs`, `test_phase6_ffi_lifecycle.rs`, `test_phase6_release_blockers_e2e.rs`, `test_phase6_cover_traffic_e2e.rs`, `test_phase6_sync_e2e.rs`, and `test_security_hardening.rs`.
- `app/` — Flutter mobile client (`lib/main.dart`, `lib/mesh_bridge.dart`, `lib/core_ffi.dart`), widget test suites (`test/widget_test.dart`, `test/phase6_ux_test.dart`), and Android Kotlin service (`MainActivity.kt`, `BitchatBleService.kt`, `AndroidKeystoreProvider.kt`).

### 7.2 Exact Rebuild & Verification Entry Points
```powershell
# 1. Rust formatting, linting, and full 223-test suite
cargo fmt --manifest-path core/Cargo.toml -- --check
cargo clippy --manifest-path core/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path core/Cargo.toml --all

# 2. Re-run Phase 6 quantitative simulator evaluation
cargo run --manifest-path core/Cargo.toml --release --example evaluate_phase6

# 3. Rebuild native ARM64 Android library and copy to jniLibs
cargo build --manifest-path core/Cargo.toml --target aarch64-linux-android --release
Copy-Item "C:\cargo-target\aarch64-linux-android\release\libbitchat2_core.so" "app\android\app\src\main\jniLibs\arm64-v8a\libbitchat2_core.so" -Force

# 4. Run Flutter static analyzer, widget tests (13/13), and labeled test APK build
Push-Location app
flutter analyze
flutter test
flutter build apk --release -PallowDebugTestSigning=true
Pop-Location
```

### 7.3 Operator Troubleshooting Guide
1. **`ABI_MISMATCH` Banner on Startup**:
   - *Cause*: `libbitchat2_core.so` in `app/android/app/src/main/jniLibs/arm64-v8a/` was built from an older commit (`CORE_ABI_VERSION != 2`).
   - *Fix*: Re-run step 3 above to rebuild and copy `libbitchat2_core.so`, then rebuild the APK.
2. **`KEYSTORE_UNAVAILABLE` Recovery Card on Startup**:
   - *Cause*: Android Keystore key `bitchat_master_kek_v1` could not unwrap `bitchat_dek.enc` (e.g., corrupted `bitchat_dek.enc` or device lock-screen credential reset).
   - *Fix*: Tap **Retry Unlock** first. Only if permanent hardware key loss occurred, tap **Reset Local Identity & Encrypted Storage** (which calls `bitchat_reset_application_data` and initializes a clean identity).
3. **`FAIL-CLOSED RELEASE SIGNING` Gradle Error**:
   - *Cause*: Running `flutter build apk --release` without external signing credentials or `-PallowDebugTestSigning=true`.
   - *Fix*: Provide `BITCHAT_KEYSTORE_PATH` / `app/android/key.properties` for production distribution, or pass `-PallowDebugTestSigning=true` for internal hardware testing.
4. **Proximity Badge Shows `Unknown` Despite Active BLE Connection**:
   - *Cause*: Link is still in `ClaimedShortId` state (Noise_XX handshake has not completed).
   - *Fix*: Wait 1–2 seconds for automatic handshake completion or send a message to trigger session establishment.
