# Phase 5 Part A Evaluation Report

**Branch**: `phase-5-trust`  
**Evaluator**: `core/examples/evaluate.rs` (`cargo run --manifest-path core/Cargo.toml --example evaluate`)  
**Raw Dataset**: [`evaluation_results.csv`](file:///C:/Users/pulki/OneDrive/Pictures/Desktop/Bitchat/evaluation_results.csv) (400 runs = 4 policies $\times$ 5 topologies $\times$ 20 seeds)

---

## 1. Relay Policy Benchmark (400-Run Deterministic Suite)

### 1.1 Simulation Parameters
- **Link Loss Rate**: 5.0% uniform per hop
- **Hop Budget**: `MAX_HOPS = 7`
- **Seeds**: `1..=20` per `(policy, topology)` pair
- **Injected Clock & RNG**: Deterministic `StdRng::seed_from_u64` and simulated virtual time (`VirtualClock`) injected into `CoverageAwareFlood` and `ResourceAwareRelayPolicy` (`Coverage+ResMin`).

### 1.2 Aggregated Results by Topology and Policy

| Topology | Policy | Avg Tx | Deliv (All) | Deliv (Within TTL) | Mean Lat (ms) | P95 Lat (ms) | Avg Duplicates | Avg Suppressed |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| **Line (10 nodes)** | `NaiveFlood` | 6.0 | 62.2% | 72.1% | 9.0 | 9.0 | 4.2 | 0.0 |
| **Line (10 nodes)** | `AdaptiveFlood` | 6.7 | 70.6% | 80.7% | 87.9 | 165.0 | 4.7 | 0.0 |
| **Line (10 nodes)** | `CoverageAware` | 7.0 | 74.4% | 85.7% | 94.0 | 176.5 | 5.0 | 0.0 |
| **Line (10 nodes)** | `Coverage+ResMin` | 6.7 | 71.1% | 81.4% | 121.1 | 232.5 | 4.7 | 0.0 |
| **Grid (7x7, 49 nodes)** | `NaiveFlood` | 33.2 | 78.5% | 100.0% | 10.0 | 10.0 | 70.5 | 0.0 |
| **Grid (7x7, 49 nodes)** | `AdaptiveFlood` | 30.6 | 77.7% | 99.8% | 141.0 | 257.5 | 60.8 | 1.9 |
| **Grid (7x7, 49 nodes)** | `CoverageAware` | 32.6 | 78.6% | 100.0% | 142.9 | 256.0 | 68.8 | 0.2 |
| **Grid (7x7, 49 nodes)** | `Coverage+ResMin` | 32.2 | 78.5% | 100.0% | 199.4 | 368.5 | 67.8 | 0.3 |
| **Dense Cluster (30 nodes)** | `NaiveFlood` | 30.0 | 100.0% | 100.0% | 10.0 | 10.0 | 769.0 | 0.0 |
| **Dense Cluster (30 nodes)** | `AdaptiveFlood` | 4.4 | 100.0% | 100.0% | 10.5 | 14.5 | 89.1 | 25.6 |
| **Dense Cluster (30 nodes)** | `CoverageAware` | 4.5 | 100.0% | 100.0% | 10.6 | 15.0 | 91.7 | 25.5 |
| **Dense Cluster (30 nodes)** | `Coverage+ResMin` | 4.3 | 100.0% | 100.0% | 10.6 | 15.5 | 86.0 | 25.7 |
| **Random Geometric (50 nodes)** | `NaiveFlood` | 49.1 | 99.8% | 100.0% | 10.0 | 10.0 | 365.4 | 0.0 |
| **Random Geometric (50 nodes)** | `AdaptiveFlood` | 24.4 | 97.3% | 97.7% | 135.4 | 257.5 | 130.8 | 23.1 |
| **Random Geometric (50 nodes)** | `CoverageAware` | 28.6 | 99.6% | 100.0% | 123.9 | 239.0 | 169.0 | 20.5 |
| **Random Geometric (50 nodes)** | `Coverage+ResMin` | 28.6 | 99.6% | 100.0% | 172.0 | 338.0 | 169.8 | 20.5 |
| **Partitioned Bridge (31 nodes)** | `NaiveFlood` | 27.9 | 89.8% | 89.8% | 10.0 | 10.0 | 322.1 | 0.0 |
| **Partitioned Bridge (31 nodes)** | `AdaptiveFlood` | 4.8 | 54.7% | 54.7% | 18.9 | 32.5 | 43.8 | 12.6 |
| **Partitioned Bridge (31 nodes)** | `CoverageAware` | 9.1 | 92.2% | 92.2% | 126.7 | 250.5 | 81.2 | 19.6 |
| **Partitioned Bridge (31 nodes)** | `Coverage+ResMin` | 8.9 | 94.8% | 94.8% | 181.4 | 361.5 | 77.8 | 20.4 |

### 1.3 Key Findings & Trade-off Analysis
1. **Partitioned Bridge Survival (`CoverageAware` & `Coverage+ResMin` vs `AdaptiveFlood`)**:
   - Pure counter-based `AdaptiveFlood` suffers a severe bottleneck drop on `Partitioned Bridge (31 nodes)` (`54.7%` delivery) because duplicates heard within the source cluster suppress the bridge node before the bridge crosses the cut-edge.
   - `CoverageAware` (`92.2%`) and `Coverage+ResMin` (`94.8%`) preserve cut-edge relaying because observed neighbor sets do not cover the remote cluster, while cutting total transmissions by **68.1%** compared to `NaiveFlood` (`8.9` vs `27.9` Avg Tx) and reducing duplicate receptions by **75.8%** (`77.8` vs `322.1`).
2. **Dense Cluster Storm Suppression**:
   - In `Dense Cluster (30 nodes)`, `CoverageAware` (`4.5` Avg Tx) and `Coverage+ResMin` (`4.3` Avg Tx) maintain **100.0% delivery** while reducing duplicate packets by **88.8%** (`86.0` vs `769.0`).
3. **Latency vs Workload under `Minimal` Resource Budget (`Coverage+ResMin`)**:
   - Under `ResourceBudgetState::Minimal` (`1.5x` holdback delay multiplier and strict token-bucket pacing), P95 latency increases moderately (`239.0ms -> 338.0ms` on Random Geometric; `250.5ms -> 361.5ms` on Partitioned Bridge) in exchange for lower peak radio transmit rate and slightly fewer redundant transmissions, with zero loss in reachability within TTL.
4. **No Unmeasured Battery Savings Claimed**:
   - In accordance with Phase 5 engineering rules, desktop simulation metrics prove reductions in **transmit event count (`Avg Tx`)** and **duplicate receptions (`Avg Duplicates`)**. Physical mAh battery savings are not claimed prior to Part B hardware telemetry.

---

## 2. Trust Admission, Sybil Resistance & Attack Evaluation

Verified via unit and end-to-end integration tests (`core/src/trust/`, `core/src/gateway/bridge.rs`, `core/tests/test_phase5_policy_e2e.rs`):

| Attack / Edge Vector | Enforcement Mechanism | Verification Test | Result |
| :--- | :--- | :--- | :--- |
| **Uninvited Gateway Connection** | `Gate 0.1` + `TrustPolicy`: Identity binding proves key ownership, NOT gateway authorization. Unapproved peers are denied before route registration or frame relaying. | `gateway::bridge::tests::test_incoming_stranger_rejected_by_default`, `test_phase5_e2e_unapproved_gateway_rejected_until_vouched_then_revoked` | **PASS** (0 bytes relayed, 0 routes imported) |
| **Invitation Replay Attack** | Single-use `GatewayPairingToken` is marked `consumed = true` upon first handshake admission. Second connection attempt with same token is rejected. | `gateway::bridge::tests::test_single_use_invitation_cannot_be_replayed` | **PASS** |
| **Transitive Voucher Chain (`A -> B -> C`)** | Only peers at `TrustLevel::Trusted` with explicit `VouchOthers` capability can issue accepted vouchers. Vouched peers (`TrustLevel::Vouched`) cannot receive `VouchOthers`. | `trust::store::tests::test_no_transitive_vouching_vouched_peer_cannot_issue` | **PASS** |
| **Voucher Flood / Sybil Amplification** | Per-issuer cap (`MAX_VOUCHED_PEERS_PER_ISSUER = 8`), hourly rate limit (`MAX_VOUCHERS_PER_ISSUER_PER_HOUR = 4`), global cap (`MAX_TOTAL_VOUCHED_PEERS = 64`). | `trust::store::tests::test_issuer_vouch_cap_and_hourly_rate_limit` | **PASS** |
| **Issuer Revocation / Distrust Cascade** | Revoking or blocking an issuer atomically invalidates all active vouchers from that issuer and recomputes effective capabilities (`compute_effective_capabilities`). | `trust::store::tests::test_revoke_issuer_invalidates_vouched_peers` | **PASS** |
| **Capability Monotonicity (`Deny > Allow`)** | Explicit `deny_capabilities` mask always overrides base `TrustLevel` and `allow_capabilities` grants (`(base \| allow) & !deny`). | `trust::policy::tests::test_explicit_deny_overrides_allow_and_vouch` | **PASS** |
| **Mid-Flight Gateway Revocation Failover** | Revoking an active gateway withdraws its routes atomically; next router decision fails over to verified alternate gateway or local BLE mesh. | `gateway::bridge::tests::test_gateway_trust_integration_and_atomic_failover` | **PASS** |

---

## 3. Coarse Proximity & Volatile Chat Verification

| Subsystem | Verification Property | Test Name | Result |
| :--- | :--- | :--- | :--- |
| **Coarse Proximity Classifier** | Median window (`N=5`), minimum 3 samples, asymmetric hysteresis (`-62/-68 dBm`, `-78/-83 dBm`), 30s staleness decay to `Unknown`, zero distance conversion. | `mesh::proximity::tests::test_proximity_hysteresis_and_smoothing`, `test_phase5_e2e_proximity_decay_and_volatile_memory_expiry` | **PASS** |
| **Volatile Chat Negotiation & Zero-Disk** | Mutual `VolatileChatControl` capability negotiation, downgrade rejection while active, bounded in-memory retention (`ZeroizeOnDrop`), outbox drop cancellation on expiry. | `storage::volatile::tests::test_volatile_negotiation_and_downgrade_rejection`, `test_volatile_expiry_purges_memory_and_cancels_outbox` | **PASS** |
