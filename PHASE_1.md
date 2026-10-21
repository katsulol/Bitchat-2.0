# PHASE 1 — Foundation, Identity, Wire Format, Crypto, Simulated Transport

**Share of total project: 15%** (cumulative after this phase: 15%)

Read this whole file before writing any code. Work through the tasks in order. Do not start Phase 2 work.

---

## 0. Project context (read once)

**Bitchat 2.0** is a decentralized messenger:

- Nearby devices form a **Bluetooth Low Energy (BLE) mesh** and relay messages hop by hop with no internet.
- Any mesh device that has WiFi/cellular can act as a **gateway** and bridge messages over **Iroh (QUIC, dial-by-public-key)** to a gateway near the recipient, which re-broadcasts into that local mesh.
- A **Smart Routing & Delivery Manager** chooses between BLE mesh, direct Iroh P2P, or queued delivery based on connectivity, device state, and trust.
- Two modes: **Normal** (speed/reachability) and **Privacy** (low metadata exposure, minimal stored trace).
- Identity is a cryptographic key. No phone number, no account, no central server.

Stack: **Rust core** (Tokio, Iroh), **Flutter/Dart UI** via `flutter_rust_bridge`, BLE through a Flutter plugin on real phones.

### Roadmap (for context only; only Phase 1 is in scope now)

| Phase | Focus | Share | Cumulative |
|---|---|---|---|
| **1** | **Foundation, identity, wire format, crypto, simulated transport** | **15%** | **15%** |
| 2 | BLE mesh logic: flooding, dedup, adaptive delay, fragmentation, Flutter shell + BLE plugin | 20% | 35% |
| 3 | Iroh integration: gateway node, bridging, relay fallback, mesh-to-Iroh handoff | 17% | 52% |
| 4 | Smart Routing & Delivery Manager, queued delivery, Normal/Privacy modes | 18% | 70% |
| 5 | Web-of-Trust, device-class-aware relay, fuzzy proximity discovery, volatile chat | 15% | 85% |
| 6 | Cover traffic, Merkle lazy sync, evaluation harness, docs, hardening | 15% | 100% |

---

## 1. Ground rules for the agent

1. **Honesty over completeness.** If something cannot be verified in this environment (anything needing real BLE hardware), say so in `PROGRESS.md`. Never claim hardware-verified behavior from simulator-only tests.
2. **Crypto changes need manual review.** Every file touching key generation, Noise handshakes, signing, or encryption must carry a `// CRYPTO-REVIEW:` header comment and be listed in `docs/CRYPTO_REVIEW.md`. Do not invent custom cryptography. Use the audited crates named below.
3. **No guessing APIs.** If you are unsure of a crate's current API, read its docs (`cargo doc --open`, docs.rs) before writing code. Pin exact versions in `Cargo.toml`.
4. **Small, reviewable commits.** One logical task per commit, message format `phase1: <task>`.
5. **Tests are part of the task.** A task is not done until its tests exist and pass.
6. **No overclaiming in docs.** Do not write "anonymous", "untraceable", or "zero-trace" anywhere. Use "metadata-minimizing" or "best-effort".
7. **Scope discipline.** No flooding logic, no Iroh, no Flutter, no BLE in this phase.

---

## 2. Deliverables

```
bitchat2.0/
├── README.md
├── PROGRESS.md                  # running log; update at end of every task
├── docs/
│   ├── ARCHITECTURE.md          # layers + module map (skeleton OK)
│   ├── THREAT_MODEL.md          # table: adversary capability / defended? / how
│   ├── CRYPTO_REVIEW.md         # list of crypto-touching files + what to review
│   └── DECISIONS.md             # ADR-style log of choices made
├── .agents/skills/
│   ├── rust-build/SKILL.md
│   └── crypto-review/SKILL.md
├── .github/workflows/ci.yml
├── Cargo.toml                   # workspace
└── core/
    ├── Cargo.toml
    └── src/
        ├── lib.rs
        ├── error.rs
        ├── identity.rs          # Ed25519 identity + KeyStore trait
        ├── envelope.rs          # wire format encode/decode
        ├── crypto.rs            # Noise session wrapper + identity binding
        ├── transport/
        │   ├── mod.rs           # Transport trait
        │   └── sim.rs           # in-memory simulated network
        └── tests/ or tests/     # integration tests
```

---

## 3. Tasks (do in this order)

### Task 1 — Workspace and tooling
- `cargo` workspace at repo root with one member crate `core` (library name `bitchat2_core`).
- Dependencies (pin versions): `tokio` (features `rt-multi-thread`, `macros`, `sync`, `time`), `thiserror`, `tracing`, `bytes`, `blake3`, `ed25519-dalek`, `snow`, `rand`, `zeroize`. Dev: `proptest`, `tokio-test` if needed.
- `rustfmt.toml` defaults, `clippy` clean with `-D warnings`.
- CI workflow: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all`.
- Create `.agents/skills/rust-build/SKILL.md`: the exact fmt/clippy/test commands and the rule "all three must pass before a task is marked done".
- Create `.agents/skills/crypto-review/SKILL.md`: "flag any change touching key generation, Noise, signing, or encryption for manual review; never auto-approve; require a `CRYPTO-REVIEW` header and an entry in `docs/CRYPTO_REVIEW.md`."

**Done when:** `cargo build`, `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` all succeed on an empty-but-compiling crate and CI config is valid.

### Task 2 — Error type and identity
- `error.rs`: one `Error` enum via `thiserror` covering Envelope, Crypto, Transport, KeyStore variants. No `unwrap()` or `expect()` in non-test code.
- `identity.rs`:
  - `Identity` wraps an Ed25519 signing key. `PeerId` = the 32-byte public key. `ShortId` = first 8 bytes of `blake3(public_key)`.
  - `KeyStore` trait: `load() -> Result<Option<Identity>>`, `save(&Identity)`. Provide `FileKeyStore` (dev only, file with restrictive permissions) and `MemoryKeyStore` (tests). Document that a platform keystore implementation (Android Keystore / iOS Keychain) replaces `FileKeyStore` in a later phase.
  - Secret material uses `zeroize` on drop. `Debug` impls must never print secret bytes.
  - `Identity::generate()`, `sign()`, `verify(peer_id, msg, sig)`.
- Tests: generate → sign → verify; tampered message fails; save/load round trip; `Debug` output contains no key bytes.

### Task 3 — Envelope wire format
Fixed 31-byte header, big-endian, hand-written encode/decode (no serde; the format must stay BLE-compact and byte-exact):

| Offset | Size | Field | Notes |
|---|---|---|---|
| 0 | 1 | version | currently `1` |
| 1 | 1 | msg_type | enum below |
| 2 | 1 | flags | bit0 privacy_mode, bit1 priority, bit2 padded, bit3 fragment |
| 3 | 1 | hop_count | **relay-mutable** |
| 4 | 1 | ttl_max | max 7 |
| 5 | 8 | msg_id | set once by the origin, never recomputed by relays |
| 13 | 8 | sender_short_id | |
| 21 | 8 | recipient_short_id | all zeros = broadcast |
| 29 | 2 | payload_len | u16 |
| 31 | N | payload | |

- `MsgType`: `Announce, Handshake, Chat, Ack, Vouch, TrustUpdate, Cover, Reserved(u8)`. Unknown types must decode as `Reserved` and not error (forward compatibility).
- `msg_id` = first 8 bytes of `blake3(sender_pubkey || origin_counter_u64 || blake3(payload))`.
- Constants: `MAX_HOPS = 7`, `MAX_ENVELOPE_LEN = 512` (fragmentation arrives in Phase 2; reject larger for now).
- Padding helper: `pad_to_bucket(payload, bucket)` using buckets `[64, 128, 256, 512]`, so Cover traffic (Phase 6) can later be indistinguishable in size. Padding is length-prefixed so it is reversible.
- **Design rule to document in `DECISIONS.md`:** any signature or MAC over an envelope must exclude `hop_count`, because relays mutate it. State this explicitly.
- Decode must reject: wrong version, payload_len not matching the buffer, `hop_count > ttl_max`, `ttl_max > MAX_HOPS`, buffer larger than `MAX_ENVELOPE_LEN`, truncated header.
- Tests: `proptest` encode→decode round trip for arbitrary valid envelopes; each rejection case above; relay increments hop_count without changing any other field; unknown msg_type survives a round trip.

### Task 4 — Noise session with identity binding  (CRYPTO-REVIEW)
- Use `snow` with pattern `Noise_XX_25519_ChaChaPoly_BLAKE2s`.
- **Known weakness in the reference app:** its handshake does not strongly bind the Noise static key to a long-term identity. Fix it here: each side signs its Noise static public key with its Ed25519 identity key and sends `(identity_pubkey, signature)` as the handshake payload. The receiver verifies the signature before accepting the session and exposes the verified `PeerId`.
- API sketch: `Session::initiator(&Identity)`, `Session::responder(&Identity)`, `write_handshake() / read_handshake()`, then `encrypt(&[u8]) -> Vec<u8>` / `decrypt(&[u8]) -> Vec<u8>`, and `peer_id() -> Option<PeerId>` after completion.
- Tests: full XX handshake between two in-memory identities; encrypted message round trip both directions; a handshake with a wrong or forged identity signature is rejected; tampered ciphertext fails to decrypt; replayed ciphertext is rejected by the nonce counter; `peer_id()` equals the other side's real identity.
- Add every file in this task to `docs/CRYPTO_REVIEW.md` with: what it does, which crates it relies on, and what a human reviewer should check.

### Task 5 — Transport trait and simulator
- `transport/mod.rs`: async `Transport` trait roughly:
  - `async fn send(&self, to: ShortId, frame: Vec<u8>) -> Result<()>`
  - `async fn broadcast(&self, frame: Vec<u8>) -> Result<()>` (delivers to all peers currently in range)
  - `async fn recv(&mut self) -> Option<(ShortId, Vec<u8>)>`
  - `fn neighbors(&self) -> Vec<ShortId>`
- `transport/sim.rs`: `SimNetwork` holding N nodes with a configurable **adjacency graph** (who is in range of whom), per-link **latency**, and **loss probability** (seeded RNG so tests are deterministic). Nodes can be added, removed, or have links changed at runtime to simulate movement.
- This is the stand-in for BLE until Phase 2 wires real radios behind the same trait.
- Tests: two nodes exchange frames; three nodes in a line (A–B–C): A's broadcast reaches B only, not C; link removal stops delivery; seeded loss gives identical results across two runs.
- No flooding or relay logic here. That is Phase 2.

### Task 6 — Docs and wrap-up
- `ARCHITECTURE.md`: layer diagram in text/mermaid (Transport → Envelope → Session → future Routing/Trust/Mode layers), and which parts are **inherited concepts** (BLE mesh, Noise, no-account identity, 7-hop cap) versus **this project's additions** (identity-bound handshake, adaptive routing, gateway bridging, trust, modes).
- `THREAT_MODEL.md`: the adversary table (reads content; passive BLE observer; compromised gateway; relay observer; timing correlation; forged vouch; Sybil flood) with an honest "defended / partial / not yet" column. Mark everything not implemented as "planned".
- `README.md`: what the project is, how to build/test, current status.
- `PROGRESS.md`: per-task status, test counts, and an explicit **"Not verified"** section.

---

## 4. Acceptance criteria (all must be true)

- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all` pass locally and in CI config.
- [ ] No `unwrap()`/`expect()`/`panic!` in non-test code (grep to confirm).
- [ ] Envelope round-trip property test passes; every rejection case has its own test.
- [ ] Noise handshake rejects a forged identity signature (test exists and passes).
- [ ] `Debug` on `Identity` and `Session` never leaks key material (test exists).
- [ ] `SimNetwork` is deterministic under a fixed seed (test exists).
- [ ] `docs/CRYPTO_REVIEW.md` lists every crypto-touching file.
- [ ] Both `.agents/skills/*/SKILL.md` files exist.
- [ ] `PROGRESS.md` contains a "Not verified" section.

## 5. Explicitly out of scope for Phase 1

BLE of any kind, flooding/relay/dedup, Iroh, Flutter, routing decisions, trust/vouching logic, cover traffic, persistence of messages, UI.

## 6. When finished, report back with

1. Test count and the exact commands run, with pass/fail output.
2. A list of files that need human crypto review.
3. Any decision you made that deviates from this spec, and why (also recorded in `docs/DECISIONS.md`).
4. Anything you could not verify and why.
