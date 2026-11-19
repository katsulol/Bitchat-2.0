# Experimental Cover Traffic (`BC2C` v1) — Architecture & Threat Model

> **Status**: **Experimental / Default OFF / Pending Human Crypto Review**  
> Cover traffic is disabled by default (`enabled = false`) and must be explicitly enabled by the user or an experiment harness. Enabling this feature provides **bounded direct-link traffic padding** ONLY; do not interpret it as anonymous routing or full metadata resistance.

## 1. Threat Model & Scope
- **Protected adversary model**: A passive observer monitoring packet sizes and coarse inter-packet timing across an active direct BLE link (`SendTarget::DirectBle`) between two peers with an already established Noise XX session.
- **What it does NOT protect against (explicit limitations)**:
  - Does **not** hide device presence or initial Noise handshake (`MsgType::Handshake`) frames, fragment boundaries, or control frames sent outside `ApplicationFrame`.
  - Does **not** conceal multi-hop mesh topology, global traffic analysis, or active probing attackers.
  - Does **not** open or reopen sleeping BLE/Iroh connections solely to emit cover traffic.

## 2. Protocol & Security Invariants (`core/src/mesh/cover.rs`)
1. **Default OFF & Capability Negotiation**: Control frames (`Offer`, `Accept`, `Reject`, `Disable`) and `Dummy` frames use magic `b"BC2C"` v1 inside encrypted Noise `ApplicationFrame`s on approved direct sessions (`AuthOutcome::Allow`). Unsupported, unapproved, or pending peers receive zero dummy frames.
2. **Fixed Payload Buckets**: Dummy fillers are sampled strictly from `COVER_DUMMY_BUCKETS = [32, 96, 224]` filled from `OsRng` (`OsCoverRng`) and subsequently padded by `pad_inner_payload` to standard `PADDING_BUCKETS` (`[256, 512]`). Each dummy frame uses a fresh Noise AEAD nonce.
3. **Zero Side Effects on Receiver**: `BitchatCoreService::poll_inbound` discards decoded `CoverFrameKind::Dummy` frames before `InboxDeduplicator`, `DeliveryCoordinator` ACK emission, `SqliteOutboxStore`, `VolatileSessionManager`, and UI `BridgeMessage` delivery. Zero ACK loops or response amplification can occur.
4. **Hard Budgets & Overrides**:
   - Rolling 60-second byte cap (`DEFAULT_COVER_MAX_BYTES_PER_MINUTE = 2048`), per-tick burst limit (`1`–`2` frames), randomized interval jitter (`[min_interval, max_interval]`, production default `1.5s–6.0s`), and session lifetime cap (`1800s`).
   - Immediately overridden (suppressed) when `RelayResourceProfile` is `Paused` or `Minimal`, `ThermalState` is `Serious` or `Critical`, or any real outbound message is `Queued`/`InFlight` (`has_real_pending == true`).

## 3. Off/On Evaluation Plan (Task 12 / Task 13)
- **Simulation (Task 12)**: Compare `cover = OFF` vs `cover = ON` on idle vs bursty sessions, measuring wire bytes/min overhead, dummy-to-real byte ratio, real-message P95 latency delta, and frame-size/inter-arrival distribution entropy under the stated passive link observer.
- **Physical Testing (Task 13)**: Measure BLE battery/current telemetry variance during matched idle runs if physical phones are run by a human tester; keep Default OFF until empirical benefit outweighs power cost.
