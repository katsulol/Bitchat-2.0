---
name: crypto-review
description: Protocol for reviewing and auditing cryptographic operations in bitchat2_core.
---

# Crypto Review Skill

## Rule
Flag any change touching key generation, Noise handshakes, signing, or encryption for manual review.
Never auto-approve cryptographic changes.

## Requirements
1. Every file touching cryptographic operations must carry a `// CRYPTO-REVIEW:` header comment explaining:
   - What the code does.
   - Which crates / primitives it relies on.
   - Specific invariants a human reviewer must check.
2. Every crypto-touching file must be logged in `docs/CRYPTO_REVIEW.md`.
3. Do not invent custom cryptography. Use audited crates specified in the specification.
