# Cryptographic Review Registry

Every file touching key generation, signing, verification, Noise handshakes, or symmetric encryption must be registered here with human review directives.

## Review Protocol
1. Verify no custom cryptographic primitives are implemented.
2. Verify all sensitive keys are wiped from memory on drop (`zeroize`).
3. Verify debug implementations never log secret key bytes.
4. Verify nonces are never reused or rolled over insecurely.

## Registered Files
| File | Primitives / Crates | Purpose | Reviewer Checklist |
|---|---|---|---|
| `core/src/identity.rs` | `ed25519-dalek`, `blake3`, `zeroize` | Ed25519 signing, verification, PeerId, ShortId derivation | 1. Key generation uses `OsRng`.<br>2. `ZeroizeOnDrop` implemented for `Identity`.<br>3. `Debug` implementations redact key material.<br>4. Rejection of tampered signatures. |
| *Pending Task 4: `core/src/crypto.rs`* | `snow`, `ed25519-dalek` | Noise_XX handshake with Ed25519 static-key binding | Static key signing, peer identity validation, nonce safety |
