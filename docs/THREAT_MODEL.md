# Threat Model & Security Posture

## 1. Adversary Capabilities & Defenses

| Adversary Capability | Phase 2 Status | Defended? | Mitigation Mechanism |
|---|---|---|---|
| **Eavesdrop packet content** | **Defended** | Yes | End-to-end ChaCha20-Poly1305 AEAD payload encryption established via Noise_XX handshake (`core::crypto`). Relays observe only encrypted ciphertext. |
| **Impersonate sender identity** | **Defended** | Yes | Noise static keys are explicitly signed by the sender's long-term Ed25519 identity (`core::identity`). The handshake payload delivers `(PeerId, Signature)`, verified before session acceptance. |
| **Tamper with wire header fields** | **Defended** | Yes | Strict hand-written decode validator (`core::envelope`) rejects invalid versions, malformed lengths, oversized envelopes (>512 bytes), and hop counts exceeding TTL limits. |
| **Tamper with encrypted payload** | **Defended** | Yes | Poly1305 authentication tag failure immediately rejects altered or truncated payload buffers. |
| **Replay encrypted messages** | **Defended** | Yes | Stateless Noise transport with 128-packet bitmask sliding replay window (`SlidingReplayWindow`). Replays and packets older than the window horizon are rejected. |
| **Relay hop count manipulation** | **Defended** | Yes | Signatures and authentication tags computed over envelopes explicitly exclude relay-mutable `hop_count` (ADR-0002). Relays decrement TTL up to hard cutoff `MAX_HOPS = 7`. |
| **Passive BLE observer (traffic metadata)** | **Partial** | Partial | `ShortId` exposes only 8-byte BLAKE3 prefixes on the wire. Size bucketing (`pad_to_bucket` to 64, 128, 256, 512 bytes) prevents size-based fingerprinting. Active cover traffic scheduled in Phase 6. |
| **Relay observer (hop tracking)** | **Partial** | Partial | Intermediate nodes observe sender and recipient ShortIds to forward packets. In Privacy Mode (Phase 4), ephemeral aliases and onion-like multi-hop encapsulation reduce relay observability. |
| **Timing correlation attack** | **Partial** | Partial | `AdaptiveFlood` injects density-scaled randomized retransmission delays $[0, D(N)]$, disrupting strict temporal correlation across relay hops. |
| **Compromised Gateway / Eavesdropper** | **Defended** (Content) / **Planned** (Routing) | Partial | Content remains protected by end-to-end Noise AEAD even when traversed through internet gateways. Gateway routing trust verification scheduled in Phase 3. |
| **Sybil Flood / DoS Flooding** | **Partial** | Partial | Deduplication caches (`LruSeenCache`, `BloomSeenCache`) drop repeated broadcast storms. `Reassembler` strictly caps concurrent fragments (128 slots, 4KB limit) to defend against memory exhaustion. Rate-limiting and Web-of-Trust prioritization scheduled in Phase 5. |
| **Forged Vouch / Reputation Poisoning** | **Planned** | Not yet | Web-of-Trust cryptographic vouch chains scheduled in Phase 5. |
| **BLE Physical Presence Leakage** | **Known Limitation** | No | Advertising service UUID `0xFD6F` openly discloses Bitchat presence to any BLE scanner in radio range. |

---

## 2. Security Invariants Enforced in Code

1. **Zero Secret Key Leakage**:
   - `Identity`, `Session`, and `KeyStore` implementations implement custom `Debug` formatters outputting `[REDACTED]`.
   - Secret key buffers implement `ZeroizeOnDrop` via `zeroize`.
2. **Strict Cryptographic Crates**:
   - No home-grown cryptographic algorithms. All primitives rely on audited community implementations: `ed25519-dalek`, `snow`, `blake3`.
3. **No Panic in Core Code**:
   - Zero `unwrap()`, `expect()`, or `panic!` calls in non-test production code. All errors propagate via `bitchat2_core::error::Error`.

---

## 3. BLE Advertising & Physical Presence Disclosure

> [!WARNING]
> **Open Radio Presence**: In accordance with the project's honest threat modeling standard, users and integrators must be aware of physical link-layer leakage.

1. **Service UUID Disclosure**:
   - To discover neighboring mesh nodes without internet infrastructure, Bitchat 2.0 peripherals must broadcast BLE advertisements containing the 16-bit Service UUID `0xFD6F`.
   - Any commodity BLE sniffer (or smartphone with an app like nRF Connect) within 10–50 meters can detect that a Bitchat node is active in the vicinity.
2. **Device Address Randomization vs. Static Signatures**:
   - Android operating systems periodically rotate the device's Resolvable Private Address (RPA) at the link layer to reduce long-term device tracking across physical locations.
   - However, because the advertised Service UUID (`0xFD6F`) and GATT service characteristic (`0x2A06`) remain constant, an observer cannot track a *specific* user indefinitely via MAC address, but *can* detect the real-time presence and density of active Bitchat mesh participants in a given physical space.
3. **Operational Recommendation**:
   - If operating in hostile environments where RF detection poses physical peril, users must disable mesh participation or enable device Airplane Mode. Phase 4 will introduce "Passive Sniff Mode" (scanning without advertising) as an additional mitigation.
