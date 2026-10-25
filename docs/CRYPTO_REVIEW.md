# Cryptographic Review Registry

Every file touching key generation, signing, verification, Noise handshakes, or symmetric encryption must be registered here with human review directives.

## Review Protocol
1. Verify no custom cryptographic primitives are implemented.
2. Verify all sensitive keys are wiped from memory on drop (`zeroize`).
3. Verify debug implementations never log secret key bytes.
4. Verify nonces are never reused or rolled over insecurely.
5. Verify sliding replay windows advance only AFTER successful AEAD tag authentication.

## Registered Files
| File | Primitives / Crates | Purpose | Reviewer Checklist |
|---|---|---|---|
| `core/src/identity.rs` | `ed25519-dalek`, `blake3`, `zeroize` | Ed25519 signing, verification, PeerId, ShortId derivation | 1. Key generation uses `OsRng`.<br>2. `ZeroizeOnDrop` implemented for `Identity`.<br>3. `Debug` implementations redact key material.<br>4. Rejection of tampered signatures. |
| `core/src/crypto.rs` | `snow`, `ed25519-dalek` | Noise_XX handshake with Ed25519 static-key binding & stateless ChaChaPoly AEAD transport with sliding replay window | 1. Strict pattern `Noise_XX_25519_ChaChaPoly_BLAKE2s`.<br>2. Domain separation prefix `b"bitchat2/noise-static/v1"` included in static key signature.<br>3. Handshake verifies remote identity signature before transport transition.<br>4. Stateless transport mode uses explicit 64-bit nonce carried in payload header.<br>5. 128-packet sliding replay window rejects duplicate nonces and stale frames.<br>6. Replay window advances only after successful AEAD MAC verification.<br>7. Redacted `Debug` output prevents session key leakage. |
| `core/src/mesh/peer.rs` | `snow`, `ed25519-dalek`, `blake3` | Multi-hop end-to-end encrypted session coordination and relay isolation | 1. Relays forward envelopes as opaque byte slices; no relay ever has access to plaintext or session keys.<br>2. Handshake payloads verify mutual Ed25519 identity signatures over ephemeral Noise static keys.<br>3. Remote PeerId is validated and bound before establishing transport sessions.<br>4. Messages carry 8-byte nonces and 16-byte Poly1305 MAC tags.<br>5. Tampered ciphertext is rejected before local delivery.<br>6. Out-of-order packets survive bounded relay hops without session desynchronization. |
| `core/src/gateway/wire.rs` | `ed25519-dalek`, `serde_json` | Signed RouteSnapshots with epoch and sequence monotonicity | 1. Canonical sign bytes use explicit domain separation `b"bitchat2-route-snapshot-v1"`.<br>2. Author signature verified against connection-bound `PeerId`.<br>3. Epoch and sequence monotonicity prevents snapshot replays.<br>4. Bounds on snapshot size (max 128 recipients, max 3600s lease). |
| `core/src/mesh/delivery.rs` | `ed25519-dalek`, `rand` | Authenticated application framing, logical IDs, and end-to-end delivery ACKs | 1. 16-byte logical ID generated once and bound inside authenticated AEAD payload.<br>2. ACKs authenticated by intended recipient session key.<br>3. Retries use fresh ephemeral packet IDs and nonces to avoid replay/seen-cache suppression.<br>4. Receiver deduplication keyed by authenticated sender + logical ID. |
| `core/src/storage/key.rs` | `zeroize` | Key provider contract and data encryption key protection | 1. `DataEncryptionKey` implements `ZeroizeOnDrop`.<br>2. `Debug` implementation redacts raw key bytes.<br>3. Production keystore decoupled from signing key material. |
| `core/src/storage/sqlite.rs` | `chacha20poly1305`, `rand` | Encrypted SQLite storage for Normal Mode messages | 1. Message plaintext encrypted with ChaCha20-Poly1305 before SQL INSERT.<br>2. 96-bit random nonces generated via CSPRNG (`rand::thread_rng`).<br>3. AAD strictly binds schema, key version, logical ID, sender, recipient, and mode.<br>4. Tampered ciphertext fails AEAD authentication; no corrupted plaintext leaked.<br>5. Zero-disk guarantee: rejects Privacy Mode records from touching disk. |
| `core/src/envelope.rs` | Standard library | Authenticated inner bucket padding (64, 128, 256, 512 bytes) | 1. Plaintext padded to standard buckets *before* AEAD encryption.<br>2. 2-byte big-endian payload length authenticated under Poly1305 tag.<br>3. Decryption validates payload length before stripping padding. |
