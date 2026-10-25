# Bitchat 2.0 Mesh Evaluation Report

## 1. Overview & Evaluation Setup

This evaluation quantifies routing efficiency, delivery reliability, latency distribution, and duplicate traffic for the mesh relay engine. We compare three routing strategies across five distinct network topologies under simulated 5% random link packet loss:

1. **`NaiveFlood` (Baseline)**: Every first-seen envelope is forwarded immediately to all reachable neighbors with zero artificial delay and no duplicate suppression ($k = \infty$).
2. **`AdaptiveFlood`**: Combines density-scaled forwarding delay ($D \in [0, D_{\max}(\text{neighbors})]$) with counter-based duplicate suppression ($k = 2$). Redundant retransmissions are cancelled once two duplicates confirm local neighborhood reception.
3. **`CoverageAware` (Phase 4)**: Combines density-scaled jitter with full neighborhood coverage tracking. Nodes cancel redundant rebroadcasts ONLY when overheard duplicate transmissions confirm that ALL local neighbors have been covered by peer transmissions. Stale, unverified, or incomplete coverage defaults to conservative forwarding.

### Simulation Parameters
- **Topologies**: 5 distinct network graph models (Line 10, Grid 7x7, Dense Cluster 30, Random Geometric 50, Partitioned Bridge 31).
- **Seeds**: 20 fixed pseudo-random seeds (1000..1019) per topology/policy combination (300 total simulated runs).
- **Link Loss Rate**: 5% uniform packet drop per hop.
- **Link Latency**: Zero-overhead discrete-event time stepping (10ms discrete time slices).
- **Raw Data**: Full per-seed metrics recorded in `evaluation_results.csv`.

---

## 2. Empirical Results Summary

| Topology | Policy | Avg Transmissions | Delivery % | Mean Latency | P95 Latency | Avg Duplicates | Relays Suppressed |
| :--- | :--- | :---: | :---: | :---: | :---: | :---: | :---: |
| **Line (10 nodes)** | `NaiveFlood` | 6.7 | 71.1% | 9.0 ms | 9.0 ms | 4.7 | 0.0 |
| | `AdaptiveFlood` | 6.1 | 62.8% | 82.1 ms | 157.5 ms | 4.2 | 0.0 |
| | `CoverageAware` | 6.8 | 72.2% | 98.1 ms | 185.5 ms | 4.8 | 0.0 |
| **Grid (7x7, 49 nodes)** | `NaiveFlood` | 33.1 | 78.3% | 10.0 ms | 10.0 ms | 70.3 | 0.0 |
| | `AdaptiveFlood` | 30.7 | 78.5% | 135.2 ms | 245.0 ms | 60.9 | 2.2 |
| | `CoverageAware` | 31.9 | 77.8% | 151.1 ms | 274.5 ms | 66.8 | 0.3 |
| **Dense Cluster (30 nodes)** | `NaiveFlood` | 30.0 | 100.0% | 10.0 ms | 10.0 ms | 768.3 | 0.0 |
| | `AdaptiveFlood` | 4.4 | 100.0% | 10.6 ms | 15.5 ms | 88.7 | 25.6 |
| | `CoverageAware` | 4.3 | 100.0% | 10.5 ms | 14.5 ms | 86.4 | 25.7 |
| **Random Geometric (50 nodes)** | `NaiveFlood` | 49.0 | 99.3% | 10.0 ms | 10.0 ms | 363.4 | 0.0 |
| | `AdaptiveFlood` | 24.8 | 97.2% | 129.3 ms | 258.5 ms | 135.8 | 22.5 |
| | `CoverageAware` | 28.4 | 99.5% | 125.2 ms | 244.5 ms | 170.9 | 20.4 |
| **Partitioned Bridge (31 nodes)** | `NaiveFlood` | 27.9 | 89.7% | 10.0 ms | 10.0 ms | 321.9 | 0.0 |
| | `AdaptiveFlood` | 5.3 | 60.2% | 39.0 ms | 72.5 ms | 47.7 | 13.7 |
| | `CoverageAware` | 9.4 | **94.8%** | 130.5 ms | 248.0 ms | 84.7 | 20.0 |

---

## 3. Findings & Performance Analysis

### A. Partitioned Bridge: Resolution of Premature Bridge Suppression
- **Failure in AdaptiveFlood**: `AdaptiveFlood` suffered a severe delivery collapse down to **60.2%** (frequently delivering only to Cluster A nodes and starving Cluster B completely). Nodes in Cluster A heard duplicate broadcasts from peer cluster members and suppressed before the bridge node (node 15) received and forwarded into Cluster B.
- **CoverageAware Resolution**: `CoverageAware` evaluates whether overheard duplicate sources actually cover the node's full neighbor set. Because Cluster A members do not cover Bridge 15, Node 14's uncovered set remains non-empty, preventing premature suppression.
- **Empirical Breakthrough**: Delivery ratio jumped from **60.2% to 94.8%** (surpassing even `NaiveFlood`'s 89.7%), while cutting transmissions by **66.3%** compared to `NaiveFlood` (9.4 vs 27.9) and reducing duplicate receptions from 321.9 down to 84.7.

### B. Dense Cluster: Broadcast Storm Mitigation Preserved
- `CoverageAware` matches `AdaptiveFlood`'s full broadcast storm mitigation:
  - Transmissions reduced from **30.0 to 4.3** (an **85.7% reduction**).
  - Duplicate packet receptions slashed from **768.3 to 86.4** (an **88.8% reduction**).
  - **100.0% delivery reliability** sustained with minimal latency penalty (10.5 ms).

### C. Random Geometric Graph & Low-Density Topologies
- On the 50-node random geometric topology, `CoverageAware` achieves **99.5% delivery** (compared to 97.2% for `AdaptiveFlood`) while maintaining a 42% transmission reduction relative to naive flooding.
- On low-degree networks (Line 10), suppression correctly avoids triggering, preserving reachability across multi-hop chains (72.2% delivery under 5% per-hop loss).

---

## 4. Limits of this Evaluation

1. **Discrete-Event Simulation**: Executed in `SimNetwork` using virtual event stepping. BLE advertising interval collisions, hardware radio sleep cycles, and OS Bluetooth queue drops are abstracted into a uniform 5% per-hop loss model.
2. **Static Topologies**: Evaluated against fixed graph structures. Dynamic mobile churn and physical RSSI variations are addressed in Phase 4 Part B and Phase 5.
