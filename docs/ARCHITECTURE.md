# Bitchat 2.0 Architecture

## Overview
Bitchat 2.0 is a decentralized peer-to-peer and mesh messaging platform combining:
1. Short-range Bluetooth Low Energy (BLE) mesh flooding.
2. Long-range Iroh (QUIC, dial-by-public-key) internet gateway bridging.
3. Cryptographic identities without phone numbers or centralized directory servers.

## Phase 1 Layer Stack

```
+-------------------------------------------------------------+
| Session Layer (Noise_XX with Ed25519 Identity Binding)       |
+-------------------------------------------------------------+
| Envelope Wire Format (31-byte header, compact binary codec)  |
+-------------------------------------------------------------+
| Identity & Keystore (Ed25519 signing key, BLAKE3 ShortId)   |
+-------------------------------------------------------------+
| Transport Layer (SimNetwork in Phase 1 / BLE in Phase 2)     |
+-------------------------------------------------------------+
```

## Inherited Concepts vs. Additions
- **Inherited Concepts**:
  - BLE mesh flooding and deduplication.
  - Noise Protocol framework for forward-secret sessions.
  - Key-based identity (no phone numbers or central accounts).
  - 7-hop cap for mesh propagation.
- **Bitchat 2.0 Additions**:
  - Strongly bound Noise XX static key to long-term Ed25519 identity.
  - Adaptive multi-transport smart routing (BLE mesh vs. Iroh gateway vs. delay-tolerant store-and-forward).
  - Dual operational modes: Normal (high deliverability) vs. Privacy (metadata-minimizing).
  - Web-of-Trust vouching and device-class-aware relaying.
