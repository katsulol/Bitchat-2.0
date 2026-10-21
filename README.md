# Bitchat 2.0

Decentralized, privacy-preserving mesh messenger combining Bluetooth Low Energy (BLE) hopping with internet-bridged Iroh P2P QUIC gateways.

## Current Status: Phase 1 — Foundation & Core Tooling (Completed — 15% Total Share)

All Phase 1 tasks are implemented, verified with comprehensive tests, and passing CI criteria:
- [x] **Task 1: Workspace and tooling** (Cargo workspace, pinned dependencies, CI pipeline, quality enforcement skills)
- [x] **Task 2: Error type and identity** (Ed25519 `Identity`, `PeerId`, `ShortId`, `zeroize`, `KeyStore`, secret redaction)
- [x] **Task 3: Envelope wire format** (31-byte compact header, hand-written codec, `proptest`, reversible bucket padding)
- [x] **Task 4: Noise session with identity binding** (Noise_XX with signed static keys, replay/tamper rejection)
- [x] **Task 5: Transport trait and simulator** (Deterministic simulated mesh network with loss and latency)
- [x] **Task 6: Docs and wrap-up** (Architecture, threat model, ADRs, crypto review registry)

## Repository Structure

```
.
├── Cargo.toml                  # Cargo workspace definition & build profiles
├── core/
│   ├── Cargo.toml              # bitchat2_core library definition
│   └── src/
│       ├── lib.rs              # Library entry point
│       ├── error.rs            # Unified Error type (thiserror)
│       ├── identity.rs         # Ed25519 identity, PeerId, ShortId, KeyStore
│       ├── envelope.rs         # 31-byte binary wire codec & padding
│       ├── crypto.rs           # Noise_XX session with identity binding
│       └── transport/          # Transport trait and SimNetwork simulator
│           ├── mod.rs
│           └── sim.rs
├── docs/                       # Architecture, threat model, crypto review, ADRs
│   ├── ARCHITECTURE.md         # System layers and component responsibilities
│   ├── THREAT_MODEL.md         # Adversary capability matrix and defenses
│   ├── CRYPTO_REVIEW.md        # Cryptographic audit checklist & registered files
│   └── DECISIONS.md            # Architectural Decision Records (ADRs)
├── .agents/skills/             # Engineering standards and review rules
│   ├── rust-build/SKILL.md     # Build & lint enforcement rules
│   └── crypto-review/SKILL.md  # Cryptographic review protocols
├── .github/workflows/ci.yml    # Continuous integration workflow
└── PHASE_1.md                  # Phase 1 specification
```

## Quick Start & Verification

### Prerequisites
- Rust stable toolchain 1.98+ (`rustc`, `cargo`, `clippy`, `rustfmt`)

### Running Verification Suite
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all
```
