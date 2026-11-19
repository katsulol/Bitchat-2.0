# Bitchat 2.0 — Final Quantitative Evaluation & Ablation Report (`Task 12`)

## 1. Reproducibility Metadata & Preserved Historical Artifacts

- **Evaluation Harness Binary**: `core/examples/evaluate_phase6.rs`
- **Exact Reproduction Command**:
  ```powershell
  cargo run --manifest-path core/Cargo.toml --release --example evaluate_phase6
  ```
- **Source Commit Evaluated**: `88b124dfa796` (branch `phase-6`)
- **Seeds**: `20` matched deterministic seeds (`1000..1019`)
- **Raw Output Artifacts (`docs/eval/`)**:
  - `docs/eval/eval_config.json` — simulation parameters, seeds, and scope declarations
  - `docs/eval/phase6_flood_raw.csv` — `600` raw seed rows (`6` topologies × `5` policies × `20` seeds)
  - `docs/eval/phase6_retry_ablation.csv` — `160` raw seed rows (`Warm`/`Fresh` × `5%`/`20%` loss × `NoRetry(1)`/`WithRetry(3)` × `20` seeds)
  - `docs/eval/phase6_cover_raw.csv` — `40` raw seed rows (`CoverOFF` vs `CoverON` × `20` seeds)
  - `docs/eval/phase6_sync_raw.csv` — `21` set-size/overlap cells comparing `IdInventory` vs bounded `Merkle`
  - `docs/eval/chart_flood_tx_vs_delivery.svg` & `docs/eval/chart_sync_bytes.svg` — standalone SVG visualizations
- **Preserved Historical Artifact**: Root `evaluation_results.csv` and `core/examples/evaluate.rs` are preserved **untouched** for direct historical comparison.

### Comparator & Proxy Scope Disclaimers
1. **Comparator Scope**: `NaiveFlood` and `AdaptiveFlood` are Bitchat 2.0's own internal baselines implemented inside `core/src/mesh/policy.rs`. They are **not** third-party competitor binaries or external native `bitchat` releases; no fabricated competitor scores are reported.
2. **Resource Proxy Scope**: Resource-aware simulation variants (`Coverage+ResBalanced`, `Coverage+ResMinimal`) measure configured byte/work forwarding proxies (`transmissions`, `wire_bytes_total`, `relays_suppressed`) in the discrete-event simulator. They do **not** establish physical phone battery-percentage savings (which belong exclusively to human-run Task 13 in `docs/DEVICE_TEST_RESULTS_FINAL.md`).

---

## 2. Latency Accounting & Comparison Against Historical `evaluation_results.csv`

In the earlier `core/examples/evaluate.rs` (which produced root `evaluation_results.csv`), links were configured with `zero_latency = Duration::ZERO` and the event loop drained `while progress { node.poll_step(sim_time) }` within a single `10ms` tick. Because `NaiveFlood` has `0ms` holdback delay, multi-hop flood waves across 7 hops collapsed onto a single simulated timestamp (`mean_latency_ms = 10.00, p95_latency_ms = 10.00`).

In `core/examples/evaluate_phase6.rs`:
- Each wireless hop accounts for explicit per-hop radio serialization and transit latency `LINK_DELAY_MS = 25ms` (`((hop_count + 1) * 25ms)`) in addition to the cumulative policy holdback timer advanced in `TIMER_STEP_MS = 10ms` increments.
- Consequently, `NaiveFlood` latency now reflects actual multi-hop path length (`25.0ms` on 1-hop `Dense Cluster`, `48.8ms` on `Partitioned Bridge`, `76.2ms` on `Sparse Line`, `88.8ms` on `Random Geometric`, `132.5ms` on `Grid 7x7`), while `CoverageAware` and `Coverage+ResMinimal` reflect the true holdback-for-suppression latency trade-off (`143ms–329ms` median).
- **Simulation Limitation**: In-memory discrete-event simulation models independent Bernoulli packet loss (`5%`) and fixed `25ms` per-hop link delay + BLE 244B MTU fragmentation (`estimate_fragmented_wire_bytes`); it does not model 2.4 GHz RF co-channel contention, OS Bluetooth stack scheduling jitter, or antenna multipath fading.

---

## 3. Multi-Topology Broadcast Flood & Resource-Aware Policy Results (`20` Seeds)

Notice the distinction between **Total Delivery (`DelivAll`, denominator = all `N - 1` nodes)** and **Hop-Budget-Reachable Delivery (`DelivTTL`, denominator = nodes reachable within `MAX_HOPS = 7`)**:
- On `Sparse Line (10 nodes)`, nodes at hop indices 8 and 9 exceed `MAX_HOPS = 7` (`reachable_within_ttl = 7` of `9`), capping `DelivAll` at `77.8%` by policy rather than link loss.
- On `Grid (7x7, 49 nodes)`, `33` of `48` destinations lie within `<= 7` Manhattan hops from the corner origin (`DelivAll` ceiling `68.8%–79.0%`), whereas **`DelivTTL` reaches `100.0%±0.0%`**.
- Across all `600` runs, application-layer deduplication (`msg_id` tracking) resulted in **`0` duplicate UI displays (`duplicate_displays = 0`)**.

| Topology | Policy | Transmissions (`mean ± sd`) | `DelivAll` (`mean ± sd`) | `DelivTTL` (`mean ± sd`) | Median Latency (`ms`) | P95 Latency (`ms`) | Duplicate Receptions (`Rx`) | Relays Suppressed |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| **Dense Cluster (30 nodes)** | `NaiveFlood` | `30.0 ± 0.0` | `100.0% ± 0.0%` | `100.0% ± 0.0%` | `25.0` | `33.8` | `768.8` | `0.0` |
| **Dense Cluster (30 nodes)** | `AdaptiveFlood` | `4.5 ± 0.7` | `100.0% ± 0.0%` | `100.0% ± 0.0%` | `25.0` | `38.2` | `92.8` | `25.4` |
| **Dense Cluster (30 nodes)** | `CoverageAware` | `4.3 ± 0.6` | `100.0% ± 0.0%` | `100.0% ± 0.0%` | `25.0` | `38.8` | `86.3` | `25.7` |
| **Dense Cluster (30 nodes)** | `Coverage+ResBalanced` | `4.2 ± 0.5` | `100.0% ± 0.0%` | `100.0% ± 0.0%` | `25.0` | `38.8` | `84.9` | `25.8` |
| **Dense Cluster (30 nodes)** | `Coverage+ResMinimal` | `4.5 ± 0.5` | `100.0% ± 0.0%` | `100.0% ± 0.0%` | `25.0` | `39.8` | `91.5` | `25.5` |
| **Sparse Line (10 nodes)** | `NaiveFlood` | `6.4 ± 2.5` | `66.7% ± 32.0%` | `77.1% ± 35.4%` | `76.2` | `150.0` | `4.5` | `0.0` |
| **Sparse Line (10 nodes)** | `AdaptiveFlood` | `6.7 ± 2.3` | `70.6% ± 29.7%` | `81.4% ± 32.6%` | `148.0` | `315.2` | `4.8` | `0.0` |
| **Sparse Line (10 nodes)** | `CoverageAware` | `7.0 ± 2.3` | `75.0% ± 29.2%` | `85.7% ± 32.6%` | `160.0` | `337.2` | `4.9` | `0.0` |
| **Sparse Line (10 nodes)** | `Coverage+ResBalanced` | `6.6 ± 2.4` | `69.4% ± 31.0%` | `80.0% ± 34.2%` | `145.2` | `312.2` | `4.5` | `0.0` |
| **Sparse Line (10 nodes)** | `Coverage+ResMinimal` | `7.2 ± 2.1` | `77.8% ± 27.0%` | `88.6% ± 30.2%` | `197.8` | `421.0` | `5.0` | `0.0` |
| **Random Geometric (50 nodes)** | `NaiveFlood` | `48.9 ± 2.9` | `99.3% ± 2.4%` | `99.5% ± 2.2%` | `88.8` | `147.5` | `362.9` | `0.0` |
| **Random Geometric (50 nodes)** | `AdaptiveFlood` | `24.5 ± 3.0` | `97.1% ± 5.8%` | `97.5% ± 5.4%` | `214.5` | `391.5` | `131.3` | `22.9` |
| **Random Geometric (50 nodes)** | `CoverageAware` | `28.9 ± 2.5` | `99.4% ± 1.5%` | `99.8% ± 0.9%` | `208.2` | `387.8` | `173.2` | `19.9` |
| **Random Geometric (50 nodes)** | `Coverage+ResBalanced` | `28.6 ± 1.8` | `99.4% ± 1.6%` | `99.8% ± 0.6%` | `202.0` | `364.0` | `171.0` | `20.5` |
| **Random Geometric (50 nodes)** | `Coverage+ResMinimal` | `28.9 ± 2.8` | `99.6% ± 1.2%` | `100.0% ± 0.0%` | `253.5` | `477.5` | `171.6` | `20.2` |
| **Partitioned Bridge (31 nodes)** | `NaiveFlood` | `28.7 ± 5.5` | `92.3% ± 18.3%` | `92.3% ± 18.3%` | `48.8` | `95.0` | `331.9` | `0.0` |
| **Partitioned Bridge (31 nodes)** | `AdaptiveFlood` | `4.9 ± 2.2` | `54.7% ± 19.0%` | `54.7% ± 19.0%` | `35.8` | `72.2` | `44.4` | `12.5` |
| **Partitioned Bridge (31 nodes)** | `CoverageAware` | `8.9 ± 1.7` | `92.3% ± 18.3%` | `92.3% ± 18.3%` | `150.2` | `345.2` | `78.6` | `19.8` |
| **Partitioned Bridge (31 nodes)** | `Coverage+ResBalanced` | `8.9 ± 2.0` | `89.8% ± 20.3%` | `89.8% ± 20.3%` | `143.2` | `336.5` | `79.0` | `19.1` |
| **Partitioned Bridge (31 nodes)** | `Coverage+ResMinimal` | `8.9 ± 1.8` | `92.3% ± 18.3%` | `92.3% ± 18.3%` | `201.2` | `453.5` | `78.8` | `19.8` |
| **Intermittent Connectivity (20 nodes)** | `NaiveFlood` | `18.6 ± 1.9` | `97.4% ± 6.3%` | `97.4% ± 6.3%` | `101.2` | `191.2` | `41.3` | `0.0` |
| **Intermittent Connectivity (20 nodes)** | `AdaptiveFlood` | `15.4 ± 3.5` | `91.8% ± 19.0%` | `91.8% ± 19.0%` | `209.8` | `417.0` | `31.5` | `2.4` |
| **Intermittent Connectivity (20 nodes)** | `CoverageAware` | `18.1 ± 1.2` | `99.2% ± 1.9%` | `99.2% ± 1.9%` | `214.5` | `427.8` | `39.6` | `1.6` |
| **Intermittent Connectivity (20 nodes)** | `Coverage+ResBalanced` | `17.9 ± 1.4` | `98.9% ± 2.7%` | `98.9% ± 2.7%` | `217.0` | `438.5` | `39.4` | `1.5` |
| **Intermittent Connectivity (20 nodes)** | `Coverage+ResMinimal` | `17.5 ± 1.5` | `98.7% ± 3.7%` | `98.7% ± 3.7%` | `273.8` | `575.2` | `35.8` | `1.6` |
| **Grid (7x7, 49 nodes) [Historical]** | `NaiveFlood` | `33.0 ± 1.4` | `78.1% ± 1.9%` | `100.0% ± 0.0%` | `132.5` | `200.0` | `70.1` | `0.0` |
| **Grid (7x7, 49 nodes) [Historical]** | `AdaptiveFlood` | `30.2 ± 1.8` | `77.8% ± 2.7%` | `100.0% ± 0.0%` | `275.0` | `437.8` | `59.8` | `1.9` |
| **Grid (7x7, 49 nodes) [Historical]** | `CoverageAware` | `32.9 ± 1.0` | `79.0% ± 0.6%` | `100.0% ± 0.0%` | `275.0` | `439.2` | `69.0` | `0.3` |
| **Grid (7x7, 49 nodes) [Historical]** | `Coverage+ResBalanced` | `32.7 ± 1.4` | `78.6% ± 1.8%` | `100.0% ± 0.0%` | `270.0` | `443.0` | `68.9` | `0.2` |
| **Grid (7x7, 49 nodes) [Historical]** | `Coverage+ResMinimal` | `32.0 ± 1.7` | `77.8% ± 1.8%` | `100.0% ± 0.0%` | `329.2` | `566.8` | `66.6` | `0.2` |

---

## 4. Unicast E2E Delivery Retry & Warm vs Fresh Session Ablation (`2-Hop Relay`, `20` Seeds)

Each run sends `5` unicast E2E Noise-encrypted `ApplicationFrame` messages across a 2-hop relay chain (`A <-> Relay <-> B`, `25ms` per-hop link delay, `311 B` padded encrypted wire envelope per data/ACK frame), comparing `NoRetry (max_attempts = 1)` against `WithRetry (max_attempts = 3)` across **Warm Sessions** (session pre-established) and **Fresh Sessions** (3-flight Noise XX handshake included under link loss):

| Session Workload | Link Loss | Retry Policy | Delivery Before Expiry (`mean ± sd`) | Handshake Tx (`mean`) | Retries (`mean`) | Total Wire Bytes (`mean`) | Duplicate Wire Rx (`mean`) | Duplicate UI Displays (`total`) | Median RTT Latency (`ms`) | P95 RTT Latency (`ms`) |
|---|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| **WarmSession** | `5%` | `NoRetry(1)` | `83.0% ± 19.3%` | `0.0` | `0.00` | `5,800` | `0.00` | `0` | `100.0` | `100.0` |
| **WarmSession** | `5%` | `WithRetry(3)` | `99.0% ± 4.4%` | `0.0` | `1.15` | `7,075` | `0.50` | `0` | `106.0` | `184.0` |
| **WarmSession** | `20%` | `NoRetry(1)` | `52.0% ± 24.0%` | `0.0` | `0.00` | `4,945` | `0.00` | `0` | `90.0` | `90.0` |
| **WarmSession** | `20%` | `WithRetry(3)` | `86.0% ± 14.3%` | `0.0` | `3.80` | `8,273` | `0.95` | `0` | `130.0` | `274.0` |
| **FreshSession** | `5%` | `NoRetry(1)` | `64.0% ± 35.0%` | `3.6` | `0.00` | `5,080` | `0.00` | `0` | `80.0` | `80.0` |
| **FreshSession** | `5%` | `WithRetry(3)` | `90.0% ± 30.0%` | `3.6` | `1.10` | `6,970` | `0.50` | `0` | `90.0` | `204.0` |
| **FreshSession** | `20%` | `NoRetry(1)` | `20.0% ± 28.3%` | `5.3` | `0.00` | `2,846` | `0.00` | `0` | `40.0` | `40.0` |
| **FreshSession** | `20%` | `WithRetry(3)` | `45.0% ± 42.4%` | `5.4` | `2.30` | `5,628` | `0.70` | `0` | `79.0` | `151.0` |

### Key Ablation Findings (Including Negative Results)
- **End-to-End Retry Gain**: On established sessions (`WarmSession`), bounded 3-attempt retry lifts delivery from **`83.0%` to `99.0%`** at `5%` link loss (`+16.0` points) and from **`52.0%` to `86.0%`** at `20%` link loss (`+34.0` points).
- **Lost-ACK Duplicate Suppression**: Retries triggered by dropped ACKs cause `0.50–0.95` duplicate wire receptions at `B`, all of which are suppressed by `InboxDeduplicator` (`dupUI = 0`) while still re-emitting the authenticated end-to-end ACK so sender `A` converges to `Delivered`.
- **Fresh-Session Multi-Hop Loss Sensitivity (Negative Result)**: Under severe `20%` per-hop link loss across a 2-hop relay (`36%` one-way drop probability per flight across 3 handshake flights + data + ACK), `FreshSession` delivery reaches `45.0% ± 42.4%` (`WithRetry(3)`) vs `20.0% ± 28.3%` (`NoRetry(1)`), demonstrating why persistent session caching and exponential outbox backoff are necessary on lossy multi-hop links.

---

## 5. Experimental Cover Traffic Evaluation (`60s` Window, `20` Seeds)

Evaluated over a 60-second window with `5` real messages sent at irregular timestamps (`t = 4.5s, 13.0s, 29.0s, 41.0s, 52.0s`) and `CoverTrafficConfig` (`min_interval = 1500ms`, `max_interval = 4500ms`, `max_bytes_per_minute = 2048 B`):

| Mode | Real Frames | Dummy Frames (`mean ± sd`) | Total Wire Bytes (`mean`) | Dummy Wire Overhead (`B`) | Mean Inter-Frame Gap (`ms`) | Inter-Frame Gap CV | Mean Real-Message Latency (`ms`) |
|---|---:|---:|---:|---:|---:|---:|---:|
| `CoverOFF` | `5.0` | `0.0 ± 0.0` | `1,555` | `0` | `11,875.0` | `0.227` | `25.0` |
| `CoverON` | `5.0` | `17.4 ± 2.4` | `6,966` | `5,411` | `2,391.1` | `1.371` | `25.0` |

- **Observation & Limitation**: `CoverON` pads idle gaps on a negotiated direct link with `17.4 ± 2.4` indistinguishable `311 B` encrypted envelopes per minute while adding **`0.0ms` latency penalty** to real messages (`has_real_pending` preempts dummy generation) and staying strictly below the `2,048 B/min` inner dummy cap. However, `CoverON` increases idle direct-link byte volume by `4.48x` and does **not** conceal online radio presence (`0xFD6F`), handshake setup, or multi-hop routing metadata; it therefore remains **Experimental / Default OFF**.

---

## 6. Experimental Conversation Sync Evaluation (`IdInventory` vs Bounded `Merkle`)

| Set Size (`n`) | Overlap (`%`) | `IdInventory` Wire Bytes | `IdInventory` Rounds | `IdInventory` Outcome | `Merkle` Wire Bytes | `Merkle` Rounds | `Merkle` Nodes Queried | `Merkle` Outcome |
|---:|---:|---:|---:|---|---:|---:|---:|---|
| `0` | `100%` | `202` | `3` | `CONVERGED` | `229` | `3` | `0` | `CONVERGED` |
| `8` | `100%` | `347` | `3` | `CONVERGED` | `229` | `3` | `0` | `CONVERGED` |
| `8` | `95%` | `1,037` | `5` | `CONVERGED` | `1,368` | `6` | `1` | `CONVERGED` |
| `8` | `50%` | `2,549` | `5` | `CONVERGED` | `2,880` | `6` | `1` | `CONVERGED` |
| `8` | `0%` | `4,562` | `5` | `CONVERGED` | `4,893` | `6` | `1` | `CONVERGED` |
| `32` | `100%` | `782` | `3` | `CONVERGED` | `229` | `3` | `0` | `CONVERGED` |
| `32` | `95%` | `1,964` | `5` | `CONVERGED` | `3,476` | `13` | `8` | `CONVERGED` |
| `32` | `50%` | `9,212` | `7` | `CONVERGED` | `11,601` | `18` | `11` | `CONVERGED` |
| `32` | `0%` | `17,642` | `11` | `CONVERGED` | `20,031` | `22` | `11` | `CONVERGED` |
| `128` | `100%` | `2,595` | `3` | `CONVERGED` | `229` | `3` | `0` | `CONVERGED` |
| `128` | `95%` | `6,318` | `5` | `CONVERGED` | `12,005` | `38` | `33` | `CONVERGED` |
| `128` | `50%` | `36,315` | `19` | `CONVERGED` | `45,195` | `62` | `43` | `CONVERGED` |
| `128` | `0%` | `70,035` | `35` | `CONVERGED` | `80,166` | `84` | `49` | `CONVERGED` |
| `256` | `100%` | `5,061` | `5` | `CONVERGED` | `229` | `3` | `0` | `CONVERGED` |
| `256` | `95%` | `11,970` | `7` | `CONVERGED` | `22,430` | `71` | `64` | `CONVERGED` |
| `256` | `50%` | `72,501` | `35` | `CONVERGED` | `90,971` | `126` | `91` | `CONVERGED` |
| `256` | `0%` | `139,941` | `67` | `CONVERGED` | `158,539` | `158` | `91` | `CONVERGED` |
| `512` | `100%` | `9,993` | `9` | `CONVERGED` | `229` | `3` | `0` | `CONVERGED` |
| `512` | `95%` | `23,820` | `13` | `CONVERGED` | `42,711` | `129` | `118` | `CONVERGED` |
| `512` | `50%` | `144,873` | `67` | `CONVERGED` | `182,243` | `251` | `184` | `CONVERGED` |
| `512` | `0%` | `263,779` | `122` | `QUOTA_ABORT_256KIB` | `268,001` | `285` | `179` | `QUOTA_ABORT_256KIB` |

- **Negative Result & Threshold Conclusion**: When differences are scattered across uniform random `logical_id` keys (`95%`, `50%`, `0%`), `Merkle` branch descent transmits more bytes and requires more round-trips than `IdInventory` at all tested sizes (`8..512`). `Merkle` is strictly superior only when confirming already-synchronized archives (`100%` overlap: `229 B` fixed cost vs `2,595 B–9,993 B`, an **11.3x–43.6x** reduction at `n >= 128`). Thus sync remains **Default OFF**, defaults to **`IdInventory`** when enabled, and retains **`Merkle`** as an explicit experimental mode.
