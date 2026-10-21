# Threat Model & Security Posture

## Adversary Matrix

| Adversary Capability | Phase 1 Status | Defended? | Mitigation Mechanism |
|---|---|---|---|
| **Eavesdrop packet content** | Defended | Yes | Noise_XX ChaCha20-Poly1305 payload encryption. |
| **Passive BLE observer (metadata)** | Partial | In Progress | ShortId truncates identity; full PeerId only revealed inside encrypted Noise handshake. Cover traffic / bucketing planned. |
| **Tamper with envelope wire headers** | Defended | Yes | Validation rules reject malformed lengths, excessive hops (>7), and corrupted fields. |
| **Tamper with payload** | Defended | Yes | Poly1305 AEAD tag authentication failures reject corrupt frames. |
| **Replay encrypted packets** | Defended | Yes | Monotonic AEAD nonces reject replayed frames. |
| **Impersonate sender identity** | Defended | Yes | Static Noise key signed by Ed25519 identity key verified before session establishment. |
| **Relay hop count manipulation** | Defended | Yes | Signed messages exclude relay-mutable hop count; `MAX_HOPS = 7` hard drop. |
| **Compromised Gateway / Sybil Flood** | Planned | Not yet | Web-of-Trust and rate limits in Phase 4 & 5. |
| **Traffic analysis & sizing** | Planned | Partial | Size bucketing helper implemented in Phase 1; active cover traffic in Phase 6. |
