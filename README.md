# Bitchat 2.0

Decentralized, privacy-preserving mesh messenger combining Bluetooth Low Energy (BLE) hopping with internet-bridged Iroh P2P QUIC gateways.

## Current Status: Phase 2 — BLE Mesh Logic, Evaluation Harness, Flutter Shell (In Progress)

### Roadmap & Progress
- **Phase 1 (15%)**: Foundation, identity, wire format, crypto, simulated transport. [Completed]
- **Phase 2 (20%)**: BLE mesh logic, adaptive relaying, evaluation harness, Flutter shell. [In Progress]
  - [x] **Task 0**: Phase 1 carry-overs (Stateless Noise transport, sliding replay window, domain separation, compiler no-panic denies).
  - [ ] **Task 1**: Seen-message cache (`LruSeenCache`, `BloomSeenCache`).
  - [ ] **Task 2**: Link-layer fragmentation (`Fragmenter`, `Reassembler`, memory bounds).
  - [ ] **Task 3**: Mesh node & baseline relay engine (`NaiveFlood`, `MeshStats`).
  - [ ] **Task 4**: Adaptive relay policy (`AdaptiveFlood`, density-scaled delay, suppression).
  - [ ] **Task 5**: Evaluation harness (5 topologies, 20+ seeds, `docs/EVALUATION.md`).
  - [ ] **Task 6**: End-to-end encryption across relays (E2E Noise over mesh).
  - [ ] **Task 7**: Rust `BridgeTransport` and `flutter_rust_bridge` API.
  - [ ] **Task 8**: Flutter app & dual-role BLE GATT client/peripheral.
  - [ ] **Task 9**: Device test plan & documentation (`docs/DEVICE_TEST_PLAN.md`).

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
│       └── transport/          # Transport trait and SimNetwork simulator
│           ├── mod.rs
│           └── sim.rs
├── docs/                       # Architecture, threat model, crypto review, ADRs
│   ├── ARCHITECTURE.md         # System layers and component responsibilities
│   ├── THREAT_MODEL.md         # Adversary capability matrix and defenses
│   ├── CRYPTO_REVIEW.md        # Cryptographic audit checklist & registered files
│   └── DECISIONS.md            # Architectural Decision Records (ADRs)
├── .agents/skills/             # Engineering standards and review rules
├── .github/workflows/ci.yml    # Continuous integration workflow
├── PHASE_1.md                  # Phase 1 specification
└── PHASE_2.md                  # Phase 2 specification
```

## Developer Setup & Verification

### Prerequisites
- Rust stable toolchain 1.98+ (`rustc`, `cargo`, `clippy`, `rustfmt`)
- Flutter SDK 3.47+ & Android SDK / NDK

### Running Verification Suite
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all
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
