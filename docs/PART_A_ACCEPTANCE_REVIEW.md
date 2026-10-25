# Phase 4 Part A Acceptance Review & Verification Audit

**Status**: HARD STOP FOR HUMAN REVIEW  
**Audit Target**: Section 11 of `PHASE_4_REVISED.md`  
**Branch**: `phase-4-delivery`  
**Latest Verification Commit**: `1e27fbe`  
**TestSuite Summary**: 122 passing tests (120 unit/simulated in `core` + 1 Iroh 4-node desktop integration in `test_phase3_mesh_iroh_e2e` + 1 Phase 4 delivery E2E domain integration in `test_phase4_delivery_e2e`), 0 failures, 0 warnings.

---

## 1. Traceability & Implementation Status Matrix

Each requirement from Part A (Tasks 0 through 7) is classified into:
- **`implemented and tested`**: Code exists, covered by automated unit/property/integration tests.
- **`implemented but untested`**: Code exists, but automated tests are pending.
- **`deferred to Part B`**: Explicitly postponed until Part B authorization (platform keystore, Flutter UI, physical devices, public runtime daemon).
- **`missing`**: Not implemented.

| Requirement Area | Detailed Specification | Source Location | Exact Test Name | Status |
| :--- | :--- | :--- | :--- | :--- |
| **Discrete Event Timer Advancement** | `step_at(now)` advances discrete time without sleeping on wall-clock | [`core/src/mesh/relay.rs#L274-L290`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/mesh/relay.rs#L274-L290) | `mesh::relay::tests::test_adaptive_flood_suppression_in_dense_cluster` | **implemented and tested** |
| **Non-Blocking Transport Semantics** | `Transport::try_recv()` returns `Ok(false)` immediately when queue empty | [`core/src/transport/mod.rs#L24-L25`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/transport/mod.rs#L24-L25)<br>[`core/src/transport/sim.rs#L231-L238`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/transport/sim.rs#L231-L238)<br>[`core/src/transport/bridge.rs#L164-L171`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/transport/bridge.rs#L164-L171) | `mesh::relay::tests::test_adaptive_flood_suppression_in_dense_cluster` | **implemented and tested** |
| **Iteration Budget & Loop Safety** | Hard 10,000 step iteration cap and 5s test timeout on all drain loops | [`core/src/mesh/relay.rs#L644-L650`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/mesh/relay.rs#L644-L650)<br>[`core/src/mesh/relay.rs#L834-L842`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/mesh/relay.rs#L834-L842)<br>[`core/src/mesh/relay.rs#L906-L913`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/mesh/relay.rs#L906-L913) | `mesh::relay::tests::test_ring_topology_terminates`<br>`mesh::relay::tests::test_adaptive_flood_priority_messages_never_suppressed` | **implemented and tested** |
| **Coverage-Aware Relaying** | Bounded local neighbor observation table, stale fallback, suppression avoidance | [`core/src/mesh/policy.rs#L120-L470`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/mesh/policy.rs#L120-L470) | `mesh::policy::tests::test_coverage_aware_prevents_partitioned_bridge_suppression`<br>`mesh::policy::tests::test_coverage_aware_suppression_in_dense_cluster`<br>`mesh::policy::tests::test_stale_coverage_falls_back_to_forwarding` | **implemented and tested** |
| **Benchmark Baseline Reproduction** | Naive vs Adaptive vs CoverageAware reproduction across 5 topologies | [`core/examples/evaluate.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/examples/evaluate.rs) | `cargo run --example evaluate` (300 simulation runs) | **implemented and tested** |
| **Handshake Frame Codec** | Versioned `HandshakeFrame` (step 1..3, attempt ID, session context, payload) | [`core/src/crypto.rs#L40-L115`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/crypto.rs#L40-L115) | `crypto::tests::test_handshake_frame_encoding_and_validation` | **implemented and tested** |
| **Session Collision & Tie-Breaking** | Deterministic ShortId collision handling, flight caching, stale rejection | [`core/src/mesh/peer.rs#L140-L320`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/mesh/peer.rs#L140-L320) | `mesh::peer::tests::test_simultaneous_handshake_collision`<br>`mesh::peer::tests::test_handshake_loss_and_flight_caching` | **implemented and tested** |
| **Strict Remote Identity Binding** | Session mapping updated only after remote identity verification | [`core/src/mesh/peer.rs#L220-L260`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/mesh/peer.rs#L220-L260) | `mesh::peer::tests::test_wrong_remote_identity_rejected` | **implemented and tested** |
| **Signed RouteSnapshots** | Domain separation `b"bitchat2/gateway-route-snapshot/v1"`, max 128 recipients, 3600s lease | [`core/src/gateway/wire.rs#L73-L148`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/gateway/wire.rs#L73-L148) | `gateway::wire::tests::test_route_snapshot_bounds_validation`<br>`gateway::wire::tests::test_route_sync_frame_roundtrip` | **implemented and tested** |
| **Epoch & Sequence Monotonicity** | Monotonic epoch/sequence validation, sequence overflow restart rejection | [`core/src/gateway/routes.rs#L78-L122`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/gateway/routes.rs#L78-L122) | `gateway::routes::tests::test_snapshot_epoch_and_sequence_monotonicity`<br>`gateway::routes::tests::test_snapshot_sequence_overflow_rejected` | **implemented and tested** |
| **Reconnect Replay Protection** | Gateway state retained across disconnects; old epochs strictly rejected | [`core/src/gateway/routes.rs#L146-L154`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/gateway/routes.rs#L146-L154)<br>[`core/src/gateway/bridge.rs#L237-L248`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/gateway/bridge.rs#L237-L248) | `gateway::routes::tests::test_snapshot_reconnect_old_epoch_replay_rejected` | **implemented and tested** |
| **Bitchat-to-Iroh Identity Binding** | Ed25519 signature binding Bitchat `PeerId` to Iroh `EndpointId` | [`core/src/gateway/identity_binding.rs#L26-L100`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/gateway/identity_binding.rs#L26-L100) | `gateway::identity_binding::tests::test_valid_binding_verification`<br>`gateway::identity_binding::tests::test_mismatched_endpoint_key_rejected`<br>`gateway::identity_binding::tests::test_tampered_signature_rejected` | **implemented and tested** |
| **Gateway Author Binding** | Author signature verified against connection-bound `PeerId` | [`core/src/gateway/bridge.rs#L325-L345`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/gateway/bridge.rs#L325-L345) | `test_phase3_four_node_e2e_mesh_over_real_iroh` | **implemented and tested** |
| **Logical vs Packet ID Separation** | Logical ID (`[u8; 16]`) invariant across attempts; ephemeral Packet ID (`[u8; 8]`) | [`core/src/mesh/delivery.rs#L85-L100`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/mesh/delivery.rs#L85-L100)<br>[`core/src/mesh/peer.rs#L370-L390`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/mesh/peer.rs#L370-L390) | `test_phase4_delivery_e2e_lifecycle` | **implemented and tested** |
| **Fresh Retry Nonces** | Retries advance Noise transport nonces, surviving replay window | [`core/src/crypto.rs#L360-L400`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/crypto.rs#L360-L400)<br>[`core/src/mesh/peer.rs#L370-L390`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/mesh/peer.rs#L370-L390) | `test_phase4_delivery_e2e_lifecycle`<br>`crypto::tests::test_stateless_dropped_packet_does_not_break_later_packets` | **implemented and tested** |
| **Recipient ACK Authentication** | Authenticated ACKs carrying logical ID, rejected if sender mismatch | [`core/src/mesh/delivery.rs#L102-L115`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/mesh/delivery.rs#L102-L115)<br>[`core/src/mesh/delivery.rs#L440-L465`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/mesh/delivery.rs#L440-L465) | `mesh::delivery::tests::test_ack_from_wrong_peer_rejected`<br>`test_phase4_delivery_e2e_lifecycle` | **implemented and tested** |
| **Exactly-Once Deduplication** | Recipient deduplication keyed by `(authenticated_sender, logical_id)` | [`core/src/mesh/delivery.rs#L220-L260`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/mesh/delivery.rs#L220-L260) | `mesh::delivery::tests::test_inbox_deduplicator_exactly_once_display`<br>`test_phase4_delivery_e2e_lifecycle` | **implemented and tested** |
| **Monotonic Delivery States** | FSM: `Queued -> InFlight -> {Delivered, Failed, Expired, Cancelled}` | [`core/src/mesh/delivery.rs#L350-L480`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/mesh/delivery.rs#L350-L480) | `mesh::delivery::tests::test_delivery_state_lifecycle_and_terminal_guarantees` | **implemented and tested** |
| **Delivery Integration E2E Lifecycle** | Handshake bootstrap, queueing, attempt 1, lost ACK, retry, dedup, completion | [`core/tests/test_phase4_delivery_e2e.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/tests/test_phase4_delivery_e2e.rs) | `test_phase4_delivery_e2e_lifecycle` | **implemented and tested** *(domain component composition)* |
| **Public Runtime Daemon Entry Point** | Service-owned background task daemon (`api::start_daemon` / FFI events) | *Part B Scope* | *Pending Part B Approval* | **deferred to Part B** |
| **Encrypted SQLite Outbox** | ChaCha20-Poly1305 AEAD record encryption, 96-bit CSPRNG nonces, AAD binding | [`core/src/storage/sqlite.rs#L110-L210`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/storage/sqlite.rs#L110-L210) | `storage::tests::test_send_to_normal_mode_durable_commit_and_aead_privacy`<br>`storage::tests::test_tampered_ciphertext_fails_aead_authentication` | **implemented and tested** |
| **Crash Recovery & Restart** | Unacknowledged records reloaded from SQLite with intact logical IDs | [`core/src/storage/sqlite.rs#L380-L470`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/storage/sqlite.rs#L380-L470) | `storage::tests::test_restart_and_crash_recovery_from_file` | **implemented and tested** |
| **Volatile Privacy Retention** | In-memory heap outbox; application-scoped zero intentional disk writes | [`core/src/storage/memory.rs#L20-L90`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/storage/memory.rs#L20-L90)<br>[`core/src/storage/sqlite.rs#L215-L221`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/storage/sqlite.rs#L215-L221) | `storage::tests::test_send_to_privacy_mode_zero_disk_guarantee` | **implemented and tested** |
| **Key Provider Abstraction** | `KeyProvider` trait with `InjectedKeyProvider` for Part A testing | [`core/src/storage/key.rs#L25-L65`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/storage/key.rs#L25-L65) | `storage::tests::test_keystore_locked_rejection` | **implemented and tested** |
| **Production Platform Keystore** | Android Keystore / iOS Keychain hardware-backed DEK wrapping | *Part B Scope* | *Pending Part B Approval* | **deferred to Part B** |
| **Pure Decision Router** | `Router::decide` with typed `RouteReason` codes, decoupled handshakes | [`core/src/mesh/router.rs#L85-L180`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/mesh/router.rs#L85-L180) | `mesh::router::tests::*` (8 tests) | **implemented and tested** |
| **Privacy Relay Transport Enforcement** | Physical `RelayMode::Disabled` requirement; transient direct observation insufficient | [`core/src/mesh/router.rs#L130-L150`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/mesh/router.rs#L130-L150) | `mesh::router::tests::test_router_privacy_mode_blocks_iroh_when_endpoint_relay_enabled_even_if_direct`<br>`mesh::router::tests::test_router_privacy_mode_allows_iroh_when_endpoint_relay_disabled_and_direct` | **implemented and tested** |
| **Authenticated Bucket Padding** | Buckets (64, 128, 256, 512) for $\le 510$B; checked length limits & framing tests | [`core/src/envelope.rs#L142-L177`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/envelope.rs#L142-L177) | `envelope::tests::test_pad_inner_payload_all_framing_boundaries`<br>`envelope::tests::test_pad_inner_payload_behavior_above_512` | **implemented and tested** |
| **Gateway Quotas & Bandwidth Reservation** | 64 KB/s global, 8 KB/s per-peer, 16 env/min; 25% queue & 25% bandwidth reserved | [`core/src/gateway/fairness.rs#L40-L125`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/gateway/fairness.rs#L40-L125) | `gateway::fairness::tests::test_control_plane_reservation_prevents_chat_starvation`<br>`gateway::fairness::tests::test_control_plane_reservation_prevents_bandwidth_starvation`<br>`gateway::fairness::tests::test_global_bandwidth_quota`<br>`gateway::fairness::tests::test_peer_envelope_rate_limit` | **implemented and tested** |
| **Gateway Default Stance** | Gateways OFF by default; disabling withdraws routes immediately | [`core/src/gateway/config.rs#L18-L27`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/gateway/config.rs#L18-L27)<br>[`core/src/gateway/bridge.rs#L122-L130`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/gateway/bridge.rs#L122-L130) | `mesh::router::tests::test_router_gateway_opt_out_falls_back_to_mesh` | **implemented and tested** |
| **Flutter UI State Integration** | Rendering delivery states, path diagnostics, mode toggles in app UI | *Part B Scope* | *Pending Part B Approval* | **deferred to Part B** |
| **Physical Multi-Phone Field Test** | Real BLE advertising/scanning, cross-network Iroh, battery measurements | *Part B Scope* | *Pending Part B Approval* | **deferred to Part B** |

---

## 2. Technical Audit Details

### 2.1 Simulator Timing & Discrete Stepping
- **Root Cause of Previous Stall**: In `core/src/mesh/relay.rs`, `step_at(sim_time)` previously called `self.transport.recv().await` when pending timers were empty. In single-threaded discrete event tests, `recv().await` blocked indefinitely awaiting incoming channel messages that could only be produced by other tasks.
- **Resolution**:
  - Implemented `Transport::try_recv(&mut self) -> Option<(ShortId, Vec<u8>)>` on both `SimTransport` and `BridgeTransport`.
  - Refactored `step_at(now)` to poll `fire_matured_relays(now)` and `self.transport.try_recv()`, returning `Ok(false)` immediately if no work is due.
  - Added hard iteration caps (`assert!(drain_steps <= 10_000)`) and 5-second test-level `tokio::time::timeout` wrappers across all simulation drain loops.

### 2.2 Partitioned-Bridge Baseline & Coverage Decisions
- **Benchmark Evaluation Results** (`cargo run --example evaluate`, 300 simulation runs across 20 seeds at 5% loss):
  ```
  Topology                      | Policy         |     Avg Tx |    Deliv % |   Mean Lat |    P95 Lat | Duplicates | Suppressed
  ------------------------------------------------------------------------------------------------------------------------
  Partitioned Bridge (31 nodes) | NaiveFlood     |       30.2 |      97.5% |     10.0ms |     10.0ms |      349.8 |        0.0
  Partitioned Bridge (31 nodes) | AdaptiveFlood  |        5.5 |      60.0% |     39.6ms |     78.0ms |       49.4 |       13.6
  Partitioned Bridge (31 nodes) | CoverageAware  |        9.4 |      97.3% |    141.3ms |    278.5ms |       83.3 |       20.8
  ```
- **Explanation of Changed Baseline & Binomial Variance**:
  - `PartitionedBridge31` consists of Cluster A (15 nodes) and Cluster B (15 nodes) connected solely via bridge links $14 \leftrightarrow 15$ and $15 \leftrightarrow 16$.
  - With $5\%$ link loss probability and $N=20$ simulation seeds, any loss on the single bridge cut prevents the flood from reaching all 15 nodes in Cluster B (delivering to only $15/30 = 50\%$ of nodes).
  - A single lost run on the bridge cut drops the mean delivery ratio by $\frac{100\% - 50\%}{20} = 2.5\%$.
  - Over 2 hops across the bridge, the cut failure probability is $p = 1 - (1 - 0.05)^2 \approx 0.0975$.
  - Binomial sampling error for $N=20$ yields standard deviation $\sigma = \sqrt{\frac{p(1-p)}{N}} \approx 6.6\%$ per trial, or $\pm 4.9\%$ delivery ratio variance across seed batches. Observed variations between $87.5\%$ and $97.5\%$ in Naive flooding are purely expected binomial sampling artifacts on a single-cut bridge cut.
- **Strictly Local Protocol Information**:
  - `CoverageAwareFlood` uses **only** local 1-hop physical BLE neighbor sets, 1-hop overheard duplicate sources within the suppression jitter window, and 1-hop neighbor sets announced in local Announce beacons.
  - No multi-hop or out-of-band global oracle is used.
- **Observation Overhead & Uncertainty**:
  - Periodic Announce beacons broadcast $\approx 30$ bytes every $10\text{--}30\text{s}$ ($\approx 1\text{--}3\text{ B/s}$ per node).
  - In-memory neighbor table is bounded to 64 entries with a 30s TTL.
  - Observation uncertainty: If packet loss causes missed beacons, the `uncovered` set remains non-empty, causing `is_coverage_complete` to return `false`. The node conservatively forwards rather than suppresses, preserving reachability guarantees under loss.

### 2.3 Delivery Integration Test Scope & Architecture
- **Domain Composition**: [`core/tests/test_phase4_delivery_e2e.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/tests/test_phase4_delivery_e2e.rs) exercises the full delivery lifecycle across integrated core modules:
  1. `OutboxManager` (normal mode, SQLite encrypted with ChaCha20-Poly1305).
  2. `DeliveryCoordinator` (attempt scheduling, timeouts, and state transitions).
  3. `PeerSessionManager` (Noise XX 3-flight handshake, directional session keys, 128-packet replay window).
  4. `Router` (`RouteContext` evaluation and `RouteReason` dispatch).
  5. `MeshNode` and `SimNetwork` (simulated transport transmission).
  6. `InboxDeduplicator` (atomic `(sender, logical_id)` deduplication).
- **Public Runtime Daemon Scope**:
  - The test verifies: handshake bootstrap, outbox admission, attempt 1 transmission, lost ACK handling, timeout detection, retry with identical logical ID (`[u8; 16]`) but fresh ephemeral packet ID (`[u8; 8]`) and incrementing transport nonce, recipient deduplication (`DedupStatus::Duplicate`), second ACK transmission, and terminal completion to `Delivered`.
  - **Honest Scope Boundary**: This test composes domain components in memory. It does **not** invoke the public service daemon (`api::start_daemon` or FFI runtime), which manages the Flutter background task lifecycle. Public service daemon testing is explicitly deferred to Part B.

### 2.4 Small-Record Padding Policy & Framing Boundaries
- **Small-Record-Only Policy**:
  - Padded buckets ($64, 128, 256, 512$ bytes) apply strictly to payloads $\le 510$ bytes (total framed buffer $\le 512$ bytes).
  - Payloads $> 510$ bytes up to `u16::MAX` ($65,535$ bytes) are prefixed with a 2-byte big-endian length without trailing zero-padding. This avoids wasting scarce BLE airtime on arbitrary large records while maintaining uniform length-prefixed framing.
  - Payloads $> 65,535$ bytes are checked and rejected with `Error::Envelope("Payload exceeds maximum u16 length")`.
- **Framing Boundary Test Suite** (`envelope::tests::test_pad_inner_payload_all_framing_boundaries`):
  - `0 bytes`: Padded to bucket 64; unpads to 0 bytes.
  - `1 byte`: Padded to bucket 64; unpads to 1 byte.
  - `62 bytes`: Padded to bucket 64 ($62 + 2 = 64$ bytes exact bucket match); unpads to 62 bytes.
  - `63 bytes`: Padded to bucket 128 ($63 + 2 = 65 > 64$ bytes); unpads to 63 bytes.
  - `510 bytes`: Padded to bucket 512 ($510 + 2 = 512$ bytes exact bucket match); unpads to 510 bytes.
  - `511 bytes`: Unpadded length prefix, 513 bytes total ($511 + 2 = 513 > 512$ bytes); unpads to 511 bytes.
  - `65,535 bytes`: Unpadded length prefix, 65,537 bytes total (`u16::MAX`); unpads to 65,535 bytes.
  - `65,536 bytes`: Checked and rejected with `Error::Envelope`.

### 2.5 Gateway Quotas, Refill/Window Semantics, and Starvation Protection
Implemented in [`core/src/gateway/fairness.rs`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/core/src/gateway/fairness.rs):
- **Quota Units & Windows**:
  - **Global Bandwidth**: Bytes/second ($64\text{ KiB/s} = 65,536\text{ B/s}$ limit, 1-second sliding window).
  - **Per-Peer Bandwidth**: Bytes/minute ($8\text{ KiB/s}$ sustained, $480\text{ KiB/min}$ limit, 60-second sliding window).
  - **Per-Peer Envelope Rate**: Envelopes/minute (16 env/min limit, 60-second sliding window). Keyed by authenticated Ed25519 `PeerId`.
- **Anti-Starvation Protection (Queue & Bandwidth)**:
  - `MsgType::Handshake` and `MsgType::Ack` are designated control traffic.
  - **Queue Reservation**: $25\%$ of queue depth is strictly reserved for control traffic. Non-control (Chat) data is rejected when queue reaches $75\%$.
  - **Bandwidth Reservation**: $25\%$ of global bandwidth ($16\text{ KiB/s}$) is strictly reserved for control/ACK traffic. Chat traffic is capped at $75\%$ ($48\text{ KiB/s}$). If chat data completely saturates its $48\text{ KiB/s}$ allocation, Handshake and Ack traffic can still be scheduled up to the full $64\text{ KiB/s}$ ceiling.
  - **Per-Peer Envelope Throttling**: Chat packets count toward the 16 env/min cap; control/ACK traffic is exempt from data envelope throttling so peers are never locked out of delivering ACKs.
- Verified by:
  - `gateway::fairness::tests::test_control_plane_reservation_prevents_chat_starvation` (queue reservation)
  - `gateway::fairness::tests::test_control_plane_reservation_prevents_bandwidth_starvation` (bandwidth reservation)
  - `gateway::fairness::tests::test_global_bandwidth_quota`
  - `gateway::fairness::tests::test_peer_envelope_rate_limit`

### 2.6 Privacy Relay Transport Capability Enforcement
- **Limitation of Router Observation**:
  - A transient router observation of `IrohPathObservation::Direct` on an endpoint with relays enabled cannot guarantee privacy:
    1. Discovery and hole-punching metadata can leak to relay servers during connection establishment.
    2. Active Iroh connections can migrate mid-stream to relay servers upon cellular/WiFi handover or NAT rebinding.
- **Capability Enforcement**:
  - Transport must enforce `RelayMode::Disabled` on the Iroh endpoint at construction time.
  - `RouteContext` exposes `iroh_endpoint_relay_disabled: bool`.
  - When `mode == MessageMode::Privacy && privacy_block_relay`:
    - If `!iroh_endpoint_relay_disabled`, or if `iroh_path` is `Relay`, `Mixed`, or `Unknown`, the candidate is strictly blocked and falls back to BLE mesh.
    - An `Unknown` path is also blocked because path capability has not been verified direct.
    - Only endpoints configured with `RelayMode::Disabled` AND verified `IrohPathObservation::Direct` are admitted.
- Verified by:
  - `mesh::router::tests::test_router_privacy_mode_blocks_iroh_relay_and_falls_back_to_mesh`
  - `mesh::router::tests::test_router_privacy_mode_blocks_iroh_when_endpoint_relay_enabled_even_if_direct`
  - `mesh::router::tests::test_router_privacy_mode_allows_iroh_when_endpoint_relay_disabled_and_direct`

### 2.7 Route-Snapshot Epoch Reconnect & Identity Binding
- **Monotonic Session Epochs**:
  - `GatewayBridge` enforces atomic strictly monotonic epoch generation across reconnects: `last_session_epoch.fetch_update(..., |prev| Some(now_secs.max(prev + 1)))`.
- **Reconnect Replay Defense**:
  - In `core/src/gateway/routes.rs`, `remove_routes_for_endpoint` immediately removes active route entries from the table upon disconnect, but **preserves** the `(last_epoch, last_seq)` in `gateway_state`.
  - Upon gateway reconnect, any replayed `RouteSnapshot` from the previous session (same epoch or older epoch) is rejected with `"Stale or duplicate sequence rejected"` or `"Stale epoch rejected"`. Only a fresh snapshot with a strictly higher epoch is accepted.
  - Sequence numbers are checked for overflow: `sequence == u64::MAX` returns `"Sequence number overflow: session must restart"`.
- Verified by:
  - `gateway::routes::tests::test_snapshot_reconnect_old_epoch_replay_rejected`
  - `gateway::routes::tests::test_snapshot_sequence_overflow_rejected`
  - `gateway::routes::tests::test_snapshot_epoch_and_sequence_monotonicity`
  - `gateway::identity_binding::tests::test_valid_binding_verification`
  - `gateway::identity_binding::tests::test_mismatched_endpoint_key_rejected`
  - `gateway::identity_binding::tests::test_tampered_signature_rejected`

### 2.8 Application-Scoped Privacy Wording
All documentation and code comments state:
> **Application-Scoped Non-Persistence**: The Rust core guarantees zero intentional writes of Privacy message bodies to persistent storage (SQLite, temp files, or logs). It does not prevent OS-level virtual memory paging (swap file), OEM crash dumps, or memory forensic analysis if the host device is compromised.

---

## 3. Verification Commands & Outputs

```powershell
cargo fmt --check
# Clean (exit code 0)

cargo clippy --all-targets -- -D warnings
# Clean (exit code 0, 0 warnings)

cargo test --all
# 122 passed; 0 failed; 0 ignored; finished in 5.56s
#   - bitchat2_core (lib): 120 passed
#   - test_phase3_mesh_iroh_e2e: 1 passed
#   - test_phase4_delivery_e2e: 1 passed
```

---

## 4. Conclusion & Part B Gate Status

All Part A defects and review items from Section 11 of `PHASE_4_REVISED.md` are resolved, tested, and accounted for with honest boundaries.

**Part B remains strictly blocked** awaiting human review. No platform keystore integration or Flutter UI code has been executed.
