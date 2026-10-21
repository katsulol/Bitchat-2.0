# PHASE 2 — BLE Mesh Logic, Evaluation Harness, Flutter Shell

**Share of total project: 20%** (cumulative after this phase: 35%)

Work in two halves and commit between them:

- **Part A (Tasks 0–6): pure Rust, fully testable in the simulator.** Roughly 13% of the project.
- **Part B (Tasks 7–9): Flutter shell and real BLE bridge. Cannot be verified without physical phones.** Roughly 7%.

Do not start Phase 3 (Iroh) work.

All ground rules from `PHASE_1.md` still apply. The important ones: no custom crypto; `CRYPTO-REVIEW` header plus an entry in `docs/CRYPTO_REVIEW.md` for any crypto-touching file; never guess an API; one commit per task (`phase2: <task>`); fmt, clippy `-D warnings`, and tests pass after every task; never claim hardware-verified results; no words like "anonymous", "untraceable", or "zero-trace" in code or docs.

---

## Task 0 — Phase 1 carry-over fixes (do these first)

1. **Noise transport must survive packet loss and reordering. (CRYPTO-REVIEW)**
   Phase 1 uses `snow`'s stateful transport mode, which relies on an implicit incrementing nonce. On a lossy, reordering mesh, one dropped packet desynchronizes the session permanently.
   - Switch to snow's stateless transport mode (read its docs for the current API) with an **explicit 64-bit nonce carried in each encrypted payload**.
   - Add a **sliding replay window** (size 64 or 128) so replays and very old packets are rejected while modest reordering is accepted.
   - Tests: out-of-order delivery within the window succeeds; replay of an already-accepted nonce is rejected; a nonce older than the window is rejected; a dropped packet does not break later packets.
   - Update `docs/CRYPTO_REVIEW.md` and add an ADR to `docs/DECISIONS.md`.
2. **Domain separation in the handshake signature. (CRYPTO-REVIEW)**
   Confirm the Ed25519 signature over the Noise static public key includes a fixed context prefix (for example `b"bitchat2/noise-static/v1" || static_pubkey`). If not, add it, with a test that a signature made without the prefix is rejected.
3. **Enforce the no-panic rule in the compiler, not by grep.**
   Add `#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]` to `core/src/lib.rs`. Fix anything it flags.
4. **Move the Windows workaround out of committed config.**
   Phase 1 put `debug = 0 / opt-level = 0 / incremental = false` into the committed `Cargo.toml`. Remove it from there. Put it in a gitignored `.cargo/config.toml` and document in `README.md` how a Windows developer enables it. Committed profiles must stay sane for CI and for benchmarks later.
5. **Repo hygiene.** Confirm `.gitignore` covers `target/`, any key files `FileKeyStore` can create, and Flutter build output. Confirm no secret key material exists in git history; if it does, report it, do not hide it.

**Done when:** all existing 25 tests still pass, the new Noise tests pass, and the lint denies are active.

---

## PART A — Rust mesh logic (simulator-verified)

### Task 1 — Seen-message cache
Create `core/src/mesh/seen.rs`.
- `trait SeenCache { fn check_and_insert(&mut self, id: MsgId, now: Instant) -> bool }` (returns `true` if already seen).
- `LruSeenCache`: exact, bounded capacity, entries expire after a configurable TTL. This is the default.
- `BloomSeenCache`: two rotating Bloom filters (current and previous generation) so old entries age out without a per-entry timestamp. Configurable target false-positive rate (default 1%).
- Tests: no false negatives for either; Bloom's measured false-positive rate stays near its target at capacity (assert a loose bound, print the measured value); expiry works; capacity bound holds.
- Record in `DECISIONS.md` why exact LRU is the default and Bloom is kept as the compact alternative that Phase 6 will compare.

### Task 2 — Link-layer fragmentation
Create `core/src/mesh/fragment.rs`. BLE payloads are far smaller than a 512-byte envelope, so fragmentation sits **below** the envelope.
- Fragment header: `frame_id: u16`, `index: u8`, `total: u8`. `Fragmenter::split(frame, mtu) -> Vec<Vec<u8>>` and a `Reassembler`.
- Reassembler must bound memory: max concurrent partial frames, per-frame timeout, and drop-oldest on overflow. A hostile peer must not be able to exhaust memory.
- Tests: split/reassemble round trip for many sizes and MTUs (proptest); out-of-order fragments; duplicate fragments; missing fragment times out and is freed; overflow eviction; malformed headers rejected.

### Task 3 — Mesh node and relay engine (baseline policy)
Create `core/src/mesh/{mod.rs, relay.rs, policy.rs, stats.rs}`.
- `MeshNode<T: Transport>` owns the identity, transport, seen cache, reassembler, and a `Box<dyn RelayPolicy>`.
- Origin API: `send_broadcast(payload)` and `send_to(recipient, payload)` (builds the envelope, sets `msg_id` once, `ttl_max = MAX_HOPS`).
- Receive path: reassemble → decode → seen check → deliver locally if broadcast or addressed to us → consult policy → schedule relay → on timer, increment `hop_count` and send. Never relay an envelope with `hop_count >= ttl_max`. Never relay our own origin messages back out as if foreign.
- `trait RelayPolicy`: `should_relay(&RelayContext) -> bool`, `delay(&RelayContext) -> Duration`, `suppress_after(&RelayContext) -> u8`. `RelayContext` carries `neighbor_count`, `hop_count`, `priority`, and `duplicates_heard`.
- Implement `NaiveFlood`: relay every first-seen envelope immediately. This is the **baseline** that mirrors the reference app.
- `MeshStats`: transmissions, duplicates dropped, relays suppressed, deliveries, per-message latency.
- Use `tokio::time::pause()` / start-paused tests so timing is deterministic. Do not use real sleeps in tests.
- Tests: A–B–C line, A's broadcast reaches C via B in exactly one relay; ring topology terminates (no infinite loops); TTL stops propagation at 7 hops on a long line; duplicates are dropped; stats counters are correct.

### Task 4 — Adaptive relay policy
Create `AdaptiveFlood` in `policy.rs`.
- **Adaptive delay:** relay delay is random in `[0, D]` where `D` grows with `neighbor_count`, so dense areas spread transmissions out in time instead of colliding.
- **Counter-based suppression:** while a relay is pending, count how many times the same `msg_id` is heard again. If the count reaches threshold `k` (configurable, default 2), cancel the relay, because the neighborhood is already covered.
- **Priority bypass:** envelopes with the priority flag use minimal delay and are never suppressed.
- **Unicast shortcut:** for `send_to`, if the recipient is a direct neighbor, send directly and skip the flood.
- Leave a clearly marked extension point so Phase 5's device-class-aware policy can wrap or replace this one. Default behavior: every device relays.
- In `DECISIONS.md`, state that this is **inspired by** adaptive-delay and counter-based broadcast-storm suppression ideas from the literature (including the cited Wang, Li and Li approach). It is **not** a reproduction of any specific paper's algorithm, and must not be described as one.
- Tests: suppression cancels relays in a dense cluster; priority messages are never suppressed; unicast shortcut skips the flood; delays scale with neighbor count.

### Task 5 — Evaluation harness (the data your report needs)
Create `core/tests/` or `core/benches/` plus `docs/EVALUATION.md`.
- Topologies: line (10 nodes), grid (7x7), dense cluster (30 nodes all in range), random geometric graph (50 nodes), and a partitioned graph with a single bridge node.
- For each topology, run `NaiveFlood` and `AdaptiveFlood` across at least 20 fixed seeds with 5% link loss. Record: total transmissions, delivery ratio, mean and 95th-percentile latency, duplicates received.
- Emit a CSV and a summary table in `EVALUATION.md`.
- **Report the real numbers, whatever they are. Do not tune parameters to reach a target, and do not claim improvements the data does not show.** If adaptive flooding lowers delivery ratio in some topology, say so and say by how much.
- Include a "Limits of this evaluation" paragraph: simulator only, idealized radio model, no collision modeling beyond the loss rate.

### Task 6 — End-to-end encryption across relays
- Carry Noise handshake messages as `Handshake` envelopes through the mesh. Relays forward them as opaque bytes and never inspect payloads.
- Build a `Peer`-level API that establishes a `Session` with a non-adjacent peer over the mesh, then sends `Chat` envelopes encrypted end to end.
- Tests: in an A–B–C line, A and C complete the handshake **through B** and exchange messages; B's relay path never sees plaintext (assert on what B's code can access); a tampered envelope payload in transit fails decryption at C; C verifies A's `PeerId` via the identity-bound signature.
- This task touches crypto flow: add `CRYPTO-REVIEW` headers and update `docs/CRYPTO_REVIEW.md`.

**Part A done when:** all checks pass, `EVALUATION.md` contains real measured results, and `PROGRESS.md` marks Part A complete. **Commit and stop for human review before starting Part B.**

---

## PART B — Flutter shell and BLE bridge (UNVERIFIED until tested on real phones)

Every item in Part B must be labeled **UNVERIFIED** in `PROGRESS.md` until the human reports results from `docs/DEVICE_TEST_PLAN.md`.

### Task 7 — Rust `BridgeTransport` and `flutter_rust_bridge` API
- `core/src/transport/bridge.rs`: a `Transport` implementation that does not touch radios itself. Dart calls `on_peer_connected(short_id)`, `on_peer_disconnected(short_id)`, `on_frame_received(short_id, bytes)`; Rust exposes a stream of `OutboundFrame { to: Option<ShortId>, bytes }` for Dart to write to BLE.
- Tests entirely in Rust: feed frames in, observe outbound frames, confirm `MeshNode` works on top of it exactly as on `SimNetwork`.
- Expose a small `flutter_rust_bridge` API: `init(storage_path)`, `my_peer_id()`, `send_broadcast(text)`, `send_to(peer, text)`, event stream of received messages and peer changes, stats snapshot. Pin the codegen version and document the generate command in the README.

### Task 8 — Flutter app and BLE layer
- `flutter create` an app in `app/`. Android first; iOS only if it builds without extra work, and say so either way.
- **BLE plugin choice matters.** A mesh node must act as both a GATT **client** (scan, connect) and a GATT **server/peripheral** (advertise, accept connections). Some popular plugins, including `flutter_blue_plus`, are central-role only. Evaluate current options (for example `bluetooth_low_energy`, or a small native Kotlin module exposed over a platform channel) by **reading their docs for peripheral support**, pick one, and record the choice and the alternatives in `DECISIONS.md`.
- BLE design: one custom service UUID; one characteristic for write-without-response plus notify; advertise the service UUID; scan for it; cap simultaneous connections (default 5); request a larger MTU and pass the negotiated MTU to the `Fragmenter`; reconnect with backoff; foreground service on Android so the radio survives screen-off.
- Minimal UI: peer list, public broadcast thread, send box, and a debug screen showing `MeshStats`.
- No Iroh, no routing modes, no trust UI.

### Task 9 — Docs and device test plan
- `docs/DEVICE_TEST_PLAN.md`, written for a human with 2–3 phones:
  1. Two phones, same room: discovery, send both ways, record latency.
  2. Three phones in a line where A and C are out of each other's range: A→C must arrive via B. How to arrange this (distance, obstacles, or turning phones' TX power down).
  3. Kill B mid-conversation, confirm reconnection behavior.
  4. Screen-off for 5 minutes: does the node survive?
  5. Battery drain over 30 minutes with the screen off, in percent.
  - A table the human fills in: device models, Android versions, results, notes.
- Update `ARCHITECTURE.md`, `THREAT_MODEL.md` (BLE advertising exposes presence; say so honestly), `README.md`, `PROGRESS.md`.

---

## Acceptance criteria

- [ ] Task 0 items all done; 25 prior tests still pass; Noise survives loss and reordering within the replay window.
- [ ] `clippy::unwrap_used`, `expect_used`, `panic` are denied for non-test code.
- [ ] `core/tests` includes passing tests for: seen caches, fragmentation (including DoS bounds), baseline flooding, adaptive policy, mesh-relayed E2E handshake.
- [ ] `docs/EVALUATION.md` has measured results across all five topologies, 20+ seeds, both policies, with an honest limits section.
- [ ] No test uses real sleeps; timing tests use paused time.
- [ ] `CRYPTO_REVIEW.md` is updated for every crypto change in this phase.
- [ ] Part A committed and reviewed before Part B starts.
- [ ] Every Part B item is marked UNVERIFIED in `PROGRESS.md`.
- [ ] `DEVICE_TEST_PLAN.md` exists.

## Out of scope for Phase 2

Iroh, gateway logic, routing-mode selection, queued delivery, trust and vouching, cover traffic, device-class-aware policy, Merkle sync, any claim of real-world BLE performance.

## Report back with

1. Commands run with pass/fail output and test counts.
2. The evaluation table, including cases where adaptive flooding did **not** beat the baseline.
3. Files needing human crypto review.
4. Deviations from this spec and why.
5. Everything unverified, especially all of Part B.
