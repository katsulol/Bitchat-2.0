# Bitchat 2.0 — Phase 6 Baseline & Source Map (Task 0)

- **Branch**: `phase-6` (branched from `adaptive-delivery` at commit `6638a797ed28115bec75a660feda32f1625c0cdd`)
- **Upstream Main Reference**: `603b5f47d936db24c35968c29da67df36fa4b118` (`main` / `phase-5-trust`)
- **Audit Timestamp**: `2026-11-20`
- **Specification Contract**: `PHASE_6.md` (SHA-256 `D7CC899B5B7D46A47946823051C414E42E62511ADEEBDB9C1F82EEBECCD1D869`)
- **Pre-existing Working Tree Notes**: Untracked root planning/status docs (`ADAPTIVE_DELIVERY_FEATURES.md`, `CURRENT_STATUS.md`, `PHASE_6.md`) preserved untouched.

---

## 1. Toolchain, Dependency & Schema Versions

| Component / Artifact | Exact Version / Identifier / SHA-256 | Source Location |
|---|---|---|
| Rust Compiler | `rustc 1.98.0 (88d9e12ae 2026-08-18)` | System toolchain |
| Cargo | `cargo 1.98.0 (797e8a9bc 2026-08-05)` | System toolchain |
| Flutter / Dart | `Flutter 3.47.7 (stable)` / `Dart 3.13.5` | `app/pubspec.yaml` (`2.0.4+4`) |
| Android NDK | `30.0.16248370` | `app/android/app/build.gradle.kts` |
| Core Crate | `bitchat2_core v0.1.0` (`CORE_BUILD_VERSION = "v2.0.5-phase5"`) | `core/Cargo.toml`, `core/src/api.rs:41` |
| Crypto & Storage Crates | `snow 0.9.6`, `ed25519-dalek 2.1.1`, `chacha20poly1305 0.10`, `blake3 1.5.5`, `rusqlite 0.32 (bundled)`, `zeroize 1.8.1`, `iroh 1.4.1`, `proptest 1.6.0` | `core/Cargo.toml` |
| Wire Envelope Version | `CURRENT_VERSION = 1`, `HEADER_LEN = 31`, `MAX_HOPS = 7`, `MAX_ENVELOPE_LEN = 512`, `PADDING_BUCKETS = [64, 128, 256, 512]` | `core/src/envelope.rs:6-10` |
| Fragment Header Version | `FRAGMENT_HEADER_LEN = 4` (`frame_id: u16, index: u8, total: u8`), `DEFAULT_MAX_PARTIAL_FRAMES = 64`, `DEFAULT_MAX_FRAME_BYTES = 2048` | `core/src/mesh/fragment.rs:9-12` |
| Voucher Wire Format | `VOUCH_VERSION = 1`, `SIGNED_VOUCHER_WIRE_LEN = 198`, `SIGNED_REVOCATION_WIRE_LEN = 189` | `core/src/trust/voucher.rs:22-27` |
| Wrapped DEK Format | Rust `BC2K` v1 (`69` bytes), Android `AndroidKeystoreProvider` `BC2K` v1 (`69` bytes; migrates v0 `60` bytes) | `core/src/storage/key.rs:25-27`, `AndroidKeystoreProvider.kt:43-48` |
| Outbox SQLite Schema | `outbox` table (`bitchat_outbox.db`), AAD `b"bitchat2-outbox-v1:"`, WAL mode | `core/src/storage/sqlite.rs:100-143` |
| Baseline ARM64 Native `.so` | `24,610,000` bytes — SHA-256 `4D82299C00E763E2A16412DC9CB9EA04D89651781B15F7C111746622CA8F92F4` | `app/android/app/src/main/jniLibs/arm64-v8a/libbitchat2_core.so` |
| Baseline Release APK *(gitignored)* | `82,951,605` bytes — SHA-256 `0901BE28E11086E2A140975067198E27C4D6152E59B4C247FD0BEF417244B549` | `app/build/app/outputs/flutter-apk/app-release.apk` |

---

## 2. Baseline Verification Snapshot (Commit `6638a79`)

| Command | Outcome | Counts / Notes |
|---|---|---|
| `cargo fmt --manifest-path core/Cargo.toml -- --check` | PASS | Verified on `6638a79` |
| `cargo clippy --manifest-path core/Cargo.toml --all-targets -- -D warnings` | PASS | 0 warnings |
| `cargo test --manifest-path core/Cargo.toml --all` | PASS | `170 / 170` passed (`146` unit + `24` across 6 E2E/ablation integration crates) |
| `cd app && flutter analyze` | PASS | `No issues found!` |
| `cd app && flutter test` | PASS | `5 / 5` passed (`keystore_fail_closed_test.dart`, `delivery_explanation_test.dart`, `widget_test.dart`) |

---

## 3. Gap & Remediation Audit (`PHASE_6.md` Task 0 Checklist vs Live HEAD `6638a79`)

Note: Phase 6's `PHASE_6.md` baseline text referenced commit `603b5f4` (`153` Rust tests, `1` widget test). Between `603b5f4` and `6638a79` (`adaptive-delivery`), several P0/P1 fixes landed. Below is the exact ground-truth status at HEAD `6638a79`:

| `PHASE_6.md` Task 0 Gap | Status at `6638a79` | Exact Production Location(s) | Remaining Phase 6 Action & Target Task |
|---|---|---|---|
| **1. `ResourceBudgetManager` not governing live forwarding** | **Partially Resolved** (`adaptive-delivery` Task 4 wired `check_transit_budget` in `MeshNode` and `GatewayRateLimiter` in `GatewayBridge`) | `core/src/mesh/relay.rs:93-138, 534-555, 665-680`; `core/src/gateway/bridge.rs`; `core/src/api.rs:196-201` | Meter actual fragmented wire bytes at small BLE MTU (note `FRAGMENT_HEADER_LEN = 4`), bound per-neighbor control-plane churn, add stale-telemetry conservative degradation (`>300s`), expand regression tests (**Task 3**). |
| **2. In-memory-only trust state & `gateway_start` missing that state** | **Partially Resolved / Still Unpersisted** (`Arc<RwLock<TrustStore>>` shared with `GatewayBridge`, but RAM-only!) | `core/src/api.rs:255-258, 1437-1443`; `core/src/trust/store.rs:50-59`; `core/src/gateway/config.rs:78-80` | Implement AEAD-encrypted SQLite trust store (`bitchat_trust.db`) persisting peer approvals, blocks, revocations, vouchers, voucher revocations, and gateway approvals; enforce fail-closed startup hydration (**Task 2**). |
| **3. BLE direct-message admission bypass** | **Partially Resolved** (`TrustPolicy::authorize` invoked in `poll_inbound`, returns early on `Deny`) | `core/src/api.rs:1188-1393`; `core/src/mesh/relay.rs:32, 451-525` | Separate immediate relay (`from_peer`) vs origin sender (`sender_short_id`), add `UnboundLink`/`ClaimedShortId`/`VerifiedPeer` states, gate direct RSSI binding strictly on `hop_count == 0`, separate end-user block vs immediate-relay disallow vs gateway revoke, emit structured rejection/pending-request events (**Task 1**). |
| **4. Synthetic full identities constructed from `ShortId`** | **Resolved in `adaptive-delivery`** (verified: zero fabricated `PeerId([short; 0..0])` sites remain) | `core/src/api.rs:304-325` (`resolve_verified_peer_id`) | Add explicit `PeerLinkIdentityState` tracking and collision/spoof/RSSI regression coverage in public-runtime suite (**Task 1**, **Task 6**). |
| **5. Hardcoded `gateway_relay_disabled` diagnostics** | **Resolved for relay flag in `adaptive-delivery`**, partial on path observation | `core/src/api.rs:819-823`, `core/src/gateway/bridge.rs` (`relay_disabled()`, `observed_path()`) | Separate gateway configured relay policy vs live observed path (`Direct`/`Relay`/`Mixed`/`Unknown`) + last observation timestamp (**Task 5**). |
| **6. Plaintext `FileKeyStore` root identity** | **STILL OPEN** (`identity.key` stored as 32-byte raw file!) | `core/src/identity.rs:279-324` (`FileKeyStore`); `core/src/api.rs:176-189` | Implement `ProtectedIdentityStore` (`BCID` v1 AEAD-wrapped `identity.enc`), crash-safe atomic migration preserving `PeerId`, typed recovery errors, Android backup exclusion XML (**Task 4**). |
| **7. APK/native artifact older than source** | **Fresh at `6638a79`**, must be refreshed after Phase 6 changes | `app/android/app/src/main/jniLibs/arm64-v8a/libbitchat2_core.so`, `app/build/app/outputs/flutter-apk/app-release.apk` | Rebuild `.so` and release APK after runtime/UI tasks + add CI/provenance commit/ABI checks (**Task 10**, **Task 11**, **Task 15**). |

---

## 4. Authoritative Owners & Pump Inventory

| Subsystem | Authoritative Owner (Source) | Notes / Duplication Status |
|---|---|---|
| **Runtime Singleton** | `BitchatCoreService` in `SERVICE: OnceLock<RwLock<Option<BitchatCoreService>>>` (`core/src/api.rs:118-144`), Tokio `RUNTIME: OnceLock<Option<Runtime>>` (`core/src/ffi.rs:8`) | **Gap for Task 5**: UI Flutter isolate (`MeshBridge._pumpTimer` every 50 ms at `app/lib/mesh_bridge.dart`) drives `pump_outbox()`, `poll_outbound()`, `poll_inbound()`, `poll_delivery_events()`, whereas `BitchatBleService.kt` owns OS foreground service lifecycle & GATT queues. Android `MainActivity.onDestroy()` stops BLE service and cancels Flutter timer. Task 5 adds lifecycle generations, idempotency, typed init errors, and single-owner coordination. |
| **Noise Sessions** | `PeerSessionManager` (`core/src/mesh/peer.rs`, owned inside `BitchatCoreService`) | Mutually verified Noise_XX + Ed25519 binding + `StatelessTransportState` + 128-packet replay window. |
| **Outbox & Retry Scheduler** | `OutboxManager` (`SqliteOutboxStore` + `MemoryOutboxStore`) + `DeliveryCoordinator` (`core/src/mesh/delivery.rs`) | Rehydrates pending Normal records on startup; bounded `3:1` EDF; single active `InFlight` per recipient. |
| **Trust & Consent** | `Arc<RwLock<TrustStore>>` (`core/src/trust/store.rs`), shared with `GatewayBridge` (`core/src/gateway/bridge.rs:69`) | Currently RAM-only until Task 2 adds `SqliteTrustStore` (`bitchat_trust.db`). `GatewayConfig` keeps endpoint-specific `peer_states`; Task 2 unifies persistence. |
| **Display UI Timers** | `app/lib/main.dart` (`400 ms` poll / `600 ms` dialog refresh) | Display-only; verified zero competing Dart retry timers (`MeshBridge._pumpTimer` is sole pump). |

---

## 5. C-ABI Export Inventory (`core/src/ffi.rs` — 43 Exports)

1. `bitchat_init`
2. `bitchat_init_with_key`
3. `bitchat_my_short_id`
4. `bitchat_my_peer_id`
5. `bitchat_send_broadcast`
6. `bitchat_send_to`
7. `bitchat_initiate_handshake`
8. `bitchat_is_session_ready`
9. `bitchat_on_peer_connected`
10. `bitchat_on_peer_disconnected`
11. `bitchat_on_frame_received`
12. `bitchat_poll_outbound`
13. `bitchat_poll_inbound`
14. `bitchat_gateway_start`
15. `bitchat_gateway_stop`
16. `bitchat_gateway_export_contact`
17. `bitchat_gateway_import_and_approve_contact`
18. `bitchat_gateway_connect_to_contact`
19. `bitchat_gateway_set_route`
20. `bitchat_gateway_get_stats`
21. `bitchat_gateway_get_routes`
22. `bitchat_send_message`
23. `bitchat_send_message_with_deadline`
24. `bitchat_cancel_message`
25. `bitchat_get_delivery_state`
26. `bitchat_poll_delivery_events`
27. `bitchat_get_diagnostics`
28. `bitchat_pump_outbox`
29. `bitchat_gateway_list_pending`
30. `bitchat_gateway_approve_peer`
31. `bitchat_gateway_revoke_or_block_peer`
32. `bitchat_trust_set_peer_state`
33. `bitchat_trust_issue_voucher`
34. `bitchat_trust_import_voucher`
35. `bitchat_update_resource_state`
36. `bitchat_set_relay_contribution_profile`
37. `bitchat_record_ble_rssi`
38. `bitchat_get_peer_proximity`
39. `bitchat_set_volatile_session`
40. `bitchat_get_volatile_state`
41. `bitchat_get_peer_id`
42. `bitchat_free_string`
43. `bitchat_free_bytes`
