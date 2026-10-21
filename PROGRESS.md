# Phase 1 Progress Log

## Status Overview
- Total Phase 1 Share: 15%
- Current Status: Phase 1 Fully Completed (Tasks 1–6)

## Task Checklist
- [x] Task 1 — Workspace and tooling
- [x] Task 2 — Error type and identity
- [x] Task 3 — Envelope wire format
- [x] Task 4 — Noise session with identity binding (CRYPTO-REVIEW)
- [x] Task 5 — Transport trait and simulator
- [x] Task 6 — Docs and wrap-up

## Test Counts & Verification Run
- **Total Tests Passing**: 25 tests (0 failures, 0 ignored)
  - `identity`: 6 tests (key generation, signing/verification, tampered signature failure, short id derivation, memory keystore roundtrip, file keystore roundtrip, secret debug redaction)
  - `envelope`: 10 tests (proptest arbitrary roundtrip, reject wrong version, reject truncated header, reject payload length mismatch, reject hop count > ttl, reject ttl > 7, reject buffer > 512, relay hop increment, unknown msg_type roundtrip, reversible padding helper)
  - `crypto`: 5 tests (Noise XX full handshake, bidirectional AEAD encryption, forged identity signature rejection, tampered ciphertext rejection, replayed ciphertext nonce rejection, session debug redaction)
  - `transport::sim`: 4 tests (two-node direct exchange, three-node line topology isolation, link removal delivery cessation, seeded loss determinism)
- **Clippy**: `cargo clippy --all-targets -- -D warnings` clean with zero warnings.
- **Formatter**: `cargo fmt --check` clean.
- **Panic Audit**: Zero `unwrap()`, `expect()`, or `panic!` calls in non-test production code.

## Not Verified (Phase 1 Boundaries)
- **Physical Bluetooth Low Energy (BLE) Hardware**: Real-world 2.4 GHz radio propagation, BLE advertising packet size limits (31-byte legacy / extended advertising), and OS-level Bluetooth stack quirks are simulated purely in software (`SimNetwork`). No hardware verification is claimed.
- **Hardware Security Module / OS Keystores**: Android Keystore and iOS Keychain hardware-backed key persistence are not tested; `MemoryKeyStore` and development-only `FileKeyStore` are used for Phase 1.
- **Multi-hop Mesh Flooding & Relay Routing**: Automated mesh forwarding, deduplication caches, and rate-limiting are out of scope for Phase 1 and will be introduced in Phase 2.
- **Iroh Internet Bridging & Gateway Dialing**: QUIC P2P tunneling over Iroh is out of scope for Phase 1 and will be integrated in Phase 3.
