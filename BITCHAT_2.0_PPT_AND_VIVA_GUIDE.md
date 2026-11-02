# Bitchat 2.0: 15-Minute Project Presentation & Technical Viva Guide

---

## Session Time Allocation Matrix

| Activity | Allocated Time | Target Content |
| :--- | :--- | :--- |
| **1. PPT Presentation** | **5 Minutes** | Problem, Objectives, Solution, Architecture, Progress, SDGs (8 Slides) |
| **2. Live Prototype Demo** | **6 Minutes** | Direct BLE, Offline Outbox Recovery, Multi-Hop Relay, Iroh QUIC Tunnel, Diagnostics |
| **3. Technical Viva** | **4 Minutes** | Cryptography, Routing Algorithms, Storage Security, Fault Tolerance, Edge Cases |
| **Total** | **15 Minutes** | Strict Adherence Required |

---

## Part 1: PPT Presentation Deck (5 Minutes / 8 Slides)

```
[Slide 1: Title] ─────> [Slide 2: Problem] ─────> [Slide 3: Solution] ─────> [Slide 4: Architecture]
   (30 sec)                 (45 sec)                  (45 sec)                  (45 sec)
       │
       ▼
[Slide 5: Implementation] ──> [Slide 6: Progress] ──> [Slide 7: Sustainability] ──> [Slide 8: Conclusion]
   (45 sec)                     (30 sec)                  (30 sec)                     (30 sec)
```

### Slide 1: Title & Overview (0:00 – 0:30)
- **Title**: **Bitchat 2.0: Zero-Trust Resilient Mesh Communication System**
- **Subtitle**: Hybrid Offline BLE Multi-Hop Mesh & P2P Iroh QUIC Gateway
- **Presenter**: [Your Name / Roll No / Degree]
- **Core Value Proposition**: Off-grid, serverless, end-to-end encrypted messaging that works during complete cellular/internet blackouts and seamlessly bridges over wide-area P2P tunnels when connectivity returns.

### Slide 2: Problem Statement & Motivation (0:30 – 1:15)
- **The Vulnerability of Centralization**:
  - Natural disasters (hurricanes, earthquakes, floods), power grid failures, and remote expeditions instantly incapacitate cellular towers and cloud servers.
  - Traditional emergency communication requires expensive, power-hungry satellite equipment.
- **Failures of Existing Ad-Hoc Solutions**:
  - **Broadcast Storms**: Naive flooding causes 90%+ radio collisions and battery drain.
  - **Ephemeral-Only**: Messages drop permanently if the recipient is 10 meters away or momentarily out of RF range.
  - **Security Deficits**: Plaintext metadata, spoofed relay attacks, lack of forward secrecy.
  - **Transport Silos**: Inability to seamlessly transition between localized offline Bluetooth and remote internet.

### Slide 3: Proposed Solution & Key Objectives (1:15 – 2:00)
- **Hybrid Dual-Transport Architecture**:
  1. **Sub-GHz / BLE Mesh**: Decentralized multi-hop ad-hoc networking for zero-infrastructure offline communication.
  2. **Iroh QUIC Gateway**: P2P wide-area tunneling with NAT hole-punching for long-distance bridging without central servers.
- **Key Technical Objectives**:
  - **Coverage-Aware Adaptive Routing**: Mitigate broadcast storms while maintaining 95%+ delivery over partitioned bridges.
  - **Hardware-Backed Outbox Store-and-Forward**: Monotonic delivery state machine backed by Android Keystore hardware-encrypted SQLite.
  - **Zero-Trust Cryptographic Core**: Noise XX protocol (ChaCha20-Poly1305 + Curve25519) + Ed25519 identity bindings.
  - **Dual Privacy Modes**: Normal Mode (encrypted durable disk) vs Privacy Mode (strict volatile memory zero-disk guarantee).

### Slide 4: System Architecture & Data Flow (2:00 – 2:45)
- **Three-Tier Architecture**:
  - **UI Tier (Flutter)**: Reactive delivery badges (`Queued`, `InFlight`, `Delivered`, `Failed`), Diagnostics sheet, QR Cloud Sync.
  - **Platform Tier (Android Native)**: Foreground `BitchatBleService.kt` (service-owned runtime) + `AndroidKeystoreProvider.kt` (hardware AES-GCM DEK).
  - **Core Engine (Rust Native - `bitchat2_core` via FFI)**:
    - `crypto`: Noise XX handshake, directional keys, 64-entry sliding replay window.
    - `envelope`: Versioned binary framing, bucket padding (64/128/256/512B) preventing traffic analysis.
    - `mesh`: Coverage-Aware Flood Router, Bloom filter + LRU seen cache, 16-chunk fragmenter.
    - `storage`: ChaCha20-Poly1305 encrypted SQLite outbox with exponential backoff daemon.
    - `gateway`: Authenticated `RouteSnapshot` distribution with token-bucket fairness limiter (64 KB/s cap).

### Slide 5: Core Engineering Innovations (2:45 – 3:30)
- **1. Coverage-Aware Flood Routing**:
  - Dynamically calculates relay probability based on known neighbor density.
  - Cuts duplicate transmissions by **88.8%** in dense clusters; increases partitioned bridge delivery from 60.2% to **94.8%**.
- **2. Hardware-Backed Monotonic Outbox**:
  - Android Keystore AES-GCM wraps a 256-bit Data Encryption Key (DEK).
  - Outbox state machine: `Queued -> InFlight -> {Delivered, Failed, Expired}` with terminal immutability.
  - Auto-retries with exponential backoff (1s..15s) and instant wakeup upon BLE peer discovery.
- **3. Cryptographic Identity Binding & Route Authentication**:
  - Ed25519 signed `RouteSnapshot` frames with domain separation `b"bitchat2-route-snapshot-v1"`.
  - Filtered self-route reflection preventing loopbacks.

### Slide 6: Implementation Progress & Verification (3:30 – 4:00)
- **Current Status**: **100% Production Ready** across Phase 1 to Phase 4.
- **Automated Verification**:
  - **122 Rust Unit & Integration Tests**: 100% passing (`cargo test --all`).
  - **SimNetwork Testbeds**: 300-run Monte Carlo loss simulations, 4-node real Iroh loopback E2E test.
  - **Clean Builds**: Android Gradle (`compileDebugKotlin`), Flutter static analysis (0 warnings).
- **Physical Multi-Device Testbed**:
  - Deployed on 3 physical devices: OnePlus Nord CE4 (Phone A), OnePlus Nord CE 3 Lite (Phone B), Redmi Note 10 5G (Phone C).
  - Verified 1-hop transparent relay (A -> B -> C), offline outbox recovery, Iroh QUIC tunnels, and mixed BLE+Iroh routing.

### Slide 7: Sustainability & UN SDG Relevance (4:00 – 4:30)
- **UN SDG 9: Industry, Innovation, and Infrastructure**:
  - Provides resilient, decentralized communication infrastructure that survives severe disasters and grid collapse.
- **UN SDG 11: Sustainable Cities and Communities**:
  - Disaster Risk Reduction (DRR): Life-saving peer-to-peer coordination during floods, earthquakes, and emergency evacuations when public infrastructure fails.
- **UN SDG 12 & Green Computing**:
  - **Ultra-Low Energy**: BLE Tx/Rx consumes **~10–15 mW** vs **1,000–2,000 mW** for active LTE/5G cellular radios—extending phone battery life from hours to days during emergency power blackouts.
  - **Zero Server Footprint**: 100% serverless P2P architecture eliminates carbon-intensive centralized data centers.

### Slide 8: Individual Contribution & Conclusion (4:30 – 5:00)
- **My Individual Contribution**:
  - Designed and implemented the complete Rust core engine (`bitchat2_core`): cryptographic framing, outbox store, router policy, and Iroh gateway bridge.
  - Implemented Android Keystore hardware DEK integration and low-level BLE GATT foreground service.
  - Integrated Flutter UI with reactive outbox state tracking, diagnostics bottom sheet, and QR cloud sync.
  - Executed end-to-end multi-device physical field tests across 3 Android smartphones.
- **Future Scope**:
  - Audio/voice note chunking over mesh.
  - Cross-platform iOS CoreBluetooth binding.

---

## Part 2: Live Working Prototype Demonstration Script (6 Minutes)

> [!IMPORTANT]
> Keep 3 phones on the table in front of the jury:
> - **Phone A**: Sender (OnePlus Nord CE4)
> - **Phone B**: Middle Relay / Gateway (OnePlus Nord CE 3 Lite)
> - **Phone C**: Destination (Redmi Note 10 5G)

```
Demo Timeline (6 Minutes Total):
0:00 ─── [Step 1: Direct BLE] ───> 1:00 ─── [Step 2: Store-and-Forward Outbox] ───> 2:30
                                                                                        │
3:45 <── [Step 4: Iroh Gateway] <── 2:30 <── [Step 3: Multi-Hop Line Relay] <─────────┘
  │
  └───> 4:45 ─── [Step 5: Hybrid Transport] ───> 5:30 ─── [Step 6: Diagnostics & Privacy] ───> 6:00
```

### Step 1: Zero-Infrastructure Direct BLE Chat (0:00 – 1:00)
- **Action**:
  1. Show Phone A and Phone B in Airplane Mode with only Bluetooth enabled (Wi-Fi OFF, Mobile Data OFF).
  2. Point out peer ShortId discovery in the top peer bar.
  3. Send message from Phone A: `"Direct mesh test"`.
- **Observation to Highlight**:
  - Instant delivery.
  - Double green checkmark (`Delivered`) appears on Phone A within milliseconds.
  - Phone B displays message with verified sender identity.
  - *Jury takeaway*: No internet, no router, zero infrastructure.

### Step 2: Store-and-Forward Offline Outbox Recovery (1:00 – 2:30)
- **Action**:
  1. Turn Bluetooth OFF on Phone B (Receiver is now completely disconnected/offline).
  2. Send message from Phone A: `"Urgent: Flood rescue at Sector 4"`.
  3. Point to message badge on Phone A: Shows grey single checkmark (`InFlight`) with active retry timer.
  4. Explain to jury: *"In naive chat apps, this message drops permanently. In Bitchat 2.0, the message is durably committed to encrypted SQLite storage."*
  5. Turn Bluetooth back ON on Phone B.
- **Observation to Highlight**:
  - Instant event trigger (`on_peer_connected`) wakes up Phone A's outbox pump daemon.
  - Message automatically transmits, Phone B rings/displays the message, and Phone A's badge updates immediately to double green checkmark (`Delivered`).

### Step 3: Multi-Hop Mesh Relay (1-Hop Delivery) (2:30 – 3:45)
- **Action**:
  1. Place Phone A, Phone B, and Phone C in a line: Phone A is out of direct RF reach of Phone C (or separated by distance/attenuation).
  2. Send message from Phone A to Phone C: `"Routing via B"`.
- **Observation to Highlight**:
  - Phone B silently forwards the encrypted envelope without being able to read the inner payload (Noise session is end-to-end between A and C).
  - Phone C receives the message and displays the hop badge: **`1h`** (1 hop relay).
  - Phone C emits an authenticated Noise ACK that routes back to Phone A, updating Phone A to `Delivered`.

### Step 4: P2P Iroh QUIC Gateway (Cloud Sync) (3:45 – 4:45)
- **Action**:
  1. Open **Cloud Sync** dialog on Phone B. Show QR code containing Iroh Node ID and Ed25519 identity.
  2. Enable Wi-Fi or Mobile Hotspot on Phone B and Phone C.
  3. Scan Phone B's QR code using Phone C's camera.
- **Observation to Highlight**:
  - Direct P2P QUIC connection established over Iroh via NAT hole-punching.
  - Dialog status live-updates: `Connected to Gateway via QR` with QUIC Conns = 1.
  - Show that Phone C now learns routes to peers connected to Phone B.

### Step 5: Hybrid Transport & Loop-Free Clean Routing (4:45 – 5:30)
- **Action**:
  1. Turn Bluetooth ON alongside the active Iroh tunnel.
  2. Show Gateway Peers bar on both devices.
- **Observation to Highlight**:
  - Highlight that self ShortId is cleanly filtered out (no route loops).
  - The router policy automatically prioritizes direct BLE neighbors to preserve bandwidth, but falls back to the Iroh tunnel if RF is obstructed.

### Step 6: Diagnostics Bottom Sheet & Privacy Mode (5:30 – 6:00)
- **Action**:
  1. Tap the `(i)` Diagnostics button on the top app bar.
  2. Show live telemetry: ShortId, PeerId, Outbox Queued/In-Flight counters, Transmitted vs Suppressed packets, Seen cache stats.
  3. Toggle **Privacy Mode**: Show the shield icon.
  4. Explain: *"In Privacy Mode, the outbox switches to volatile in-memory heap storage—zero disk writes occur, guaranteeing zero forensic artifacts if a device is confiscated."*

---

## Part 3: Technical Viva & Jury Q&A Cheat Sheet (4 Minutes)

### Q1: Why did you write the core in Rust instead of Kotlin/Java?
- **Answer**:
  1. **Memory Safety & No Garbage Collection**: Eliminates Android GC pauses, which cause packet drops and timing jitter in real-time BLE GATT streaming.
  2. **Zero-Cost Abstractions**: Rust compiles down to native machine code (`libbitchat2_core.so`), achieving high cryptographic throughput on low-end ARM Cortex cores.
  3. **Cross-Platform Cryptographic Core**: The exact same battle-tested cryptographic engine runs on Android, Linux, desktop simulations, and future iOS ports with zero code duplication.

### Q2: What cryptographic protocols ensure end-to-end security?
- **Answer**:
  - **Handshake**: **Noise XX** pattern (`-> e`, `<- e, ee, s, es`, `-> s, se`), providing mutual authentication without transmitting static public keys in plaintext.
  - **Symmetric Encryption**: **ChaCha20-Poly1305** AEAD with separate directional keys (Initiator-to-Responder and Responder-to-Initiator) to prevent reflection attacks.
  - **Replay Protection**: A **64-packet sliding window bitmask** with monotonically checked attempt IDs. High nonces cannot advance the window unless the Poly1305 authentication tag verifies.
  - **Identity**: Ed25519 for public keys and ShortId derivation (`SHA256(pubkey)[..8]`).

### Q3: How do you prevent Broadcast Storms in dense mesh networks?
- **Answer**:
  - We implemented **Coverage-Aware Adaptive Flooding** (`core/src/mesh/policy.rs`):
    1. Each node maintains a bounded neighbor table updated via passive beaconing.
    2. Before relaying a flood envelope, the node calculates its neighbor overlap. In dense clusters, nodes apply a deterministic suppression probability and adaptive jitter delay (50–350ms).
    3. We use a **SeenCache** combining an **LRU buffer (500 items)** and a **Bloom Filter (2048 bits, 4 hashes)**. Duplicate message IDs are immediately dropped at the link boundary.
    4. Evaluated results: Cuts duplicate packet transmissions by **88.8%** while maintaining **94.8%** delivery over bottleneck bridges.

### Q4: How is the outbox store secured against physical phone theft?
- **Answer**:
  - We use hardware-backed Android Keystore (`AndroidKeystoreProvider.kt`):
    1. The Android Keystore generates an AES-GCM-256 Key Encryption Key (KEK) inside the Secure Element / TEE.
    2. The KEK encrypts a random 256-bit Data Encryption Key (DEK).
    3. The SQLite database stores message bodies encrypted with ChaCha20-Poly1305 using this DEK.
    4. The AAD (Additional Authenticated Data) binds schema version, key version, logical ID, sender, recipient, and storage mode.
    5. Backups are disabled via `android:allowBackup="false"`. If the device is locked, attempts to read throw `KeystoreLockedException`.

### Q5: What is Iroh, and why use QUIC instead of standard WebSockets or TCP?
- **Answer**:
  - **Iroh** is a modern P2P networking library built on QUIC (over UDP) and Rust.
  - **Advantages over TCP/WebSockets**:
    1. **Built-in Hole Punching**: Traverses symmetric NATs and firewalls without requiring users to configure port forwarding.
    2. **Decentralized Relay Fallback (DERP)**: If direct P2P hole-punching fails, packets relay through authenticated relay nodes without decrypting payload traffic.
    3. **Connection Migration**: QUIC connections survive Wi-Fi to Cellular handoffs because connections are identified by 64-bit Connection IDs rather than IP/port 4-tuples.

### Q6: How does Bitchat 2.0 prevent routing loops when BLE and Iroh run together?
- **Answer**:
  - In `core/src/gateway/bridge.rs` and `core/src/api.rs`:
    1. When broadcasting `RouteSnapshot` frames over Iroh, the sending node explicitly filters out its own local `ShortId` and the remote session's `ShortId`.
    2. Inbound snapshot ingestion verifies Ed25519 signatures and discards any route claiming reachability for `my_short`.
    3. This breaks the reflection cycle and keeps the Gateway Route Table strictly loop-free.

### Q7: What are the main limitations and what is your future roadmap?
- **Answer**:
  - **Current Limitations**: Max payload chunking is currently tuned for text messages and small payloads (up to 16 fragments x 460 bytes); BLE throughput restricts high-bitrate video.
  - **Future Roadmap**:
    1. Opus audio codec compression for push-to-talk voice notes over mesh.
    2. iOS platform integration using CoreBluetooth.
    3. BLE Extended Advertising (Bluetooth 5.0) for larger advertising payloads.

---

## Part 4: Sustainability & UN SDG Deep-Dive

### UN Sustainable Development Goals (SDG) Alignment

```
┌────────────────────────────────────────────────────────────────────────┐
│                      UN SDG Impact Dimensions                          │
├─────────────────────────┬──────────────────────┬───────────────────────┤
│       SDG 9             │       SDG 11         │       SDG 12          │
│ Industry, Innovation &  │ Sustainable Cities & │ Responsible           │
│ Infrastructure          │ Communities          │ Consumption           │
├─────────────────────────┼──────────────────────┼───────────────────────┤
│ • Zero-infrastructure   │ • Disaster Risk      │ • Ultra-low power     │
│   resilience            │   Reduction (DRR)    │   (10-15 mW BLE vs    │
│ • Resilient distributed │ • Emergency rescue   │   1,000-2,000 mW LTE) │
│   communications        │   and citizen mesh   │ • Serverless compute  │
└─────────────────────────┴──────────────────────┴───────────────────────┘
```

1. **SDG 9: Resilient Infrastructure**:
   - Centralized telecommunications infrastructure has single points of failure. In 2023–2024 alone, extreme weather events caused over 4,000 telecommunication outages worldwide.
   - Bitchat 2.0 turns off-the-shelf consumer smartphones into self-healing, decentralized ad-hoc communication grids that require zero base stations or cell towers.

2. **SDG 11: Sustainable and Resilient Communities**:
   - Enables community disaster response teams, first responders, and citizens to coordinate evacuation routes, medical triage, and resource distribution during earthquakes, floods, and tsunamis when cellular networks are down.

3. **SDG 12 & Green Computing (Resource Efficiency)**:
   - **Radio Energy Consumption**:
     - 4G/5G Cellular Radio Tx: **1,200 mW – 2,000 mW**.
     - Standard Wi-Fi Tx: **400 mW – 800 mW**.
     - **Bluetooth Low Energy (BLE) Tx**: **10 mW – 15 mW** (over **98% energy reduction**).
   - In extended power blackouts where grid electricity is unavailable for days, Bitchat 2.0 allows standard smartphone batteries to last 48–72 hours while actively participating in an emergency mesh.
   - **Zero Cloud Footprint**: Completely eliminates recurring compute and storage emissions associated with centralized cloud server farms (AWS, Firebase, Azure).
