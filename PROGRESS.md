# Phase 2 Progress Log

## Status Overview
- Cumulative Phase 1 + 2 Share: 35%
- Phase 2 Share: 20%
- Current Status: Task 0 completed (Starting Part A: Task 1 next)

## Phase 2 Task Checklist

### Carry-over & Hardening
- [x] Task 0 — Phase 1 carry-over fixes (Stateless Noise, 128-packet sliding replay window, domain-separated handshake signature, compiler-enforced no-panic denies, clean committed configs)

### Part A — Rust Mesh Logic (Simulator-verified)
- [x] Task 1 — Seen-message cache (`LruSeenCache`, `BloomSeenCache`, ADR-0005)
- [x] Task 2 — Link-layer fragmentation (`Fragmenter`, `Reassembler`, DoS bounds)
- [ ] Task 3 — Mesh node and relay engine (`MeshNode`, `NaiveFlood` baseline, `MeshStats`)
- [ ] Task 4 — Adaptive relay policy (`AdaptiveFlood`, density-scaled delay, suppression)
- [ ] Task 5 — Evaluation harness (`docs/EVALUATION.md`, 5 topologies, 20+ seeds)
- [ ] Task 6 — End-to-end encryption across relays (E2E Noise over mesh)

### Part B — Flutter Shell & BLE Bridge (UNVERIFIED until physical device tests)
- [ ] Task 7 — Rust `BridgeTransport` and `flutter_rust_bridge` API (UNVERIFIED)
- [ ] Task 8 — Flutter app & dual-role BLE GATT layer (UNVERIFIED)
- [ ] Task 9 — Docs & device test plan (`docs/DEVICE_TEST_PLAN.md`) (UNVERIFIED)

## Test Counts & Verification Run
- **Total Tests Passing**: 43 tests (0 failures, 0 ignored)
  - `identity`: 6 tests
  - `envelope`: 10 tests
  - `crypto`: 8 tests (including stateless out-of-order delivery, replay rejection, window age rejection, dropped packet resilience, domain separation)
  - `mesh::seen`: 6 tests (LRU no false negatives, capacity bound, TTL expiry, Bloom no false negatives, measured FPR <= 3.5%, 2-rotation expiry)
  - `mesh::fragment`: 9 tests (single fragment fast path, empty frame, multi-fragment ordered, out-of-order reassembly, duplicate ignored, missing fragment timeout, overflow eviction, malformed header validation, proptest split/reassemble roundtrip)
  - `transport::sim`: 4 tests
- **Clippy**: `cargo clippy --all-targets -- -D warnings` clean with zero warnings.
- **Compiler Denies**: `#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]` active in `core/src/lib.rs`.
- **Formatter**: `cargo fmt --check` clean.

## Not Verified (Phase 2 Boundaries)
- **Physical BLE Radio Hardware**: Real 2.4 GHz radio propagation, BLE advertising packet size limits (31-byte legacy / extended advertising), and OS-level Bluetooth stack quirks are simulated in software until Part B hardware testing.
- **Part B Items**: All mobile app and hardware BLE integration tasks remain explicitly marked UNVERIFIED until physical multi-phone testing is executed.
