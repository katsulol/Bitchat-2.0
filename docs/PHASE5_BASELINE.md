# Phase 5 Baseline Audit & Benchmark Reconciliation (`PHASE5_BASELINE.md`)

**Branch**: `phase-5-trust` (branched from `phase-4-delivery` @ `94104bd`)  
**Baseline Commit**: `94104bd` (`fix(gateway): filter local identity from route announcements and active gateway routes`)  
**Date**: 2026-11-02  

---

## 1. Toolchain & Binary Hashes

| Component | Version / SHA256 |
| :--- | :--- |
| **Rust Compiler** | `rustc 1.98.0 (88d9e12ae 2026-08-18)` |
| **Cargo** | `cargo 1.98.0 (797e8a9bc 2026-08-05)` |
| **Pre-Gate-0 JNI Library (`arm64-v8a/libbitchat2_core.so`)** | `C0F965791312286DC30E746636B3D54E690EB545BE30E003C8B85F9DAF5EE141` |
| **Baseline Test Suite (`94104bd`)** | `118 passed; 0 failed` (116 unit + 2 integration) |

---

## 2. Replay-Window Prose Discrepancy Resolution

Earlier summary prose in `COMPLETE_WORK_LOG_CODES_AND_TESTS.md` referred to a "64-packet sliding replay window", while `PHASE_4_FINAL_REPORT.md` referred to a "128-packet sliding replay window".

**Actual Source Verification**:
- [`core/src/crypto.rs`](../core/src/crypto.rs) line 35:
  ```rust
  pub const REPLAY_WINDOW_SIZE: u64 = 128;
  ```
- [`ReplayWindow`](../core/src/crypto.rs) stores `bitmap: u128` and `highest_seen: Option<u64>`, enforcing an exact **128-packet** sliding anti-replay window.
- Verified by unit test `crypto::tests::test_replay_window_sliding_128_boundary`.

---

## 3. Reconciled Mesh Relay Benchmark (`core/examples/evaluate.rs`)

### Why `Line (10)` and `Grid (7x7)` Reported ~71–78% Total Delivery
In `core/src/envelope.rs`, `MAX_HOPS` is `7`.
- **Line (10 nodes)**: Origin is node `0`. Only nodes `1..=7` (7 of 9 recipients = `77.8%`) are within 7 hops; nodes `8` and `9` are at hop distance 8 and 9 and are dropped by the `hop_count >= MAX_HOPS` bound even at 0% link loss.
- **Grid (7x7, 49 nodes)**: Origin is corner `(0, 0)`. Exactly 33 of 48 recipients (`68.8%`–`79%` depending on diagonal paths) are within Manhattan distance $\le 7$; 15 nodes lie at hop distance $8..12$.

In Gate 0.6, [`core/examples/evaluate.rs`](../core/examples/evaluate.rs) was updated to:
1. Seed all node identities (`deterministic_identities`) and policy PRNGs (`AdaptiveFlood::with_seed`, `CoverageAwareFlood::with_seed`) deterministically from the run seed (`1000..1019`).
2. Record both **`Deliv(All)`** (denominator = $N - 1$ total nodes) and **`Deliv(TTL)`** (denominator = nodes reachable within `MAX_HOPS = 7` via BFS from node 0) in [`evaluation_results.csv`](../evaluation_results.csv) (300 runs = 5 topologies $\times$ 3 policies $\times$ 20 seeds, 5% per-hop link loss).

### Reconciled 300-Run Summary Table

| Topology | Policy | Avg Tx | Deliv (All $N-1$) | Deliv (Reachable $\le 7$ Hops) | Mean Latency | P95 Latency | Avg Duplicates | Avg Suppressed |
| :--- | :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| **Line (10 nodes)** | `NaiveFlood` | 7.1 | 76.7% | **87.1%** | 9.0 ms | 9.0 ms | 5.0 | 0.0 |
| | `AdaptiveFlood` | 7.0 | 76.1% | **85.7%** | 94.4 ms | 176.5 ms | 4.8 | 0.0 |
| | `CoverageAware` | 6.3 | 66.7% | **76.4%** | 82.9 ms | 156.0 ms | 4.3 | 0.0 |
| **Grid (7x7, 49 nodes)** | `NaiveFlood` | 33.1 | 78.6% | **100.0%** | 10.0 ms | 10.0 ms | 70.3 | 0.0 |
| | `AdaptiveFlood` | 30.2 | 77.4% | **100.0%** | 140.2 ms | 256.5 ms | 61.1 | 1.7 |
| | `CoverageAware` | 32.5 | 78.7% | **100.0%** | 142.8 ms | 258.0 ms | 68.0 | 0.3 |
| **Dense Cluster (30 nodes)** | `NaiveFlood` | 30.0 | 100.0% | **100.0%** | 10.0 ms | 10.0 ms | 769.0 | 0.0 |
| | `AdaptiveFlood` | 4.5 | 100.0% | **100.0%** | 10.5 ms | 14.5 ms | 91.5 | 25.5 |
| | `CoverageAware` | 4.5 | 100.0% | **100.0%** | 10.5 ms | 14.5 ms | 91.3 | 25.5 |
| **Random Geometric (50 nodes)** | `NaiveFlood` | 48.9 | 99.2% | **99.4%** | 10.0 ms | 10.0 ms | 362.8 | 0.0 |
| | `AdaptiveFlood` | 24.0 | 95.0% | **95.4%** | 130.3 ms | 254.5 ms | 128.4 | 22.4 |
| | `CoverageAware` | 28.9 | 99.4% | **99.8%** | 128.1 ms | 250.0 ms | 173.2 | 19.9 |
| **Partitioned Bridge (31 nodes)** | `NaiveFlood` | 28.7 | 92.3% | **92.3%** | 10.0 ms | 10.0 ms | 331.2 | 0.0 |
| | `AdaptiveFlood` | 5.7 | 62.8% | **62.8%** | 39.6 ms | 78.0 ms | 50.4 | 14.2 |
| | `CoverageAware` | 9.1 | **92.3%** | **92.3%** | 118.1 ms | 246.5 ms | 80.3 | 19.6 |
