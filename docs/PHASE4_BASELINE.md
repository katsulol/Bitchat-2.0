# Phase 4 Baseline & Compatibility Audit

**Branch**: `phase-4-delivery`  
**Base Commit**: `153a457 docs(phase-4): establish Phase 4 revised implementation contract` (descended from `d3e1cf9`)  
**Workspace State**: Clean working tree, 76/76 passing tests (75 unit/property + 1 Iroh integration).  
**Compiler & Toolchain**:
- `rustc 1.98.0 (88d9e12ae 2026-08-18)`
- `cargo 1.98.0 (797e8a9bc 2026-08-05)`
- `Flutter 3.47.7` (Dart `3.13.5`)
- Android NDK target: `aarch64-linux-android` (API level 26 minimum, API 34 target)

---

## 1. Baseline Dependencies & Crate Versions

| Crate | Version | Role in Core | Security / Integrity Notes |
| :--- | :--- | :--- | :--- |
| `tokio` | 1.43.0 | Async runtime (`rt-multi-thread`, `macros`, `sync`, `time`) | Channel coordination, simulated timeouts |
| `snow` | 0.9.6 | Noise Protocol Framework (`Noise_XX_25519_ChaChaPoly_BLAKE2s`) | Stateless transport, explicit nonces |
| `ed25519-dalek` | 2.1.1 | Identity keys, link auth, route snapshot signatures | Domain-separated signing (`zeroize` enabled) |
| `blake3` | 1.5.5 | KDF, message IDs, packet hashes, seen-cache keys | Fast collision-resistant keyed & unkeyed hashing |
| `bytes` | 1.10.0 | Zero-copy byte buffer handling | ByteMut buffer allocations |
| `zeroize` | 1.8.1 | Memory zeroization on drop for secret keys | Derived zeroization on cryptographic key types |
| `iroh` | 1.4.1 | P2P QUIC transport & NAT traversal bridge | Gateway-to-gateway E2E tunnels |
| `serde` / `serde_json` | 1.0 / 1.0 | Gateway wire serialization | Strictly length-prefixed on wire |
| `thiserror` / `tracing` | 2.0.11 / 0.1.41 | Structured errors & diagnostic logging | No secret leakage in debug formats |

---

## 2. Physical & Simulation Evidence Audit

### Physical Device Deployment
- **Hardware Fleet**:
  - Device 1: OnePlus (`d4463804`, CPH2467, ColorOS 14 / Android 14)
  - Device 2: Redmi (`qcprj7wso7irxgjj`, M2103K19I, MIUI 14 / Android 13)
  - Device 3: Oppo (`IJGAUOLVSOB6GM8H`, CPH2603, ColorOS 14 / Android 14)
- **Verified in Phase 3**:
  - `LINK_HELLO` exchange with 0 MAC padding over BLE GATT.
  - ShortId uppercase case-normalization across platform boundaries.
  - Noise XX simultaneous collision tie-break in `PeerSessionManager`.
  - Foreground service BLE peripheral + central concurrent operation.
- **Qualified Discrepancies**:
  - Physical multi-hop test: Devices were placed in laboratory proximity where direct RF reachability was physically possible; received hop count was not logged to strictly prove multi-hop relay vs direct link in field conditions.
  - Gateway cross-network test: Verified in automated local test harness (`test_phase3_four_node_e2e_mesh_over_real_iroh`) running 4 in-process Iroh endpoints on loopback; physical cross-network cellular vs Wi-Fi test remains pending for Part B.

---

## 3. Wire Envelopes: Authenticated vs Unauthenticated Fields

### Mesh Wire Envelope Layout (`core/src/envelope.rs`)
The wire format consists of a 31-byte fixed header followed by variable payload:
```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|    Version    |   MsgType     |     Flags     |   Hop Count   |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|    TTL Max    |                  Packet ID                    |
+-+-+-+-+-+-+-+-+                                               +
|                         (Bytes 1..8)                          |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                       Sender ShortId (8B)                     |
+                                                               +
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                      Recipient ShortId (8B)                   |
+                                                               +
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|          Payload Len          |         Payload Data ...      |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+                               +
```

### Authentication Boundary Analysis

| Field | Wire Location | Mutable by Relay? | Authenticated by AEAD? | Authenticated by Signature? |
| :--- | :--- | :--- | :--- | :--- |
| `version` | Header [0] | No | No (Unencrypted) | No |
| `msg_type` | Header [1] | No | No (Transport hint) | Inner Type Authenticated |
| `flags` | Header [2] | No | No (Transport flags) | Inner Flags Authenticated |
| `hop_count` | Header [3] | **YES** (relays increment) | **EXCLUDED** | **EXCLUDED** |
| `ttl_max` | Header [4] | No | No | Excluded |
| `msg_id` (Packet ID) | Header [5..13] | No | No (Mesh dedup ID) | Excluded |
| `sender_short_id` | Header [13..21] | No | Unauthenticated Claim | Verified via Session Full Pubkey |
| `recipient_short_id` | Header [21..29] | No | Routing Target | Verified via Session Full Pubkey |
| `payload_len` | Header [29..31] | No | No | Length prefix |
| `payload` (Ciphertext) | [31..] | No | **Poly1305 Tag (16B)** | E2E Authenticated |

### Key Architectural Flaw & Phase 4 Separation
1. **Packet ID vs Logical Message ID**:
   - In Phase 1–3, `msg_id` in the header was computed over `sender || origin_counter || payload`.
   - When a packet was encrypted under Noise, each re-encryption or retry attempt with an incremented nonce resulted in a *new* payload and thus a *new* packet ID.
   - Conversely, if the sender retransmitted the *same* raw packet, mesh nodes' `seen_cache` dropped it as a duplicate, preventing delivery retry.
   - **Resolution**: Phase 4 strictly introduces **Logical Message ID** (`[u8; 16]`) inside the authenticated application frame (inside Noise ciphertext). `msg_id` in the header is recognized strictly as `packet_id` (`[u8; 8]`) for hop-by-hop mesh deduplication.

2. **Handshake Framing Flaw**:
   - `PeerSessionManager` inspected `msg.payload.len() <= 64` to identify handshake flights vs chat payload.
   - **Resolution**: Phase 4 introduces explicit typed handshake framing with version, step (1, 2, 3), attempt ID, and session context.

---

## 4. GATT MTU & Fragmentation Analysis

### Current Implementation State
- **Rust Core**:
  - `core/src/mesh/fragment.rs`: `FragmentHeader` is 4 bytes `[frame_id: u16, index: u8, total: u8]`. Supports up to 255 fragments per frame.
  - `Fragmenter::split(frame, mtu)` works for any `mtu > 4`.
  - `core/src/mesh/relay.rs`: Hardcodes `DEFAULT_MESH_MTU = 512`.
- **Android Kotlin (`BitchatBleService.kt`)**:
  - Client requests `gatt.requestMtu(512)`.
  - Fallback: On failure or unsupported peer, default ATT MTU is 23 bytes (effective payload 20 bytes).
  - Secondary Fragmentation in Kotlin: `splitIntoBlePackets` splits any buffer into `[0x00, ...]` (single) or `[0x01, seq, total, ...]` (chunks) based on `maxPayload = (peerMtu - 3).coerceAtLeast(18)`.

### Discrepancy & Mitigation
- While Kotlin protects BLE transport by sub-fragmenting if MTU negotiation fails, the Rust core's `DEFAULT_MESH_MTU = 512` means envelopes were framed assuming 512-byte link frames.
- **Phase 4 Requirement**: Add deterministic unit tests verifying end-to-end fragmentation and reassembly at minimum MTU (23 bytes, 20-byte chunks, 16-byte fragment payloads), variable MTUs (64, 128, 256), and maximum MTU (512).

---

## 5. Gateway Route Sync State

- Current `RouteSyncFrame` in `core/src/gateway/wire.rs`:
  ```rust
  pub struct RouteSyncFrame {
      pub gateway_id: String,
      pub routes: Vec<String>,
      pub signature: Vec<u8>,
  }
  ```
- **Flaws Identified**:
  1. Lacks connection epoch binding: Old route announcements from previous connections could be replayed.
  2. Lacks sequence numbers: Cannot differentiate out-of-order route updates.
  3. Lacks bounded leases: Routes never expire unless the connection drops.
  4. String-based JSON encoding: Non-canonical, prone to serialization discrepancies.
- **Phase 4 Target (Task 3)**: Canonical binary/canonical-JSON signed route snapshot with `domain`, `version`, `gateway_pubkey`, `epoch`, `sequence`, `lease_seconds`, and `routes`.

---

## 6. Audit Summary & Action Items

| Subsystem | Baseline State | Phase 4 Requirement | Concrete Target |
| :--- | :--- | :--- | :--- |
| **Mesh Deduplication** | LRU & Bloom caches by packet `msg_id` | Separate packet deduplication from recipient logical message deduplication | `core/src/mesh/delivery.rs` |
| **Adaptive Flood** | Delay + counter suppression | Coverage-aware adaptive variant with stale fallback | `core/src/mesh/policy.rs` |
| **Handshake Protocol** | Length-based detection (`<= 64B`) | Typed versioned handshake frame `(step, attempt_id, session_context)` | `core/src/crypto.rs`, `peer.rs` |
| **Gateway Snapshots** | Unsequenced JSON `RouteSyncFrame` | Signed canonical snapshot with epoch, seq, lease, atomic replace | `core/src/gateway/wire.rs`, `routes.rs` |
| **Delivery Engine** | Fire-and-forget in-memory relay | Pure router decision engine + authenticated ACK tracking | `core/src/mesh/delivery.rs`, `router.rs` |
| **Outbox Storage** | In-memory only | Bounded `OutboxStore` trait + memory & encrypted SQLite implementations | `core/src/storage/` |
| **Privacy Mode** | Wire flag only | Authenticated bucket padding (64, 128, 256, 512) + zero-disk outbox enforcement | `core/src/envelope.rs`, `storage/` |
