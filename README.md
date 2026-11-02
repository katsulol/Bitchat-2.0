# Bitchat 2.0

> **Decentralized, privacy-preserving mesh messenger combining dual-role Bluetooth Low Energy (BLE) multi-hop routing with internet-bridged Iroh P2P QUIC gateways.**

---

## Overview

**Bitchat 2.0** is an offline-first, zero-central-infrastructure messaging network designed to operate under complete internet blackout, local network disruption, or censorship. It pairs a high-performance **Rust core engine (`bitchat2_core`)** compiled to native ARM64 with an Android **Flutter** user interface and dual-role BLE GATT background service.

---

## Key Capabilities

### 1. Dual-Role BLE Mesh Routing
- **Multi-Hop Flooding**: Decentralized multi-hop frame propagation with hop-limit TTL enforcement (up to 7 hops).
- **Adaptive Relay Policy**: Density-scaled jitter delays and duplicate suppression reduce channel contention and power consumption.
- **Bloom-Filtered Deduplication**: Two-stage deduplication cache combining an LRU index with rotating Bloom filters prevents broadcast storms.
- **Coarse Proximity Estimation**: Link-layer RSSI values are categorized into coarse distance buckets (`Immediate <1.5m`, `Near 1.5–5m`, `Far >5m`) to defend against trilateration while maintaining local context.

### 2. End-to-End Cryptography & Identity
- **Ed25519 Node Identities**: Cryptographic self-sovereign identities derived from public keys; short 8-byte identifiers (`ShortId`) are used for compact routing headers.
- **Stateless Noise_XX Transport**: Mutual authenticated key exchange over untrusted relays without exposing long-term static public keys in plaintext.
- **Directional Session Keys**: Distinct transmission and reception symmetric encryption keys with AEAD ChaCha20-Poly1305.
- **Anti-Replay Window**: 64-bit sliding replay window prevents message duplication and replay attacks.

### 3. Store-and-Forward Guaranteed Delivery
- **Durable Local Outbox**: Messages targeted at disconnected or multi-hop peers are committed atomically to encrypted SQLite storage.
- **Cryptographic Delivery ACKs**: Receiving endpoints generate authentic signed acknowledgment frames upon successful decryption.
- **Adaptive Exponential Backoff**: Retries throttle exponentially with jitter, pausing automatically when peers are out of range and waking up upon physical reconnection.
- **Terminal Delivery State Machine**: Strict lifecycle state guarantees (`Queued` → `InFlight` → `Delivered` or `Expired`/`Failed`), preventing zombie retries.

### 4. Internet-Bridged Iroh P2P Gateways
- **NAT-Traversing QUIC Gateways**: Direct peer-to-peer tunnels powered by Iroh for long-distance connectivity across separate BLE mesh islands.
- **Mutual Consent Pairing**: Pairing is strictly opt-in via single-use 5-minute cryptographic invite tokens; unsolicited endpoints stay in pending state without access.
- **Route Snapshots**: Gateways advertise monotonic reachable peer snapshots with bounded epoch and sequence validation.
- **Privacy Mode Enforcement**: When enabled, internet gateway relaying is blocked, ensuring zero packets leave the physical local BLE mesh.

### 5. Scoped Peer Trust & 1-Hop Capability Vouchers
- **1-Hop Authorization**: Trusted peers can act as Introducers, issuing signed Ed25519 vouchers granting scoped capabilities (`DirectMessage`, `GatewayForward`) for 1 hop only.
- **Non-Transitive Security**: Introduced peers cannot re-delegate or vouch for others, eliminating cyclic vouch amplification and Sybil cascades.
- **Audience-Bound**: Vouchers specify exact target audience PeerIds, preventing replay on unintended nodes.
- **Cascade Revocation**: Blocking an introducer immediately invalidates all active capability vouchers issued by that peer.

### 6. Volatile Ephemeral Chat
- **Hardware-Enforced Screenshot Blocking**: Android `FLAG_SECURE` automatically shields screens from capture, screen recordings, and recent-app previews.
- **Zero Disk Persistence**: Volatile messages bypass disk SQLite entirely; encrypted payloads and keys reside strictly in volatile RAM.
- **Mutual Ephemeral Negotiation**: Chat partners negotiate time-to-live expiration (1m, 5m, 15m); expiring timers scrub volatile memory automatically.

### 7. Resource-Aware Relaying
- **Dynamic Hysteresis Profiles**: Continuously samples device power state (battery percentage, AC charging status, low-power save mode, and thermal state).
- **Graceful Throttling**: Automatically steps down from Full Relay to Conservative, Battery-Saver, or Emergency Mode, protecting low-battery phones from relay exhaustion.

---

## System Architecture

```
┌────────────────────────────────────────────────────────┐
│                   Flutter Application                  │
│       (Material UI, Reactive Streams, QR Scanner)      │
├────────────────────────────────────────────────────────┤
│                     Dart Bridge                        │
│            (CoreFfi FFI / Platform Channels)           │
├──────────────────────────┬─────────────────────────────┤
│   Android Native Layer   │         Rust Core           │
│  (Kotlin Foreground Svc, │     (bitchat2_core.so)      │
│   GATT Client/Server,    │  ┌────────────────────────┐ │
│   Battery/Thermal State) │  │  Identity & KeyStore   │ │
│                          │  ├────────────────────────┤ │
│                          │  │  Noise_XX Cryptography │ │
│                          │  ├────────────────────────┤ │
│                          │  │  BLE Router & Relay    │ │
│                          │  ├────────────────────────┤ │
│                          │  │  Store & Forward / ACK │ │
│                          │  ├────────────────────────┤ │
│                          │  │  Trust & Vouchers      │ │
│                          │  ├────────────────────────┤ │
│                          │  │  Iroh P2P Gateway      │ │
│                          │  └────────────────────────┘ │
└──────────────────────────┴─────────────────────────────┘
```

---

## Repository Structure

```
.
├── Cargo.toml                  # Cargo workspace definition
├── core/
│   ├── Cargo.toml              # bitchat2_core library definition
│   ├── src/
│   │   ├── lib.rs              # Library entry point & safety lints
│   │   ├── error.rs            # Unified error types (thiserror)
│   │   ├── identity.rs         # Ed25519 identity, PeerId, ShortId, KeyStore
│   │   ├── envelope.rs         # Binary wire codec & padding
│   │   ├── crypto.rs           # Stateless Noise_XX with sliding replay window
│   │   ├── trust.rs            # Scoped capability vouchers & trust store
│   │   ├── api.rs              # Native C / Flutter API interface
│   │   ├── ffi.rs              # C-ABI export bindings
│   │   ├── mesh/               # BLE fragmentation, caches, delivery, routing
│   │   │   ├── fragment.rs
│   │   │   ├── seen.rs
│   │   │   ├── relay.rs
│   │   │   ├── peer.rs
│   │   │   ├── delivery.rs
│   │   │   ├── router.rs
│   │   │   └── proximity.rs
│   │   ├── storage/            # Encrypted SQLite storage, outbox, volatile RAM
│   │   │   ├── db.rs
│   │   │   ├── key.rs
│   │   │   └── volatile.rs
│   │   ├── gateway/            # Iroh P2P QUIC gateway bridge & fairness
│   │   │   ├── bridge.rs
│   │   │   ├── contact.rs
│   │   │   ├── iroh_link.rs
│   │   │   └── fairness.rs
│   │   └── transport/          # Transport traits, SimNetwork, and BridgeTransport
│   └── tests/                  # Integration tests across all system flows
├── app/                        # Cross-platform Flutter client
│   ├── android/                # Native Android application & Foreground Service
│   │   └── app/src/main/kotlin/com/bitchat/bitchat2/
│   │       ├── MainActivity.kt
│   │       └── BitchatBleService.kt
│   ├── lib/                    # Dart framework & UI
│   │   ├── main.dart           # UI with Volatile Chat, Gateway, and Vouchers
│   │   ├── mesh_bridge.dart    # High-level bridge coordinator
│   │   └── core_ffi.dart       # Dart FFI bindings to libbitchat2_core.so
│   └── pubspec.yaml
└── docs/                       # Technical specifications & protocols
    ├── ARCHITECTURE.md         # Layered design & component responsibilities
    ├── TRUST_PROTOCOL.md       # Scoped capability vouchers specification
    ├── VOLATILE_CHAT.md        # In-memory ephemeral chat specification
    ├── DELIVERY_PROTOCOL.md    # Store-and-Forward & terminal ACK state machine
    ├── RESOURCE_POLICY.md      # Dynamic battery & relay hysteresis policy
    ├── PROXIMITY_LIMITATIONS.md# Coarse RSSI bucketing rationale
    ├── PRIVACY_POLICY.md       # Privacy-preserving guarantees
    ├── THREAT_MODEL.md         # Adversary capability matrix & defenses
    ├── CRYPTO_REVIEW.md        # Cryptographic implementation checklist
    ├── DECISIONS.md            # Architectural Decision Records (ADRs)
    ├── EVALUATION.md           # Benchmark evaluation reports
    └── DEVICE_TEST_PLAN.md     # Physical hardware test suite
```

---

## Build & Verification

### Prerequisites
- **Rust**: 1.80+ (`rustc`, `cargo`, `clippy`, `rustfmt`)
- **Android NDK**: r27+ (with `aarch64-linux-android` toolchain)
- **Flutter**: 3.24+

### 1. Verify Rust Core
```bash
# Run complete test suite (153 unit & integration tests)
cargo test --manifest-path core/Cargo.toml

# Run linter
cargo clippy --manifest-path core/Cargo.toml --all-targets -- -D warnings
```

### 2. Compile Native Android Library
```bash
cargo build --manifest-path core/Cargo.toml --target aarch64-linux-android --release
cp target/aarch64-linux-android/release/libbitchat2_core.so app/android/app/src/main/jniLibs/arm64-v8a/
```

### 3. Build & Install Flutter App
```bash
cd app
flutter analyze
flutter build apk --debug

# Install on connected device via ADB
adb install -r build/app/outputs/flutter-apk/app-debug.apk
```

---

## Physical Device Verification

The system has been verified on physical Android devices across five core milestones:

1. **BLE Mesh & Coarse Proximity**: Dual-role advertising and scanning with real-time RSSI bucket updates (`Immediate`, `Near`, `Far`).
2. **Volatile Ephemeral Chat**: Ephemeral key negotiation with active time-to-live countdown, memory zeroization on expiry, and Android `FLAG_SECURE` window shielding.
3. **Scoped Peer Trust & 1-Hop Capability Vouchers**: Ed25519 signed voucher generation, camera QR scanning, verification, and cascade revocation.
4. **Iroh Internet Gateway**: NAT-bypassing QUIC connections between isolated nodes using 5-minute single-use pairing invitations.
5. **Resource-Aware Hysteresis**: Dynamic relay throttling based on live OS battery and charging broadcast receivers.

---

## Documentation

- [System Architecture](docs/ARCHITECTURE.md)
- [Trust & Capability Vouchers](docs/TRUST_PROTOCOL.md)
- [Volatile Chat Protocol](docs/VOLATILE_CHAT.md)
- [Guaranteed Delivery Protocol](docs/DELIVERY_PROTOCOL.md)
- [Resource Hysteresis Policy](docs/RESOURCE_POLICY.md)
- [Coarse Proximity Rationale](docs/PROXIMITY_LIMITATIONS.md)
- [Threat Model](docs/THREAT_MODEL.md)
- [Cryptographic Audit Review](docs/CRYPTO_REVIEW.md)
- [Architectural Decisions (ADRs)](docs/DECISIONS.md)
- [Physical Device Test Plan](docs/DEVICE_TEST_PLAN.md)
