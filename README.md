# Bitchat 2.0

Decentralized, privacy-preserving mesh messenger combining Bluetooth Low Energy (BLE) hopping with internet-bridged Iroh P2P QUIC gateways.

## Current Status: Phase 2 — BLE Mesh Logic, Evaluation Harness, Flutter Shell (Completed)

### Roadmap & Progress
- **Phase 1 (15%)**: Foundation, identity, wire format, crypto, simulated transport. [Completed]
- **Phase 2 (20%)**: BLE mesh logic, adaptive relaying, evaluation harness, Flutter shell. [Completed]
  - [x] **Task 0**: Phase 1 carry-overs (Stateless Noise transport, sliding replay window, domain separation, compiler no-panic denies).
  - [x] **Task 1**: Seen-message cache (`LruSeenCache`, `BloomSeenCache`, ADR-0005).
  - [x] **Task 2**: Link-layer fragmentation (`Fragmenter`, `Reassembler`, memory bounds).
  - [x] **Task 3**: Mesh node & baseline relay engine (`NaiveFlood`, `MeshStats`).
  - [x] **Task 4**: Adaptive relay policy (`AdaptiveFlood`, density-scaled delay, suppression).
  - [x] **Task 5**: Evaluation harness (5 topologies, 20+ seeds, `docs/EVALUATION.md`, `evaluation_results.csv`).
  - [x] **Task 6**: End-to-end encryption across relays (E2E Noise over mesh, `PeerSessionManager`).
  - [x] **Task 7**: Rust `BridgeTransport` and `flutter_rust_bridge` API (Part B - UNVERIFIED awaiting device test).
  - [x] **Task 8**: Flutter app & dual-role BLE GATT client/peripheral (Part B - UNVERIFIED awaiting device test).
  - [x] **Task 9**: Device test plan & documentation (`docs/DEVICE_TEST_PLAN.md`).

## Repository Structure

```
.
├── Cargo.toml                  # Cargo workspace definition
├── core/
│   ├── Cargo.toml              # bitchat2_core library definition
│   └── src/
│       ├── lib.rs              # Library entry point & compiler lint denies
│       ├── error.rs            # Unified Error type (thiserror)
│       ├── identity.rs         # Ed25519 identity, PeerId, ShortId, KeyStore
│       ├── envelope.rs         # 31-byte binary wire codec & padding
│       ├── crypto.rs           # Stateless Noise_XX with sliding replay window
│       ├── api.rs              # flutter_rust_bridge cross-language API
│       ├── mesh/               # Fragmentation, deduplication caches, relay policy
│       │   ├── mod.rs
│       │   ├── fragment.rs
│       │   ├── seen.rs
│       │   ├── relay.rs
│       │   └── peer.rs
│       └── transport/          # Transport trait, SimNetwork, and BridgeTransport
│           ├── mod.rs
│           ├── sim.rs
│           └── bridge.rs
├── app/                        # Cross-platform Flutter client
│   ├── android/                # Ready-to-use Android Studio project
│   │   ├── app/src/main/kotlin/
│   │   │   └── com/bitchat/bitchat2/
│   │   │       ├── MainActivity.kt
│   │   │       └── BitchatBleService.kt # Dual-role BLE GATT + Foreground Service
│   │   └── build.gradle.kts
│   ├── lib/
│   │   ├── main.dart           # Clean mesh UI with stats and peer list
│   │   └── mesh_bridge.dart    # MethodChannel / FFI mesh binding
│   └── pubspec.yaml
├── docs/                       # Architecture, threat model, crypto review, ADRs
│   ├── ARCHITECTURE.md         # System layers and component responsibilities
│   ├── THREAT_MODEL.md         # Adversary capability matrix and defenses
│   ├── CRYPTO_REVIEW.md        # Cryptographic audit checklist & registered files
│   ├── DECISIONS.md            # Architectural Decision Records (ADRs)
│   ├── EVALUATION.md           # 200-run simulation benchmark report
│   └── DEVICE_TEST_PLAN.md     # 2-3 phone physical hardware test protocol
├── evaluation_results.csv      # Raw evaluation data
├── .agents/skills/             # Engineering standards and review rules
├── .github/workflows/ci.yml    # Continuous integration workflow
├── PHASE_1.md                  # Phase 1 specification
└── PHASE_2.md                  # Phase 2 specification
```

## Developer Setup & Verification

### Prerequisites
- Rust stable toolchain 1.98+ (`rustc`, `cargo`, `clippy`, `rustfmt`)
- Flutter SDK 3.47+ & Android SDK / NDK

### Running Rust Verification Suite
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all
```

### Running Flutter Verification Suite
```bash
cd app
flutter analyze
flutter test
```

### Opening the Project in Android Studio
The `app/android` directory is an independent Android Studio project with full Gradle wrapper and native Kotlin source files:
1. Open **Android Studio**.
2. Select **Open** and choose either the root `app/` folder (for Flutter IDE mode) or `app/android/` (for native Android / NDK development).
3. Connect a physical Android phone with USB Debugging enabled.
4. Run or debug target `app`.

### Testing on Physical Hardware
Refer to [docs/DEVICE_TEST_PLAN.md](docs/DEVICE_TEST_PLAN.md) for step-by-step physical verification procedures across 2 to 3 Android devices (discovery, line relay, foreground service persistence, and battery drain).

### Flutter Rust Bridge (FRB) Code Generation
- **Pinned Version**: `flutter_rust_bridge_codegen = 2.8.0`
- **Install Tool**:
  ```bash
  cargo install flutter_rust_bridge_codegen --version 2.8.0
  ```
- **Generate Bindings**:
  ```bash
  flutter_rust_bridge_codegen generate
  ```

### Windows Developer Note (Smart App Control Workaround)
On Windows 11 with Smart App Control (Device Guard) enabled, debug binaries built in user directories may trigger code integrity blocks. If running on such machines, create a local, gitignored `.cargo/config.toml`:

```toml
[profile.dev]
incremental = false
debug = 0
opt-level = 0

[profile.test]
incremental = false
debug = 0
opt-level = 0
```
This flag is excluded from git to ensure CI runners compile with standard release and benchmark profiles.
