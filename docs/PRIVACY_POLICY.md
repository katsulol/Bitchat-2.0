# Bitchat 2.0 Privacy Policy & Modes Specification

**Status**: Phase 4 Contract Specification  
**Scope**: Operational Differences Between Normal and Privacy Modes, Cryptographic Bucket Padding, Disk Persistence Invariants, and Gateway Quotas.

---

## 1. Operating Modes Overview

Bitchat 2.0 defines two discrete per-message and session policy profiles: **Normal Mode** and **Privacy Mode**.

| Characteristic | Normal Mode | Privacy Mode |
| :--- | :--- | :--- |
| **Outbox Storage** | Encrypted SQLite (AEAD encrypted records) | In-Memory Only (Application-scoped non-persistence for message body) |
| **Inbox Storage** | Persistent encrypted history & dedup | Volatile in-memory only (Lost on process termination) |
| **Delivery Expiry (TTL)** | Default: 24 hours (configurable up to 7 days) | Strict: 15 minutes (configurable up to 1 hour) |
| **Plaintext Padding** | Optional (default unpadded or bucketed) | **Mandatory** (authenticated inner bucket padding) |
| **Iroh Transport Path** | Direct P2P or encrypted Iroh relay | **Direct P2P Preferred** (Relay restricted if requested) |
| **Retry Cap** | 10 attempts | 5 attempts |
| **Send Jitter** | Standard (0.5s – 1.0s) | Enhanced randomized send jitter (0.5s – 2.0s) |

> [!IMPORTANT]
> Neither mode alters cryptographic peer trust or replaces the fundamental identity verification model. Direct IP connections expose IP endpoints; relay connections conceal IP endpoints but reveal packet timing and volume to the relay. Users should select modes based on threat model rather than assuming one path is universally safer.

---

## 2. Authenticated Inner Bucket Padding

### Vulnerability of Unpadded Ciphertext
Encrypted chat messages without uniform padding leak message length and conversational structure (e.g., distinguishing "k", "yes", or detailed sentences) through ciphertext length analysis.

### Inner Authenticated Padding Rule
- Plaintext is padded **BEFORE** AEAD encryption.
- Authenticated inner buckets: **64, 128, 256, 512 bytes**.
- Padding layout:
  ```
  [2-byte big-endian payload_len] || [payload_bytes] || [zero_padding_bytes to bucket]
  ```
- The inner length prefix is authenticated under the AEAD authentication tag (Poly1305).
- Upon decryption, receivers validate:
  $$\text{payload\_len} \le \text{decrypted\_bytes} - 2$$
  Trailing zero bytes beyond `payload_len` are ignored.
- **Exemptions**: Handshake flights, low-level link headers (`FragmentHeader`), and control acknowledgments use fixed minimal framing and are not padded to buckets to avoid channel saturation over BLE.

---

## 3. Persistence & Disk Safety Guarantees

### Application-Scoped Non-Persistence for Privacy Mode
1. **Application Invariant**: The Rust core guarantees zero intentional writes of Privacy message bodies, outbox entries, or decrypted receipts to persistent storage (SQLite, log files, or temp files).
2. **Volatile Retention**: Privacy mode messages reside solely in heap allocations wrapped in zeroizing containers (`zeroize::ZeroizeOnDrop`).
3. **OS Boundary Disclaimer**: The Rust core controls application-managed storage. It explicitly cannot prevent OS-level virtual memory paging (swap file), OEM crash reporting dumps, unprivileged RAM forensic snapshotting, or flash controller wear-leveling artifacts if the physical device or operating system is compromised.
4. **Mode Switching**: Toggling the default mode from Normal to Privacy alters the policy for *future* messages. It does NOT retroactively purge previously committed Normal messages from SQLite, nor does it write active Privacy messages to disk.

---

## 4. Gateway Quotas, Rate Limiting & Fairness

### Default Gateway Stance
- **Gateways are OFF by default**. A node does NOT relay third-party traffic across Iroh unless the user explicitly enables gateway bridging in application settings.
- Toggling gateway bridging OFF immediately:
  1. Issues an empty signed route snapshot withdrawal across active Iroh links.
  2. Rejects any newly admitted inbound bridge frames.
  3. Flushes in-flight forwarding buffers within a bounded 5-second drain window.

### Quota Allocations
To prevent Denial of Service (DoS) and radio starvation:
- **Global Inbound Bandwidth**: Capped at 64 KB/s for relayed third-party packets.
- **Per-Peer Bandwidth**: Capped at 8 KB/s or max 16 envelopes per minute.
- **Control Plane Reservation**: 25% of queue capacity is strictly reserved for handshake and route maintenance traffic, preventing chat flooding from starving session establishment.
- Envelopes exceeding quota are silently dropped at the gateway boundary without generating error packets.

---

## 5. Phase 5 Extensions: Scoped Trust, Coarse Proximity & Volatile Chat

1. **Scoped Gateway Consent (`docs/TRUST_PROTOCOL.md`)**:
   - Cryptographic identity verification (`IdentityBind`) does not grant gateway relay or route announcement permissions. Peers must be explicitly authorized in `TrustStore` or present a single-use `GatewayPairingInvitation` or valid one-hop `CapabilityVoucher`.
2. **Coarse Local BLE Proximity (`docs/PROXIMITY_LIMITATIONS.md`)**:
   - Local RSSI observations are smoothed into coarse categories (`Immediate`, `Near`, `Far`, `Unknown`) and never transmitted over the network or converted into meter estimates.
3. **Volatile Chat Boundaries (`docs/VOLATILE_CHAT.md`)**:
   - Volatile conversations require bilateral authenticated negotiation (`VolatileChat` capability) and never write plaintext or ciphertext to SQLite on compliant nodes. This protects local device storage and cancels expired local outbox retries, but cannot prevent a compromised recipient from copying decrypted content.

