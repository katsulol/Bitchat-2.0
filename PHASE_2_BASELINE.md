# Phase 2 Baseline Freeze Report

**Date**: 2026-10-23  
**Branch**: `phase-3-iroh` (branched from `main` at `da91d9c6f14fd543e5a788457547c3317528c008`)  
**Status**: Phase 2 Complete, Verified on 3 Physical Devices, Frozen.

---

## 1. Environment & Toolchain Versions

- **Git Commit Baseline**: `da91d9c6f14fd543e5a788457547c3317528c008` (clean working tree).
- **Rust Toolchain**: `rustc 1.98.0 (88d9e12ae 2026-08-18)` / `cargo 1.98.0 (797e8a9bc 2026-08-05)`
  - Edition: 2021
  - MSRV: Compatible with Android NDK 30.0.16248370 Clang toolchain.
- **Android NDK**: `30.0.16248370` (`aarch64-linux-android`)
- **Flutter SDK**: `Flutter 3.47.7 • channel stable` / `Dart 3.13.5`
- **Physical Test Fleet (All Verified Running v2.0.4+4)**:
  1. **Redmi Note 10 5G** (`qcprj7wso7irxgjj`): MediaTek Dimensity 700, Android 12/13 (MIUI 14).
  2. **OnePlus Nord CE 3 Lite** (`d4463804`): Qualcomm Snapdragon 695, Android 14 (OxygenOS 14).
  3. **Oppo F25** (`IJGAUOLVSOB6GM8H`): MediaTek Dimensity 7050, Android 14 (ColorOS 14).

---

## 2. Test Execution Baseline

- **`cargo fmt --check`**: Clean (exit code 0).
- **`cargo clippy --all-targets -- -D warnings`**: Clean (0 warnings).
- **`cargo test --all`**: **54 passed, 0 failed, 0 ignored** in 0.91s.
- **`flutter analyze`**: **0 issues found**.
- **Physical Phone Test Matrix**:
  - 3-Phone Peer Discovery: Exactly 2 neighbors recognized per phone via 8-byte `ShortId`.
  - Public Broadcast: Single-path dispatch verified; 1 bubble delivered per peer, 0 duplicates.
  - Addressed Direct: ChaCha20-Poly1305 frame delivered with `[DIRECT]` tag.
  - Chunking & MTU: Reassembly of frames across 20-byte MTU boundary verified on Redmi.

---

## 3. Architecture & Protocol Baseline Specifications

### 3.1 Message Size Limits
- **BLE Link-Layer ATT MTU**: Dynamic tracking per peer connection (range: 23 bytes on Redmi up to 517 bytes on OnePlus/Oppo).
- **Packet Chunking Boundary**: Payloads > `(MTU - 3)` bytes are fragmented using Kotlin layer `[0x01 | seq | total | transfer_id (2B) | slice]`.
- **Mesh Wire MTU**: Default `512` bytes (`DEFAULT_MESH_MTU = 512`). Maximum envelope buffer limit: `4096` bytes (`MAX_ENVELOPE_LEN = 4096`).
- **Maximum Hop Budget**: `7` hops (`MAX_HOPS = 7`). Hop count >= TTL max drops packet immediately.

### 3.2 Wire Envelope Format (`core/src/envelope.rs`)
Total fixed header: 31 bytes.
- `version`: `u8` (`0x01`)
- `msg_type`: `u8` (`0x01` Chat, `0x02` Handshake, `0x03` RouteDiscovery, `0x04` Ack, `0x05` Control, `0x06` Heartbeat)
- `flags`: `u8` (Bit 0: Compressed, Bit 1: Priority, Bit 2: HasSignature, Bits 3-7: Reserved)
- `hop_count`: `u8`
- `ttl_max`: `u8` (`7`)
- `msg_id`: `[u8; 8]` (BLAKE3 keyed hash over sender pubkey, counter, payload)
- `sender_short_id`: `[u8; 8]` (Prefix of Ed25519 public key)
- `recipient_short_id`: `[u8; 8]` (`[0; 8]` for broadcast)
- `payload_len`: `u16` (Network byte order)
- `payload`: Variable bytes up to `payload_len`

### 3.3 Noise Session & Nonce Behavior (`core/src/crypto.rs`, `core/src/mesh/peer.rs`)
- Protocol: `Noise_XX_25519_ChaChaPoly_BLAKE2s` with domain separation prefix `bitchat-noise-xx:`.
- Initiator and Responder exchange 3 handshake messages (`-> e`, `<- e, ee, s, es`, `-> s, se`).
- Once handshake finishes, bidirectional stateless transport keys are derived.
- Replay Protection: 128-packet sliding replay window (`ReplayWindow`) + strictly monotonic counter validation.
- Fail-Closed: Addressed `send_to` requires established Noise session. No plaintext fallback permitted.

### 3.4 Deduplication Behavior (`core/src/mesh/seen.rs`)
- Two-Tier Deduplication:
  1. `LruSeenCache`: LRU cache with capacity 1000 items and 120s TTL (`DEFAULT_SEEN_CACHE_CAPACITY = 1000`, `DEFAULT_SEEN_CACHE_TTL = 120s`).
  2. `BloomSeenCache`: Rotated double-buffered Bloom filters with sub-1% false positive rate for high throughput.
- Suppression: Duplicate frames increment `duplicates_heard`. When duplicate count exceeds `suppress_after` threshold in `AdaptiveFlood`, the scheduled relay is cancelled.

### 3.5 Runtime Ownership & Shutdown (`core/src/api.rs`, `BitchatBleService.kt`)
- **Rust Node Runtime**: Managed as static singleton `SERVICE: OnceLock<RwLock<Option<BitchatCoreService>>>` driven by Tokio multi-thread runtime.
- **Android GATT Server**: Hosted in foreground Android Service `BitchatBleService`.
- **Inter-Process/Bridge**: Flutter polls outbound frames (`bitchat_poll_outbound`) and pumps them into `BitchatBleService` via platform channel; incoming BLE characteristic writes pump bytes into Rust via `bitchat_on_frame_received`.
- **Locking Rule**: Lock `SERVICE` only during synchronous state step/poll; never hold across network IO or unbounded waits.

---

## 4. Preservation Guarantee

The BLE configuration, native GATT server/client logic, packet chunking protocol, and existing C-ABI endpoints remain untouched and fully operational as the local radio transport throughout Phase 3.
