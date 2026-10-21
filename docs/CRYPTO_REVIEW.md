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
