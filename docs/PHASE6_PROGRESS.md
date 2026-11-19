# Bitchat 2.0 — Phase 6 Execution Progress & Acceptance Ledger

- **Execution Branch**: `phase-6`
- **Baseline Commit**: `6638a797ed28115bec75a660feda32f1625c0cdd` (reconciled `adaptive-delivery` HEAD on top of `main` `603b5f4`)
- **Contract Reference**: `PHASE_6.md`

---

## Task Status Matrix

| Task | Scope / Deliverable | Priority | Status | Commit | Evidence / Notes |
|---|---|---|---|---|---|
| **Task 0** | Freeze baseline, map live code, reconcile gaps & ownership | Baseline | **PASS** | `d71dba8` | `docs/PHASE6_BASELINE.md`, `docs/DECISIONS.md` (ADR-0017), `170/170` Rust tests + `5/5` Flutter tests verified at `6638a79` |
| **Task 1** | Live authorization & explicit link identity states (`UnboundLink`/`ClaimedShortId`/`VerifiedPeer`) | P0 | **PASS** | `c097374` | `relay.rs`, `trust/model.rs`, `api.rs`, `ffi.rs`, 6 new public-runtime tests in `core/tests/test_phase6_auth_e2e.rs` (`176/176` Cargo tests pass) |
| **Task 2** | Encrypted SQLite trust persistence & fail-closed gateway startup wiring | P0 | **PASS** | `cd1b573` | `core/src/trust/sqlite.rs` (`bitchat_trust.db`), `GatewayConfig::seed_consent_state`, `api.rs` + `bridge.rs` startup/persist wiring, 4 E2E + 2 unit tests (`182/182` Cargo tests pass) |
| **Task 3** | Live resource metering (fragmented wire bytes, control cap, stale telemetry) | P0 | **PASS** | *pending commit* | `resource.rs` (`estimate_fragmented_wire_bytes`, `refresh_stale_telemetry`, per-neighbor control cap, 256-entry bound), `relay.rs`, 4 new tests in `test_resource_forwarding_e2e.rs` (`186/186` Cargo tests pass) |
| **Task 4** | Protected root identity (`BCID` v1) & crash-safe atomic migration | P0 | Pending | — | Depends on Task 0 |
| **Task 5** | Runtime lifecycle ownership, Iroh path diagnostics & 43+ FFI safety audit | P1 | Pending | — | Depends on Tasks 1–4 |
| **Task 6** | Public-runtime release-blocker regression suite (`PHASE6_RUNTIME_ACCEPTANCE.md`) | P1 | Pending | — | Depends on Tasks 1–5 |
| **Task 7** | Optional experimental bounded cover traffic (`COVER_TRAFFIC.md`, default OFF) | P2 | Pending | — | Gated on Task 6 |
| **Task 8** | Bounded conversation sync design (`SYNC_DESIGN.md`) | P2 | Pending | — | Gated on Task 6 |
| **Task 9** | Bounded `blake3` Merkle reconciliation (`SYNC_PROTOCOL.md`, benchmarked vs ID inventory) | P2 | Pending | — | Gated on Task 8 design |
| **Task 10** | Flutter UX completion (requests, key recovery UI, RSSI/volatile hardening, tests) | P1 | Pending | — | Depends on Task 6 |
| **Task 11** | CI extension, parser fuzz smoke, provenance & Gradle release signing wiring | P3 | Pending | — | Depends on Tasks 5, 10 |
| **Task 12** | Matched 5-topology multi-seed simulations & ablations (`EVALUATION_FINAL.md`) | P3 | Pending | — | Depends on policy code |
| **Task 13** | Human-run physical multi-device matrix (`DEVICE_TEST_RESULTS_FINAL.md`) | P3 | Pending (User-run) | — | Prep scripts/checklists only; phones marked `Pending` until physical run |
| **Task 14** | Internal security hardening & stress audit (`SECURITY_REVIEW_FINAL.md`) | P3 | Pending | — | Depends on Tasks 1–11 |
| **Task 15** | Final release packaging, report docs, 7-min demo script & handoff | P3 | Pending | — | Final gate |
