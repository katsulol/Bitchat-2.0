# Bitchat 2.0 Progress & Verification Log

## Status Overview
- Cumulative Effort Progress: Phase 4 Complete; **Phase 5 Gate 0 & Part A Complete (Stopped at Part A Gate for Approval)**
- Current Milestone: **Phase 5 Part A Acceptance Gate**
- Branch: `phase-5-trust`
- Phase 5 Gate 0 Status: **100% Complete & Verified** (`0.1` Gateway Consent, `0.2` Conservative Coverage & Priority Quota, `0.3` BC2K Wrapped Key & Timestamp AAD, `0.4` Strict Hex & BLE Readiness, `0.5` Handshake/Retry Separation & Coalesced Wakeups, `0.6` 400-Run Evaluation Baseline)
- Phase 5 Part A Status: **100% Complete & Verified** (Tasks 1–7: Scoped Trust, One-Hop Vouchers, Gateway Trust Failover, Resource-Budgeted Relaying, Coarse Proximity, Volatile Chat, E2E Policy Tests & 400-Run Evaluation)
- Phase 5 Part B Status: **NOT STARTED (Awaiting Human Review & Approval of Part A)**


---

## Phase 4 Part A Task Breakdown

### [x] Task 0 — Baseline & Compatibility Audit
- **Status**: Simulated/Code Audit PASS
- **Artifacts**: [`docs/PHASE4_BASELINE.md`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/docs/PHASE4_BASELINE.md), [`docs/DELIVERY_PROTOCOL.md`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/docs/DELIVERY_PROTOCOL.md), [`docs/PRIVACY_POLICY.md`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/docs/PRIVACY_POLICY.md), ADR-0008 through ADR-0012 in [`docs/DECISIONS.md`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/docs/DECISIONS.md).
- **Audit Findings**: Documented negotiated GATT MTU (512 vs 23 fallback), separated packet ID (`[u8; 8]`) from application logical ID (`[u8; 16]`), recorded explicit authenticated bytes.

### [x] Task 1 — Mesh Reliability Carry-Over
- **Status**: Simulated PASS (300-run simulation suite)
- **Implementation**: [`core/src/mesh/policy.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/mesh/policy.rs) (`CoverageAwareFlood`, `NeighborTable`, bounded 30s TTL, stale coverage fallback).
- **Evaluation**: Partitioned bridge delivery improved from 60.2% (AdaptiveFlood) to **94.8%** (CoverageAware) while slashing transmissions by 66.3% (9.4 vs 27.9). Dense cluster duplicate transmissions cut by 88.8% vs Naive. Logged in [`docs/EVALUATION.md`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/docs/EVALUATION.md).

### [x] Task 2 — Session and Identity Hardening
- **Status**: Simulated Fault-Injection PASS
- **Implementation**: Defined `HandshakeFrame` (step 1..3, attempt ID, session context, payload) in [`core/src/crypto.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/crypto.rs). Handshake flight caching, deterministic tie-breaking, stale attempt rejection, and remote identity binding in [`core/src/mesh/peer.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/mesh/peer.rs).
- **Fault-Injection Suite**: Tested loss/duplication/reordering of every Noise XX flight, stale attempt rejection, wrong identity rejection, directional key isolation, and high forged nonces without sliding window advance.

### [x] Task 3 — Authenticated Gateway Routes
- **Status**: Simulated & Desktop Network PASS
- **Implementation**: Signed `RouteSnapshot` with canonical domain separation `b"bitchat2-route-snapshot-v1"`, bounds validation (max 128 recipients, max 3600s lease) in [`core/src/gateway/wire.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/gateway/wire.rs). Epoch and sequence monotonicity, local monotonic leases, and atomic empty withdrawal in [`core/src/gateway/routes.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/gateway/routes.rs).
- **Verification**: Verified via `test_phase3_four_node_e2e_mesh_over_real_iroh` with four real cryptographic identities and dual Iroh bridge endpoints. Reconnect/restart replay protection verified in `test_snapshot_reconnect_old_epoch_replay_rejected`.

### [x] Task 4 — Delivery Protocol and Acknowledgments
- **Status**: Simulated PASS
- **Implementation**: Authenticated `ApplicationFrame` (version 1, kind Message/Ack, logical ID `[u8; 16]`, sender/recipient binding, ciphertext body) in [`core/src/mesh/delivery.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/mesh/delivery.rs).
- **Invariants**: Strict monotonic state machine (`Queued -> InFlight -> {Delivered, Failed, Expired, Cancelled}`) with terminal state immutability. Authenticated ACKs generated solely by destination peer under established Noise session. Receiver deduplication keyed by `(authenticated_sender, logical_id)`.

### [x] Task 5 — Outbox and Persistence
- **Status**: Simulated & Disk-Level PASS
- **Implementation**: Abstract bounded `OutboxStore` in [`core/src/storage/`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/storage/).
  - `MemoryOutboxStore`: Heap-only volatile storage for Privacy Mode with strict zero-disk guarantee.
  - `SqliteOutboxStore`: Encrypted SQLite store for Normal Mode. Message body encrypted via ChaCha20-Poly1305 with CSPRNG 96-bit nonces. AAD binds schema version, key version, logical ID, sender, recipient, and mode.
  - `OutboxManager`: Enforces admission checks (queue full, payload limits, locked keystore, invalid recipient) and monotonic durable commits.
- **Verification**: Crash recovery from SQLite file, keystore locked rejection, AAD/ciphertext tamper rejection, expiry pruning, and zero-disk assertion for Privacy Mode.

### [x] Task 6 — Router and Transport Policy
- **Status**: Simulated PASS
- **Implementation**: Pure synchronous decision function in [`core/src/mesh/router.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/mesh/router.rs). Typed `RouteReason` codes.
- **Hierarchy**: Verified Direct BLE Neighbor -> Approved Iroh Gateway -> Bounded BLE Mesh Flood -> Queue Backoff. Decoupled control-plane routing from application readiness (`needs_handshake = true`). Enforced Privacy Mode relay blocking at transport capability layer (`iroh_endpoint_relay_disabled`).

### [x] Task 7 — Modes, Padding, and Gateway Fairness
- **Status**: Simulated PASS
- **Implementation**: Authenticated inner bucket padding (64, 128, 256, 512 bytes) with checked framing boundary limits in [`core/src/envelope.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/envelope.rs). Gateway rate limiter in [`core/src/gateway/fairness.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/gateway/fairness.rs) (64 KB/s global bandwidth limit, 8 KB/s per-peer bandwidth, 16 envelopes/min, 25% control traffic queue and bandwidth reservation to prevent starvation). Gateways default to OFF.

---

## Phase 4 Part B Platform Integration Breakdown

### [x] Platform Android Keystore Hardware Provider
- **Status**: Android Compilation PASS (`compileDebugKotlin` successful, 0 errors)
- **Implementation**: [`app/android/app/src/main/kotlin/com/bitchat/bitchat2/AndroidKeystoreProvider.kt`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/app/android/app/src/main/kotlin/com/bitchat/bitchat2/AndroidKeystoreProvider.kt)
  - Hardware-backed AES-GCM-256 KEK in `AndroidKeyStore` (`bitchat_outbox_kek`).
  - Generates/unwraps a 256-bit random DEK via AES/GCM/NoPadding with 12-byte CSPRNG IV.
  - Persists wrapped DEK ciphertext to app-private storage (`bitchat_dek.enc`).
  - Throws typed `KeystoreLockedException` if device/keystore is locked.
  - Excluded from cloud backups via `android:allowBackup="false"` in `AndroidManifest.xml`.
- **Service-Owned Runtime**: Service verifies Keystore DEK on `startMesh` directly within `BitchatBleService.kt` independently of Flutter UI lifecycle.

### [x] Public Runtime Daemon & Delivery API
- **Status**: Desktop Integration PASS (`test_phase4_delivery_e2e_lifecycle` passing)
- **Implementation**: [`core/src/api.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/api.rs) & [`core/src/ffi.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/ffi.rs)
  - `BitchatCoreService`: Manages encrypted SQLite store lifecycle with 32-byte hardware DEK.
  - APIs: `bitchat_init_with_key`, `bitchat_send_message`, `bitchat_get_delivery_state`, `bitchat_poll_delivery_events`, `bitchat_get_diagnostics`, and `bitchat_pump_outbox`.
  - Delivery Loop: Unpads authenticated `ApplicationFrame`, deduplicates via `InboxDeduplicator`, auto-emits Noise-authenticated ACKs, drains events monotonically.

### [x] Flutter UI Integration & Reactive Outbox
- **Status**: Static Analysis PASS (`flutter analyze`, 0 issues)
- **Implementation**: [`app/lib/core_ffi.dart`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/app/lib/core_ffi.dart), [`app/lib/mesh_bridge.dart`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/app/lib/mesh_bridge.dart), [`app/lib/main.dart`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/app/lib/main.dart)
  - MethodChannel retrieves platform DEK on startup and boots encrypted core.
  - Guarded non-overlapping outbox pump timer (`_isPumping`).
  - Stream-based delivery event bus (`_deliveryEventController`).
  - Reactive chat bubble delivery status badges: `Queued` (amber clock), `InFlight` (grey single check), `Delivered` (green double check), `Failed` / `Expired` (red alert).
  - Mode toggle: Normal vs Privacy (with direct-only shield badge and UI indicators).
  - Full Diagnostics bottom sheet: ShortId, PeerId, Queued, In Flight, Total Deliveries, Tx Total, Suppressed, Duplicates, Peers count, Gateway Active, Privacy Direct-Only Enforced, and Last Route Candidate.

---

## Verification Test Matrix

| Scope | Test Name / Suite | Target / Environment | Status |
| :--- | :--- | :--- | :--- |
| **Identity & Keys** | `identity::tests::*` (6 tests) | Rust Unit (Simulated) | **PASS** |
| **Envelope Codec** | `envelope::tests::*` (11 tests) | Rust Unit + Proptest | **PASS** |
| **Framing Boundaries** | `envelope::tests::test_pad_inner_payload_all_framing_boundaries` | Rust Unit (Simulated) | **PASS** |
| **Inner Padding** | `envelope::tests::test_pad_to_smallest_bucket` | Rust Unit (Simulated) | **PASS** |
| **Noise XX & Replay** | `crypto::tests::*` (11 tests) | Rust Unit (Simulated) | **PASS** |
| **Seen Cache** | `mesh::seen::tests::*` (6 tests) | Rust Unit (Simulated) | **PASS** |
| **Fragmentation** | `mesh::fragment::tests::*` (9 tests) | Rust Unit + Proptest | **PASS** |
| **Relay Engine** | `mesh::relay::tests::*` (7 tests) | Rust SimNetwork (Simulated) | **PASS** |
| **Policy & Flooding** | `mesh::policy::tests::*` (3 tests) | Rust SimNetwork (Simulated) | **PASS** |
| **Delivery & Ack** | `mesh::delivery::tests::*` (6 tests) | Rust Unit (Simulated) | **PASS** |
| **Peer Sessions** | `mesh::peer::tests::*` (8 tests) | Rust SimNetwork (Simulated) | **PASS** |
| **Router Decisions** | `mesh::router::tests::*` (6 tests) | Rust Pure Unit | **PASS** |
| **Outbox & AEAD** | `storage::tests::*` (8 tests) | Rust SQLite + Memory | **PASS** |
| **Gateway Wire** | `gateway::wire::tests::*` (9 tests) | Rust Unit (Simulated) | **PASS** |
| **Gateway Binding** | `gateway::identity_binding::tests::*` (5 tests) | Rust Unit (Simulated) | **PASS** |
| **Gateway Contact** | `gateway::contact::tests::*` (4 tests) | Rust Unit (Simulated) | **PASS** |
| **Gateway Routes** | `gateway::routes::tests::*` (6 tests) | Rust Unit (Simulated) | **PASS** |
| **Gateway Fairness** | `gateway::fairness::tests::*` (3 tests) | Rust Unit (Simulated) | **PASS** |
| **Simulated Link** | `transport::sim::tests::*` (4 tests) | Rust SimNetwork | **PASS** |
| **Bridge Transport** | `transport::bridge::tests::*` (2 tests) | Rust SimNetwork | **PASS** |
| **API Lifecycle** | `api::tests::*` (1 test) | Rust Unit (Simulated) | **PASS** |
| **End-to-End Mesh-Iroh** | `test_phase3_four_node_e2e_mesh_over_real_iroh` | Desktop Real Iroh Loopback | **PASS** |
| **Public Delivery E2E** | `test_phase4_delivery_e2e_lifecycle` | Desktop Multi-Node Integration | **PASS** |
| **Android Keystore Provider** | `compileDebugKotlin` | Gradle Android Build | **PASS** |
| **Flutter FFI & UI Binding** | `flutter analyze` | Dart Static Analyzer | **PASS** |

---

## Physical Field Test Matrix (Section 13)

| Test Scenario | Protocol & Target Configuration | Execution Status | Evidence / Notes |
| :--- | :--- | :--- | :--- |
| **1. Three-phone Line (Software Enforced)** | Nodes A-C blocked bi-directionally, B relays. Verify 1h hop count and delivery. Disable B and assert delivery stops. | **PENDING PHYSICAL** | Requires 3 physical phones. Software topology filter ready in BleService. |
| **2. Physical-Separation Line** | Nodes placed out of direct RF range with B midpoint. Verify link discovery & B removal control. | **PENDING PHYSICAL** | Physical RF attenuation / distance required. |
| **3. Cross-Network Iroh Tunnel** | Node B on Cellular LTE, Node C on Wi-Fi. Observe connection establishment, direct/relay path transition. | **PENDING PHYSICAL** | Desktop loopback PASS; physical cellular-to-WiFi pending field devices. |
| **4. Full 4-Role Offline Path** | Node A (BLE-only) -> B (BLE+Iroh) -> C (Iroh+BLE) -> D (BLE-only). Fresh Noise handshake, reply, zero broadcast export. | **PENDING PHYSICAL** | Verified in desktop integration test `test_phase3_four_node_e2e_mesh_over_real_iroh`. Physical 4-phone run pending. |
| **5. Receiver Unavailable & Recovery** | Receiver D offline. Sender A queues message in outbox. Bring D online. Verify pump delivery, authenticated ACK, and deduplication. | **PENDING PHYSICAL** | Verified in `test_phase4_delivery_e2e_lifecycle`. Physical app sleep/wake run pending. |
| **6. MTU, Unicode & Chunks** | Payloads across 23, 185, and 512 MTU boundaries. Unicode strings, multi-chunk fragmentation under loss. | **PENDING PHYSICAL** | Unit/property tests PASS (`test_pad_inner_payload_all_framing_boundaries`). |
| **7. Mode Checks & Zero-Disk Audit** | Verify Privacy mode generates no SQLite disk artifacts. Audit app private directory and backups (`allowBackup="false"`). | **PENDING PHYSICAL** | SQLite unit tests PASS (`test_send_to_privacy_mode_zero_disk_guarantee`). Physical flash inspection pending. |
| **8. Battery & Radio Profiling** | Repeated matched runs with Gateway ON vs OFF. Controlled traffic, fixed screen state, initial charge. | **PENDING PHYSICAL** | Power telemetry requires battery historian on target hardware. |

---

## Verification Commands & Sign-off

- **Rust Core**:
  - `cargo fmt --check`: Clean (exit code 0).
  - `cargo clippy --all-targets -- -D warnings`: Clean (exit code 0, 0 warnings).
  - `cargo test --all`: 122 tests passed, 0 failed.
- **Android Platform**:
  - `./gradlew compileDebugKotlin`: BUILD SUCCESSFUL (24 actionable tasks: 6 executed, 18 up-to-date).
- **Flutter Framework**:
  - `flutter analyze`: No issues found! (0 errors, 0 warnings).
- **Crypto Registry**: [`docs/CRYPTO_REVIEW.md`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/docs/CRYPTO_REVIEW.md) updated with `AndroidKeystoreProvider.kt`.

---

## Phase 5 Gate 0 & Part A Verification Matrix (`phase-5-trust`)

| Scope | Test / Artifact | Target / Environment | Status |
| :--- | :--- | :--- | :--- |
| **Gate 0.1 — Gateway Consent & Pairing** | `gateway::tests::*` (35 tests incl. stranger reject, single-use invite, replay reject) | Rust Unit (Simulated) | **PASS** |
| **Gate 0.2 — Coverage, Clock/RNG, Priority Quota** | `mesh::policy::tests::*` (7 tests incl. inferred vs observed, priority rate limit) | Rust SimNetwork (Simulated) | **PASS** |
| **Gate 0.3 — BC2K Envelope & SQLite AAD** | `storage::tests::*` (10 tests incl. fail-closed recovery, timestamp AAD binding) | Rust SQLite + Memory | **PASS** |
| **Gate 0.4 — Strict Hex & BLE Readiness** | `identity::tests::*` (7 tests incl. `from_hex_exact`), `BitchatBleService.kt` | Rust Unit + Kotlin Service | **PASS** |
| **Gate 0.5 — Handshake vs Retry & Coalescing** | `mesh::delivery::tests::*` (7 tests incl. `NoRouteAvailable` stays `Queued`) | Rust Unit (Simulated) | **PASS** |
| **Gate 0.6 — Baseline & Evaluation Harness** | `core/examples/evaluate.rs`, `evaluation_results.csv`, `docs/PHASE5_GATE0_REVIEW.md` | 400-Run SimNetwork Suite | **PASS** |
| **Part A Task 1 & 2 — Trust & Vouchers** | `trust::tests::*` (6 tests incl. `Deny > Allow`, non-transitive vouch, caps, cascade) | Rust SQLite + Memory | **PASS** |
| **Part A Task 3 — Gateway Trust & Failover** | `gateway::bridge::tests::test_gateway_trust_integration_and_atomic_failover` | Rust Unit (Simulated) | **PASS** |
| **Part A Task 4 — Resource-Budgeted Relaying** | `mesh::resource::tests::*` (3 tests incl. hysteresis & 1:4:8 token buckets) | Rust Unit (Simulated) | **PASS** |
| **Part A Task 5 — Coarse BLE Proximity** | `mesh::proximity::tests::*` (2 tests incl. median window, hysteresis, 30s decay) | Rust Unit (Simulated) | **PASS** |
| **Part A Task 6 — Volatile Chat Negotiation** | `storage::volatile::tests::*` (2 tests incl. downgrade reject & zeroizing expiry) | Rust Unit (Simulated) | **PASS** |
| **Part A Task 7 — Public-Runtime E2E Suite** | `test_phase5_policy_e2e` (5 integration tests) | Desktop Multi-Node Integration | **PASS** |
| **Part A Evaluation & Gate Docs** | [`docs/PHASE5_EVALUATION.md`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/docs/PHASE5_EVALUATION.md), [`docs/PART_A_ACCEPTANCE_REVIEW.md`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/docs/PART_A_ACCEPTANCE_REVIEW.md) | 400-Run Benchmark & Docs | **PASS** |

