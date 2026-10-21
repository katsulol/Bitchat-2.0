# Bitchat 2.0

Decentralized, privacy-preserving mesh messenger combining Bluetooth Low Energy (BLE) hopping with internet-bridged Iroh P2P QUIC gateways.

## Current Status: Phase 1 — Foundation & Core Tooling

Phase 1 establishes the Rust core library (`bitchat2_core`):
- [x] **Task 1: Workspace and tooling** (Cargo workspace, pinned dependencies, CI pipeline, quality enforcement skills)
- [x] **Task 2: Error type and identity** (Ed25519 `Identity`, `PeerId`, `ShortId`, `zeroize`, `KeyStore`, secret redaction)
- [ ] **Task 3: Envelope wire format** (31-byte compact header, hand-written codec, padding)
- [ ] **Task 4: Noise session with identity binding** (Noise_XX with signed static keys)
- [ ] **Task 5: Transport trait and simulator** (Deterministic simulated mesh network)
- [ ] **Task 6: Docs and wrap-up**

## Repository Structure

```
.
├── Cargo.toml                  # Cargo workspace definition
├── core/
│   ├── Cargo.toml              # bitchat2_core library definition
│   └── src/
│       ├── lib.rs              # Library entry point
│       ├── error.rs            # Unified Error type (thiserror)
│       └── identity.rs         # Ed25519 identity, PeerId, ShortId, KeyStore
├── docs/                       # Architecture, threat model, crypto review, ADRs
├── .agents/skills/             # Engineering standards and review rules
├── .github/workflows/ci.yml    # Continuous integration workflow
└── PHASE_1.md                  # Phase 1 specification
```

## Quick Start & Verification

### Prerequisites
- Rust stable toolchain 1.98+ (`rustc`, `cargo`, `clippy`, `rustfmt`)

### Running Verification
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all
```
