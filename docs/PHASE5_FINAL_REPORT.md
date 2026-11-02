# Phase 5 Final Report — Trust, Resource-Aware Relaying, Coarse Proximity & Volatile Chat

**Branch**: `phase-5-trust`  
**Build Version**: `v2.0.5-phase5`  
**Status**: Gate 0, Part A, and Part B Complete (Code, Tests, 400-Run Evaluation, Android Native `.so`, and Flutter UI Verified).

---

## 1. Executive Summary

Phase 5 hardens Bitchat 2.0's trust boundaries, gateway pairing model, storage recovery, resource-aware relay scheduling, coarse BLE signal categorization, and volatile conversation retention:

1. **Gate 0 Prerequisite Repairs (`0.1`–`0.6`)**:
   - **Gateway Consent & Single-Use Pairing (`0.1`)**: Removed unconditional auto-approval after `IdentityBindingResponse`. Uninvited peers enter `PendingGatewayPeers` without route installation or forwarding privileges. Exported `bitchat-gw://` contact URIs carry a single-use, 5-minute `invitation_token` bound inside the Ed25519 signature transcript (`b"bitchat2-gateway-binding-v2"`).
   - **Conservative Coverage, Injected Clock/RNG & Priority Quota (`0.2`)**: Separated `direct_observed` (`1.0` weight) from `inferred_forwarding` (`0.25` weight, never triggers `100%` suppression alone). Injected deterministic RNG (`ChaCha8Rng` in tests, `OsRng` in prod) and monotonic clock. Added per-sender priority flood quota (`4` priority floods / `10s`).
   - **Versioned `BC2K` Wrapped DEK & Timestamp AAD (`0.3`)**: Hardened `AndroidKeystoreProvider.kt` with `BC2K` v1 header (`magic || version || iv_len=12 || iv || ciphertext_tag`) and atomic temp-file rename. Removed silent database deletion on unwrap/decrypt error (`KeystoreInitializationException`). Bound `created_at` and `expires_at` into SQLite ChaCha20-Poly1305 AAD (`schema v2`).
   - **Strict Hex Parsing & BLE Readiness Gating (`0.4`)**: Replaced silent `unwrap_or(0)` hex parsing with `ShortId::from_hex_exact` (16 hex chars) and `PeerId::from_hex` (64 hex chars). Added `isBleReady` gating in `BitchatBleService.kt` (`BLUETOOTH_SCAN`, `BLUETOOTH_CONNECT`, `BLUETOOTH_ADVERTISE`, adapter ON).
   - **Handshake vs Retry Budget & Coalesced Wakeups (`0.5`)**: `NoRouteAvailable` and `HandshakeStarted` keep messages `Queued` without burning `attempts`. Added 500ms per-peer wakeup coalescing on `on_peer_connected`.
   - **Deterministic 400-Run Evaluation Baseline (`0.6`)**: Reconciled `core/examples/evaluate.rs` and `evaluation_results.csv` (100 seeds × 4 policies).

2. **Part A Rust Core (`Tasks 1`–`7`)**:
   - **Scoped Peer Authorization (`Task 1`)**: [`core/src/trust/policy.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/trust/policy.rs) & [`core/src/trust/store.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/trust/store.rs) — `PeerAuthorizationState`, `Capability` (`PublicBroadcast`, `DirectMessage`, `GatewayUse`, `GatewayForward`, `VouchIntroducer`), strict `Deny > Allow`.
   - **One-Hop Cryptographic Vouchers (`Task 2`)**: [`core/src/trust/voucher.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/trust/voucher.rs) — 198-byte Ed25519-signed `SignedVoucher` with domain prefix `b"bitchat2-voucher-v1"`, audience binding, max 7-day lifetime, max 8 active per introducer / 64 total, non-transitive (`DirectMessage` / `GatewayForward` only), cascading revocation.
   - **Gateway Trust Integration & Atomic Route Failover (`Task 3`)**: [`core/src/gateway/bridge.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/gateway/bridge.rs) & [`core/src/gateway/routes.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/gateway/routes.rs) — Enforces `GatewayForward` / `GatewayUse` and preserves alternate gateway routes when a compromised gateway is revoked.
   - **Resource-Budgeted Relaying (`Task 4`)**: [`core/src/mesh/resource.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/mesh/resource.rs) — `HighCapacity`, `Normal`, `LowPower`, `CriticalPause` with 2-point battery hysteresis (`15%/17%`, `30%/33%`), 5s cooldown, and `1:4:8` token buckets (`Low` / `Normal` / `High` traffic classes). Self-originated traffic and ACKs are never dropped by relay budgets.
   - **Coarse Local BLE Proximity (`Task 5`)**: [`core/src/mesh/proximity.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/mesh/proximity.rs) — Direct-link RSSI median filter (window 8, min 3 samples, 8 dB hysteresis `-65/-73 dBm`, 15s TTL) yielding `Unknown`, `Further`, or `Nearby`.
   - **Volatile Chat Negotiation & Memory Retention (`Task 6`)**: [`core/src/storage/volatile.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/storage/volatile.rs) — Noise-authenticated `VolatileNegotiationFrame` (`b"BC2V"`), `Zeroizing<Vec<u8>>` RAM store, automatic `OutboxStore::cancel()` on expired queued messages, and downgrade prevention.
   - **Public-Runtime E2E Suite & 400-Run Evaluation (`Task 7`)**: [`core/tests/phase5_policy_e2e.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/tests/phase5_policy_e2e.rs) & [`docs/PHASE5_EVALUATION.md`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/docs/PHASE5_EVALUATION.md).

3. **Part B Android & Flutter Integration (`Tasks 8.1`–`8.3`)**:
   - **C ABI & Core Service (`Task 8.1`)**: [`core/src/api.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/api.rs) & [`core/src/ffi.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/ffi.rs) — Exposed 11 new C ABI functions for pending gateway consent, peer trust state, voucher issuance/import, device resource updates, BLE RSSI samples, peer proximity, and volatile chat sessions.
   - **Android Native Resource & RSSI Callbacks + `FLAG_SECURE` (`Task 8.2`)**: [`BitchatBleService.kt`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/app/android/app/src/main/kotlin/com/bitchat/bitchat2/BitchatBleService.kt) & [`MainActivity.kt`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/app/android/app/src/main/kotlin/com/bitchat/bitchat2/MainActivity.kt) — Event-driven `BatteryManager` / `PowerManager` broadcasts coalesced to coarse state changes, throttled direct-link BLE RSSI (`<= 1 sample / 1.5s` per peer), and `WindowManager.LayoutParams.FLAG_SECURE` for active volatile chats.
   - **Flutter UI Controls (`Task 8.3`)**: [`app/lib/core_ffi.dart`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/app/lib/core_ffi.dart), [`app/lib/mesh_bridge.dart`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/app/lib/mesh_bridge.dart), [`app/lib/main.dart`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/app/lib/main.dart) — Added Trust & 1-Hop Vouchers sheet, Pending Gateway Consent approval/blocking UI, Volatile Chat timer selector + disclaimer banner, coarse proximity badges on neighbor chips, and Resource-Budgeted Relaying controls in Diagnostics.

---

## 2. Verification Summary

| Check | Command | Result |
|---|---|---|
| Rust Format | `cargo fmt --all -- --check` | **PASS** (0 diffs) |
| Rust Clippy | `cargo clippy --all-targets -- -D warnings` | **PASS** (0 warnings) |
| Rust Test Suite | `cargo test --all` | **PASS** (**153 passed, 0 failed**: 146 unit + 1 Phase 3 E2E + 1 Phase 4 E2E + 5 Phase 5 E2E) |
| 400-Run Evaluation | `cargo run --release --example evaluate` | **PASS** (`evaluation_results.csv` & [`docs/PHASE5_EVALUATION.md`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/docs/PHASE5_EVALUATION.md)) |
| Dart Static Analyzer | `cd app && flutter analyze` | **PASS** (`No issues found!`) |
| Flutter Widget Test | `cd app && flutter test` | **PASS** (`All tests passed!`) |
| Android Native `.so` | `cargo build --manifest-path core/Cargo.toml --target aarch64-linux-android --release` | **PASS** (`libbitchat2_core.so` copied to `jniLibs/arm64-v8a/`) |
| Android Debug APK | `cd app && flutter build apk --debug` | **PASS** (`build/app/outputs/flutter-apk/app-debug.apk`) |

---

## 3. Task 9 — Physical Device Field Testing Guide (For User Execution)

Install `app/build/app/outputs/flutter-apk/app-debug.apk` on 2–3 physical Android phones (`adb install -r app/build/app/outputs/flutter-apk/app-debug.apk`) and run these 5 field checks:

1. **Test 9.1 — Gateway Consent & Single-Use Pairing Invitation**:
   - On **Phone B** (Gateway), tap the cloud icon (`Iroh Gateway`) -> `Start Gateway Bridge` -> `Show QR`.
   - On **Phone A**, scan Phone B's Gateway QR. Verify Phone A connects and is automatically approved via the single-use 5-minute invitation token.
   - Try scanning the **exact same QR** on **Phone C** (or re-importing it). Verify the consumed token is rejected and Phone C appears under **Pending Gateway Consent** on Phone B until manually approved or blocked.
2. **Test 9.2 — One-Hop Cryptographic Voucher (`bitchat-voucher://`)**:
   - On **Phone B** (Verifier), tap the shield-check icon (`Trust & Vouchers`) -> copy Phone B's 64-hex `PeerId`, and approve **Phone A**'s 64-hex `PeerId` using `Approve + Introducer`.
   - On **Phone A** (Introducer), open `Trust & Vouchers` -> enter **Phone C**'s 64-hex `PeerId` as Subject and **Phone B**'s 64-hex `PeerId` as Audience -> tap `Issue Voucher` -> `Show QR`.
   - On **Phone B**, scan Phone A's Voucher QR. Verify the snackbar confirms *"Introduced peer <ShortId> via 1-hop voucher (scoped capability only)"*.
   - Attempt to issue a voucher from **Phone C** (vouched peer, not an approved introducer) for a fourth peer and verify Phone B rejects it.
3. **Test 9.3 — Resource-Budgeted Relaying & Battery Saver Transition**:
   - On a relay phone, tap `Info` (`Node & Outbox Diagnostics`). Observe `Active Profile`, `Battery`, and `OS Power Save`.
   - Turn on Android **Battery Saver** in system quick settings or toggle **Pause Third-Party Relaying** in the Diagnostics sheet.
   - Verify `Active Profile` transitions to `LowPower` or `CriticalPause` while direct messages and ACKs originated by that phone continue to send and deliver normally.
4. **Test 9.4 — Coarse Local BLE Proximity**:
   - With two phones connected over BLE at ~0.5m, observe the neighbor chip badge transition from `Unknown` to `Nearby` after 3+ scan RSSI samples (~5 seconds).
   - Move the phones 5–8m apart or behind an obstacle and observe the badge transition to `Further` (hysteresis prevents rapid flapping).
5. **Test 9.5 — Volatile Chat Retention & `FLAG_SECURE`**:
   - Select a connected neighbor chip, tap the **Timer icon** in the message input bar, and choose `Volatile Chat: 5 min (300s)`.
   - Verify the orange volatile banner appears (*"local RAM-only retention; remote deletion depends on peer compliance..."*) and try taking a screenshot on Android — verify `FLAG_SECURE` blocks the screenshot.
   - Force-stop and relaunch the app: verify volatile messages were stored only in RAM and never written to SQLite disk storage.
