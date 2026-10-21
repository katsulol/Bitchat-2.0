# Threat Model & Security Posture

## 1. Adversary Capabilities & Defenses

| Adversary Capability | Phase 1 Status | Defended? | Mitigation Mechanism |
|---|---|---|---|
| **Eavesdrop packet content** | **Defended** | Yes | End-to-end ChaCha20-Poly1305 AEAD payload encryption established via Noise_XX handshake (`core::crypto`). Relays observe only encrypted ciphertext. |
| **Impersonate sender identity** | **Defended** | Yes | Noise static keys are explicitly signed by the sender's long-term Ed25519 identity (`core::identity`). The handshake payload delivers `(PeerId, Signature)`, verified before session acceptance. |
| **Tamper with wire header fields** | **Defended** | Yes | Strict hand-written decode validator (`core::envelope`) rejects invalid versions, malformed lengths, oversized envelopes (>512 bytes), and hop counts exceeding TTL limits. |
| **Tamper with encrypted payload** | **Defended** | Yes | Poly1305 authentication tag failure immediately rejects altered or truncated payload buffers. |
| **Replay encrypted messages** | **Defended** | Yes | Monotonic AEAD nonce counters reject duplicate and out-of-order packets. |
| **Relay hop count manipulation** | **Defended** | Yes | Signatures and authentication tags computed over envelopes explicitly exclude relay-mutable `hop_count` (ADR-0002). Relays decrement TTL up to hard cutoff `MAX_HOPS = 7`. |
| **Passive BLE observer (traffic metadata)** | **Partial** | Partial | `ShortId` exposes only 8-byte BLAKE3 prefixes on the wire. Size bucketing (`pad_to_bucket` to 64, 128, 256, 512 bytes) prevents size-based fingerprinting. Active cover traffic scheduled in Phase 6. |
| **Relay observer (hop tracking)** | **Partial** | Partial | Intermediate nodes observe sender and recipient ShortIds to forward packets. In Privacy Mode (Phase 4), ephemeral aliases and onion-like multi-hop encapsulation reduce relay observability. |
| **Timing correlation attack** | **Planned** | Not yet | Relays currently forward frames without adaptive randomized delays. Jittered retransmission and delay queues scheduled in Phase 2 & 4. |
| **Compromised Gateway / Eavesdropper** | **Defended** (Content) / **Planned** (Routing) | Partial | Content remains protected by end-to-end Noise AEAD even when traversed through internet gateways. Gateway routing trust verification scheduled in Phase 3. |
| **Sybil Flood / DoS Flooding** | **Planned** | Not yet | Uncontrolled broadcast flooding can deplete mesh bandwidth. Rate-limiting, packet deduplication caches, and Web-of-Trust prioritization scheduled in Phase 2 & 5. |
| **Forged Vouch / Reputation Poisoning** | **Planned** | Not yet | Web-of-Trust cryptographic vouch chains scheduled in Phase 5. |

---

## 2. Security Invariants Enforced in Code

1. **Zero Secret Key Leakage**:
   - `Identity`, `Session`, and `KeyStore` implementations implement custom `Debug` formatters outputting `[REDACTED]`.
   - Secret key buffers implement `ZeroizeOnDrop` via `zeroize`.
2. **Strict Cryptographic Crates**:
   - No home-grown cryptographic algorithms. All primitives rely on audited community implementations: `ed25519-dalek`, `snow`, `blake3`.
3. **No Panic in Core Code**:
   - Zero `unwrap()`, `expect()`, or `panic!` calls in non-test production code. All errors propagate via `bitchat2_core::error::Error`.
