# Bitchat 2.0 Mesh Evaluation Report

## 1. Overview & Evaluation Setup

This evaluation quantifies routing efficiency, delivery reliability, latency distribution, and duplicate traffic for the mesh relay engine. We compare two routing strategies across five distinct network topologies under simulated 5% random link packet loss:

1. **`NaiveFlood` (Baseline)**: Every first-seen envelope is forwarded immediately to all reachable neighbors with zero artificial delay and no duplicate suppression ($k = \infty$).
2. **`AdaptiveFlood`**: Combines density-scaled forwarding delay ($D \in [0, D_{\max}(\text{neighbors})]$) with counter-based duplicate suppression ($k = 2$). Redundant retransmissions are cancelled once two duplicates confirm local neighborhood coverage.

### Simulation Parameters
- **Topologies**: 5 distinct network graph models.
- **Seeds**: 20 fixed pseudo-random seeds (1000..1019) per topology/policy combination (200 total simulated runs).
- **Link Loss Rate**: 5% uniform packet drop per hop.
- **Link Latency**: Zero-overhead discrete-event time stepping (10ms discrete time slices).
- **Raw Data**: Full per-seed metrics recorded in `evaluation_results.csv`.

---

## 2. Empirical Results Summary

| Topology | Policy | Avg Transmissions | Delivery % | Mean Latency | P95 Latency | Avg Duplicates | Relays Suppressed |
| :--- | :--- | :---: | :---: | :---: | :---: | :---: | :---: |
| **Line (10 nodes)** | `NaiveFlood` | 6.7 | 70.0% | 9.0 ms | 9.0 ms | 4.7 | 0.0 |
| | `AdaptiveFlood` | 6.3 | 66.7% | 81.9 ms | 159.5 ms | 4.3 | 0.0 |
| **Grid (7x7, 49 nodes)** | `NaiveFlood` | 33.3 | 78.5% | 10.0 ms | 10.0 ms | 70.5 | 0.0 |
| | `AdaptiveFlood` | 30.1 | 77.3% | 149.0 ms | 259.0 ms | 59.6 | 2.3 |
| **Dense Cluster (30 nodes)** | `NaiveFlood` | 30.0 | 100.0% | 10.0 ms | 10.0 ms | 769.0 | 0.0 |
| | `AdaptiveFlood` | 4.6 | 100.0% | 10.9 ms | 17.5 ms | 94.0 | 25.4 |
| **Random Geometric (50 nodes)** | `NaiveFlood` | 49.3 | 99.8% | 10.0 ms | 10.0 ms | 366.3 | 0.0 |
| | `AdaptiveFlood` | 23.9 | 94.2% | 122.2 ms | 259.0 ms | 131.2 | 22.3 |
| **Partitioned Bridge (31 nodes)** | `NaiveFlood` | 27.1 | 87.2% | 10.0 ms | 10.0 ms | 313.1 | 0.0 |
| | `AdaptiveFlood` | 4.5 | 52.0% | 19.9 ms | 41.0 ms | 40.5 | 12.1 |

---

## 3. Findings & Performance Analysis

### A. Dense Cluster (30 nodes in single radio range)
- **Duplicate Explosion in Naive Flood**: In an unmanaged flood, every single node retransmits upon hearing the message, resulting in **30 transmissions and 769 duplicate packet receptions** across the cluster.
- **Suppression Efficiency**: `AdaptiveFlood` reduced transmissions from **30.0 to 4.6** (an **84.7% reduction** in over-the-air packets) and slashed duplicate receptions from **769.0 to 94.0** (an **87.8% drop**), while sustaining **100.0% delivery reliability**.
- **Latency Impact**: Delay overhead remained negligible (mean 10.9 ms vs 10.0 ms) because the first transmitting node covered the entire clique.

### B. Random Geometric Graph (50 nodes, $r = 0.28$)
- **Traffic Reduction**: Transmissions fell by **51.5%** (from 49.3 to 23.9 packets), and duplicate packet receptions decreased by **64.2%** (366.3 down to 131.2).
- **Delivery Tradeoff**: Delivery ratio declined slightly from 99.8% to 94.2% due to 5% link loss interacting with suppressed paths.
- **Latency Tradeoff**: Mean delivery latency increased from 10.0 ms to 122.2 ms (P95: 259.0 ms) as adaptive delays spread packet bursts across time windows.

### C. Partitioned Bridge (Two 15-node clusters connected by 1 bridge node)
- **Known Bottleneck & Premature Suppression**: In this topology, `AdaptiveFlood` delivered to only **52.0%** of nodes on average, compared to **87.2%** under `NaiveFlood`.
- **Root Cause**: Nodes in Cluster A heard multiple redundant transmissions from fellow Cluster A members and reached their suppression threshold ($k=2$) *before* the bridge node (node 15) successfully relayed into Cluster B, or the bridge node itself was suppressed.
- **Takeaway**: Counter-based suppression without topological awareness can bottleneck narrow graph cuts / bridges. Phase 5 device-class routing and bridge prioritization will address this behavior.

### D. Line (10 nodes) and Grid (7x7 nodes)
- In linear networks with low node degree (neighbor count $\le 2$), suppression rarely triggers ($0.0$ suppressed on line).
- On a 10-node line, delivery ratio dropped from 70.0% to 66.7% due to link loss compounded over 9 hops; latency scaled proportionally with hop delay windows.

---

## 4. Limits of this Evaluation

1. **Simulator-Only Model**: All tests were executed in `SimNetwork` using memory channels. Physical 2.4 GHz RF propagation, multipath fading, BLE advertising channel collisions (channels 37, 38, 39), and OS-level Bluetooth stack queuing delays are not modeled.
2. **Simplified Collision Approximation**: Collisions and interference are modeled purely via an independent 5% link loss probability per hop, rather than explicit simultaneous-transmission physical layer destructive interference.
3. **Static Topology**: Nodes are stationary. Mobile encounter churn, reconnection latencies, and BLE connection interval variations will be evaluated on physical devices during Phase 2 Part B and Phase 3.
