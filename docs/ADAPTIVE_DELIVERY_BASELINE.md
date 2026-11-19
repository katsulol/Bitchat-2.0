# Adaptive Delivery — Task 0 Source Audit & Baseline

- **Base Commit**: `603b5f47d936db24c35968c29da67df36fa4b118` (`main` / `phase-5-trust`)
- **Audit Branch**: `adaptive-delivery` (local branch created from `603b5f4`)
- **Audit Date**: `2026-11-19`
- **Specification Reference**: `ADAPTIVE_DELIVERY_FEATURES.md` *(note: currently located at workspace root rather than `docs/ADAPTIVE_DELIVERY_FEATURES.md`)*

---

## 1. Status Matrix (Honest Baseline Separation)

| Feature / Subsystem | Implemented | Integrated (Public Runtime) | Simulator-Tested | Desktop-Tested | Physical-Tested | Notes |
|---|:---:|:---:|:---:|:---:|:---:|---|
| **Task 1: Automatic Alternate-Gateway Retry** | **No** | **No** | **No** | **No** | **No** | Planned (Task 1) |
| **Task 2: Deadline-Aware Messages & Scheduling** | **No** | **No** | **No** | **No** | **No** | Planned (Task 2); partial TTL bounds exist in `OutboxManager` |
| **Task 3: Explain Delivery Progress (UI Reasons)** | **No** | **No** | **No** | **No** | **No** | Planned (Task 3); raw enum strings (`Queued`/`InFlight`/`Delivered`/`Expired`/`Failed`) only |
| **Task 4: Relay Contribution Controls (Wired Live)** | **Module Only** | **No** | **Partial** (`Coverage+ResMin` sim) | **Unit Only** | **No** | `ResourceBudgetManager` exists in `resource.rs`, not wired into `MeshNode` in `api.rs` |
| **Baseline: BLE Mesh + Adaptive/Coverage Flood** | **Yes** | **Yes** | **Yes** (400 runs) | **Yes** | **Yes** (2–3 Android phones) | `CoverageAwareFlood` active in `api.rs:161` |
| **Baseline: Noise_XX + Encrypted SQLite / Privacy RAM** | **Yes** | **Partial** (B1–B4 gaps) | **No** | **Yes** | **Yes** | See Section 4 blockers B1, B2, B4 |
| **Baseline: Iroh Gateway + Single-Use Pairing Invite** | **Yes** | **Partial** (B3, B5 gaps) | **No** | **Yes** | **Yes** (2 Android phones) | `TrustStore` unseeded at `gateway_start()`; `GatewayRateLimiter` unwired |
| **Baseline: Scoped 1-Hop Vouchers & Volatile Chat** | **Yes** | **Partial** (B2, B3 gaps) | **No** | **Yes** | **Partial** (2 phones; no real Phone C) | Synthetic `PeerId` fallback in `api.rs`; `TrustStore` in-memory only |

---

## 2. Component Ownership & Call-Path Map (Exact Source Locations)

| Component / Role | Primary Location(s) | Current Runtime Behavior & Ownership Details |
|---|---|---|
| **Authoritative Runtime Owner** | `core/src/api.rs:94-116` (`BitchatCoreService`, `SERVICE: OnceLock<RwLock<Option<BitchatCoreService>>>`)<br>`core/src/ffi.rs` (`RUNTIME: OnceLock<tokio::runtime::Runtime>`) | Single global `RwLock<Option<BitchatCoreService>>` accessed synchronously from 40 `#[no_mangle] pub unsafe extern "C"` FFI functions blocking on Tokio `RUNTIME`. |
| **Public Send APIs** | **Primary reliable**: `api::send_message` (`core/src/api.rs:265-368`) ← `bitchat_send_message` (`core/src/ffi.rs`) ← `MeshBridge.sendMessage` (`app/lib/mesh_bridge.dart:289-324`) ← `_sendMessage` (`app/lib/main.dart:98-108`)<br>**Legacy E2E (no outbox)**: `api::send_to` (`core/src/api.rs:232-259`, `bitchat_send_to`)<br>**Public broadcast**: `api::send_broadcast` (`core/src/api.rs:218-229`) | `_sendMessage` calls `MeshBridge.sendMessage` (`0 = Normal`, `1 = Privacy`). `send_broadcast` uses `ShortId::default()` and stays local to BLE (`core/src/mesh/relay.rs:222`, `core/src/gateway/bridge.rs:709-711`). `send_to` has no UI callers in `main.dart`. |
| **Route Candidate Table** | `GatewayRouteTable` (`core/src/gateway/routes.rs:19-90`)<br>`RouteSnapshot` / `RouteRecipient` (`core/src/gateway/wire.rs:83-162`) | `routes: HashMap<ShortId, Vec<RouteEntry>>` and `gateway_state: HashMap<EndpointId, (u64, u64)>`. `lookup_authorized_at` (`routes.rs:75-90`) returns at most **one** `EndpointId` (`max_by_key(\|e\| e.sequence)`). |
| **Pure Router & Packet Dispatch** | `Router::decide(&RouteContext)` (`core/src/mesh/router.rs:103-185`)<br>`MeshNode::send_envelope` (`core/src/mesh/relay.rs:179-228`)<br>`GatewayBridge::forward_mesh_packet` (`core/src/gateway/bridge.rs:703-777`) | `Router::decide` currently has **0 production callers** (invoked only in `router.rs` unit tests). `MeshNode::send_envelope` dispatches over BLE unicast/broadcast **and** unconditionally calls `bridge.forward_mesh_packet(&env, encoded)` (`relay.rs:221-225`) whenever `gateway_bridge` is set. |
| **OutboxManager & Protected Stores** | `OutboxManager` (`core/src/storage/mod.rs:30-189`)<br>`OutboxStore`, `MessageMode`, `OutboxLimits` (`core/src/storage/outbox.rs:10-154`)<br>`SqliteOutboxStore` (`core/src/storage/sqlite.rs:38-160`)<br>`MemoryOutboxStore` (`core/src/storage/memory.rs`)<br>`KeyProvider` / `BC2K` (`core/src/storage/key.rs`) + `AndroidKeystoreProvider.kt` | Normal mode commits ChaCha20-Poly1305 encrypted records to `bitchat_outbox.db` with 103-byte AAD binding `b"bitchat2-outbox-v1:" \|\| key_version \|\| logical_id \|\| sender \|\| recipient \|\| mode \|\| created_at \|\| expires_at` (`sqlite.rs:124-143`). Privacy mode writes strictly to heap `MemoryOutboxStore`. Outbox DEK is wrapped by Android Keystore AES-GCM-256 KEK (`BC2K` v1). |
| **DeliveryCoordinator** | `DeliveryCoordinator` (`core/src/mesh/delivery.rs:295-541`)<br>`OutgoingRecord` (`delivery.rs:266-280`)<br>`ApplicationFrame` (`delivery.rs:73-209`)<br>Pumped by `api::pump_outbox` (`core/src/api.rs:439-528`) | Tracks `Queued` → `InFlight` → `Delivered \| Failed \| Expired \| Cancelled`, `attempts`, `max_attempts`, `handshake_attempts`, `expires_at: Instant`, `next_attempt_at: Instant`. Driven periodically by `api::pump_outbox`. |
| **Session Manager & Packet Generator** | `PeerSessionManager` (`core/src/mesh/peer.rs`; `send_encrypted` at `peer.rs:371-390`)<br>Stateless Noise_XX (`core/src/crypto.rs`)<br>`compute_msg_id` (`core/src/envelope.rs:92-103`) | `Noise_XX_25519_ChaChaPoly_BLAKE2s` with domain-separated static-key signature (`b"bitchat2/noise-static/v1"`), explicit 8-byte AEAD nonces, and 128-packet sliding replay window. Each `MeshNode::send_envelope` increments `origin_counter` and computes a fresh 8-byte wire `msg_id`. |
| **Receiver Deduplication** | `InboxDeduplicator` (`core/src/mesh/delivery.rs:213-263`)<br>Instantiated at `core/src/api.rs:174` (`capacity = 10_000`, `ttl = 86_400s`) | In-memory bounded LRU map keyed by `(PeerId, LogicalMessageId)` returning `DedupStatus::New` or `DedupStatus::Duplicate` within 24h TTL; not persisted across receiver process restarts. |
| **Flutter State Events & Pump** | `api::poll_delivery_events` (`core/src/api.rs:395-400`) ← `bitchat_poll_delivery_events` (`ffi.rs`) ← `MeshBridge._drainDeliveryEvents` (`app/lib/mesh_bridge.dart`) → `_deliveryEventController` → `main.dart:75-83` | `MeshBridge._pumpTimer` (`app/lib/mesh_bridge.dart:235-246`) runs every **50 ms**, calling `_drainOutbound()`, `_drainInbound()`, `_drainDeliveryEvents()`, and `CoreFfi.instance.pumpOutbox()`. UI timers in `main.dart:85` (400 ms badge poll) and `main.dart:818` (600 ms gateway modal refresh) are display-only. |
| **Android Native Service Ownership** | `BitchatBleService.kt` (Android `connectedDevice` Foreground Service)<br>`MainActivity.kt` (`MethodChannel("com.bitchat.mesh/ble")`) | Kotlin service owns dual-role BLE GATT Server/Client, live `BatteryManager` / `PowerManager` telemetry, and RSSI callbacks. `MainActivity` provides storage path, Keystore DEK, and `FLAG_SECURE`. Rust `BitchatCoreService` and its 50 ms pump run inside the Flutter isolate. |

---

## 3. Targeted Task 0 Verification Questions (Source-Backed Answers)

### Q1. Logical ID vs Packet ID Distinction
- **Status**: **VERIFIED**
- **Evidence**: `OutboxManager::send_to` (`core/src/storage/mod.rs:96-97`) generates a 16-byte `logical_id` once via `rand::thread_rng().fill_bytes(&mut logical_id)`, which stays fixed across retries inside `ApplicationFrame` (`core/src/mesh/delivery.rs:73-140`) and under Noise AEAD encryption. On every initial send and retry (`core/src/api.rs:343-346, 473-476`), `PeerSessionManager::send_encrypted` (`core/src/mesh/peer.rs:371-390`) generates a fresh Noise 8-byte nonce and `MeshNode::send_envelope` (`core/src/mesh/relay.rs:186-187`) increments `origin_counter` to compute a fresh 8-byte envelope `msg_id` via `compute_msg_id` (`core/src/envelope.rs:92-103`).

### Q2. Protected Recipient ACK Behavior
- **Status**: **VERIFIED IN `delivery.rs`, BROKEN IN `api::poll_inbound` (P0 BLOCKER B1)**
- **Evidence**:
  - `DeliveryCoordinator::handle_ack` (`core/src/mesh/delivery.rs:428-470`) properly checks `ack.kind == Ack`, `ack.sender_peer_id == *authenticated_sender`, `ack.recipient_peer_id == self.local_peer_id`, `record.recipient_peer_id == *authenticated_sender`, and `!record.state.is_terminal()`.
  - **Runtime Defect**: In `core/src/api.rs:783-795`, `poll_inbound()` passes unverified `&app_frame.sender_peer_id` as the `authenticated_sender` parameter (`handle_ack(&app_frame, &app_frame.sender_peer_id)` — tautologically always equal to itself, instead of `&decrypted.remote_peer_id`), discards the `Result` with `let _ = ...`, and unconditionally calls `svc.outbox_mgr.mark_delivered(&app_frame.logical_id)` and pushes `"Delivered"` to `svc.delivery_events` even if `handle_ack` returned `Err(...)` or `Ok(false)`.
  - Additionally, `api::poll_inbound` (`core/src/api.rs:696-782`) does not validate `app_frame.sender_peer_id == decrypted.remote_peer_id` or `app_frame.recipient_peer_id == svc.node.identity().peer_id()` for inbound `ApplicationFrameKind::Message` frames.

### Q3. Multiple Approved Gateway Routes for One Full Recipient Identity
- **Status**: **NO (Partial — `ShortId`-only single-winner lookup)**
- **Evidence**:
  - `GatewayRouteTable` (`core/src/gateway/routes.rs:20`) already holds `HashMap<ShortId, Vec<RouteEntry>>`, allowing multiple gateway `EndpointId`s per 8-byte `ShortId` in storage.
  - However: (a) routes are keyed by 8-byte `ShortId`, not full 32-byte `PeerId`; (b) `announce_local_peers` (`core/src/gateway/bridge.rs:805-809`) sets `public_key_hex: None` for local BLE neighbors; (c) `RouteSync` ingestion (`core/src/gateway/bridge.rs:643-649`) ignores `r.public_key_hex`; (d) `lookup_authorized_at` (`core/src/gateway/routes.rs:75-90`) returns only a single `max_by_key(|e| e.sequence)` gateway with no candidate enumeration or per-round exclusion filter; and (e) `MeshNode::send_envelope` (`relay.rs:221-225`) and `GatewayBridge::forward_mesh_packet` (`bridge.rs:703-777`) accept no caller-chosen gateway `EndpointId` and fall back only on internal lookup.

### Q4. Independent ACK Timeout vs Outbox Expiry
- **Status**: **PARTIAL**
- **Evidence**:
  - `OutgoingRecord.expires_at: Instant` (`core/src/mesh/delivery.rs:272`) acts as the hard monotonic stopping deadline (`DeliveryAction::Expired` at `delivery.rs:526-528`), while `OutgoingRecord.next_attempt_at: Instant` (`delivery.rs:277`) triggers `DeliveryAction::ReadyToRetry` (`delivery.rs:534-536`).
  - **Gaps**: (a) There is no explicit separation between an ACK-wait timer for Alternate-Gateway switching and backoff scheduling; (b) `api::send_message` (`core/src/api.rs:349`) sets the first `InFlight` wait window to only `500 ms` (too short for Iroh QUIC or multi-hop BLE ACKs, causing unnecessary retries), followed by `(1000 * 2^attempts.min(4)).min(15000)` (`api.rs:477`); (c) `api::send_message` (`api.rs:323`) hardcodes `max_attempts: 12`, ignoring `MessageMode::max_attempts()` (`10` Normal / `5` Privacy in `outbox.rs:35-40`); (d) `api::pump_outbox` (`api.rs:468`) hardcodes `Duration::from_secs(300)` instead of remaining record TTL; and (e) `DeliveryCoordinator` is empty after restart (`api.rs:173`) because pending records in `SqliteOutboxStore` are never rehydrated on `init_with_key()`.

### Q5. Native / Service Ownership & Second Dart Retry Pump Check
- **Status**: **VERIFIED — No second Dart retry pump, but UI isolate owns 50 ms pump**
- **Evidence**:
  - `app/lib/main.dart` only listens to `_bridge.deliveryEvents` (`main.dart:75-83`) and renders state; Dart timers at `main.dart:85` (`400 ms`) and `main.dart:818` (`600 ms`) refresh UI state only and do not retry messages.
  - `MeshBridge._pumpTimer` (`app/lib/mesh_bridge.dart:235-246`) runs every **50 ms** and calls `CoreFfi.instance.pumpOutbox()`. Because `BitchatCoreService` is initialized and pumped inside the Flutter UI isolate while `BitchatBleService.kt` runs as an Android Foreground Service, OS destruction of the Activity pauses retries until the Flutter engine/UI resumes (acceptable if documented as a known limitation, or relocated in future Android hardening).

---

## 4. Source-Backed Security Blockers & Prerequisite Gap Checklist

| ID | Severity | Exact Location(s) | Verified Gap / Risk | Target Phase / Task |
|---|---|---|---|---|
| **B1** | **P0 Security Blocker** | `core/src/api.rs:783-795`<br>`core/src/api.rs:696-782` | `poll_inbound` passes unverified `&app_frame.sender_peer_id` to `delivery_coord.handle_ack`, discards the `Result`, and unconditionally marks `outbox_mgr` + emits `"Delivered"`. Inbound `Message` frames also omit verifying `app_frame.sender_peer_id == decrypted.remote_peer_id` and `app_frame.recipient_peer_id == local_peer_id`. Enables forged `Delivered` state and identity spoofing inside encrypted sessions. | **Prereq / Task 1** |
| **B2** | **P0 Security Blocker** | `core/src/api.rs:284-292`<br>`core/src/api.rs:1365-1374, 1392-1400, 1419-1427, 1504-1512` | `send_message` (and proximity/volatile APIs) fabricate a synthetic zero-padded `PeerId([short_id; 0..0])` when no Noise session exists yet. Violates `§Current-source prerequisites` ("A synthetic full identity derived from a ShortId must not authorize or identify a recipient"), corrupts `DeliveryCoordinator` / `OutboxManager` recipient identity checks (`handle_ack` will reject real peer ACKs), and violates `PROXIMITY_LIMITATIONS.md` §2.2. | **Prereq / Task 1** |
| **B3** | **P0 Security Blocker** | `core/src/api.rs:187`<br>`core/src/api.rs:859`<br>`core/src/api.rs:641-828` | `TrustStore` is in-memory only (approvals/blocks/vouchers lost on restart), `gateway_start()` instantiates a blank `TrustStore` inside `GatewayBridge` instead of sharing/syncing `svc.trust_store`, and BLE direct-message admission in `poll_inbound()` does not invoke `TrustPolicy::authorize`. | **Prereq / Task 1** |
| **B4** | **P0 Security Blocker** | `app/lib/mesh_bridge.dart:192-205` | Swallows `getPlatformDek` exceptions (`KEYSTORE_LOCKED` / `KEYSTORE_ERROR`) and silently falls back to `initCore(storagePath)` with an ephemeral random DEK, breaking fail-closed Keystore recovery when `bitchat_outbox.db` exists. | **Prereq / Task 2** |
| **B5** | **P1 Functional Blocker** | `core/src/api.rs:161, 1299-1351`<br>`core/src/gateway/bridge.rs`<br>`core/src/gateway/fairness.rs:37` | `ResourceBudgetManager` / `ResourceAwareRelayPolicy` (`core/src/mesh/resource.rs:396`) is **never wired** into live `MeshNode` in `api.rs` (`api.rs:161` uses bare `CoverageAwareFlood::default()`); `GatewayRateLimiter` (`fairness.rs:37`) is **never wired** into live `GatewayBridge`. Resource controls currently change Diagnostics text only. | **Task 4** *(prereq for honest Task 1 budgets)* |
| **B6** | **P1 Functional Blocker** | `core/src/gateway/routes.rs:20, 75-90`<br>`core/src/gateway/bridge.rs:643-649, 703-777, 805-809`<br>`core/src/mesh/relay.rs:179-228`<br>`core/src/api.rs:265-368, 439-528` | Gateway routes keyed only by `ShortId` (`public_key_hex` dropped); `lookup_authorized_at` returns single gateway without candidate list/exclusion; `Router::decide` unused in `api.rs`; `MeshNode`/`GatewayBridge` lack targeted per-gateway forwarding; no attempted-gateway tracking or per-recipient single-active-attempt guard; `pump_outbox` (`api.rs:488`) checks `gateway_bridge.is_some()` rather than verified authorized route existence. | **Task 1** |
| **B7** | **P1 Functional Blocker** | `core/src/api.rs:173, 323, 468, 505`<br>`core/src/storage/outbox.rs:91-93, 121-154`<br>`core/src/storage/mod.rs:91-93` | Missing restart rehydration of `SqliteOutboxStore` pending records into `DeliveryCoordinator`; `OutboxStore` lacks `mark_expired` (`pump_outbox` records `mark_failed` on expiry); `OutboxManager::send_to` silently clamps over-long `ttl_override` instead of rejecting invalid presets; `pump_outbox` hardcodes `300s` frame TTL on retry; `send_message` hardcodes `max_attempts: 12`. | **Task 1 & Task 2** |
| **B8** | **P1 Functional Blocker** | `core/src/api.rs:360, 395-400, 423` | `get_diagnostics()` hardcodes `gateway_relay_disabled: true` regardless of actual Iroh `RelayMode`; `send_message` hardcodes `DirectBle` in `last_route_candidate` even before routing; `DeliveryEventRecord` lacks typed `RouteReason`, attempt count, deadline, and observed transport (`BLE`/`Iroh Direct`/`Iroh Relay`/`Unknown`). | **Task 3** |
| **B9** | **P2 Build & Doc Alignment** | `app/android/app/src/main/jniLibs/arm64-v8a/libbitchat2_core.so`<br>`README.md`, `docs/DECISIONS.md`, `docs/CRYPTO_REVIEW.md`, `docs/EVALUATION.md`, `docs/PRIVACY_POLICY.md`, `docs/THREAT_MODEL.md`, `docs/ARCHITECTURE.md` | Prebuilt `libbitchat2_core.so` predates commit `603b5f4`'s `api.rs` edit (`verified_peer_id`); docs contain drifted terminology/constants (`Immediate/Near/Far` meters in `README.md` vs `Nearby/Further/Unknown` in code; `evaluate` 300-run vs 400-run CSV); `identity.key` uses plaintext `FileKeyStore`. | **Continuous / Task 5** |

---

## 5. Baseline Verification Results & Explicit Limitations

### Fresh Automated Suite (Verified 2026-11-19 on commit `603b5f4`)
- `cargo fmt --check` → **PASS** (exit `0`)
- `cargo clippy --all-targets -- -D warnings` → **PASS** (exit `0`, 0 warnings)
- `cargo test --all` → **PASS** (`153 / 153`: `146` unit tests + `1` `test_phase3_mesh_iroh_e2e` + `1` `test_phase4_delivery_e2e` + `5` `test_phase5_policy_e2e`, `0` doc-tests)
- `cd app && flutter analyze` → **PASS** (`No issues found!`)
- `cd app && flutter test` → **PASS** (`1 / 1`, `BitchatApp smoke test`)

### Known Baseline Limitations
1. **Unverified Multi-Gateway Physical Topology**: Full offline-sender → Gateway 1 + Gateway 2 → offline-receiver path requires 4 physical Android BLE devices; current hardware verification covered 2–3 devices under Phase 4/5 single-path tests only.
2. **Prebuilt ARM64 Native Binary Lag**: `libbitchat2_core.so` must be recompiled for `aarch64-linux-android` before subsequent Android hardware validation.
3. **Companion Plan Reference**: `ADAPTIVE_DELIVERY_FEATURES.md` §Purpose references `PHASE_6.md` alongside this plan, which is not yet present in the workspace; Task 0–5 sequencing proceeds strictly per `ADAPTIVE_DELIVERY_FEATURES.md` ("one numbered task at a time").
4. **Desktop/Simulation Boundary**: Existing 400-run `SimNetwork` and Tokio desktop E2E tests do not model Android OS Activity lifecycle suspension, BLE GATT congestion, or live Iroh DERP vs direct path migration.
