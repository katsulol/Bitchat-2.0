# Architectural Decision Records (ADR)

## ADR-0001: Pure Rust Core Workspace with hand-written wire codec
- **Context**: Bitchat 2.0 needs deterministic, compact, BLE-friendly packet framing without serialization overhead.
- **Decision**: `bitchat2_core` implemented as standalone Rust workspace member with zero non-Rust dependencies in Phase 1. Hand-written big-endian codec instead of Serde for byte-exact wire envelopes.
- **Consequences**: Exact byte layout control, minimal binary footprint, verifiable memory bounds.

## ADR-0002: Relay-Mutable Fields Excluded from Signatures and MACs
- **Context**: Mesh relays need to increment `hop_count` as packets traverse nodes up to `MAX_HOPS = 7`.
- **Decision**: Any digital signature or message authentication code (MAC) computed over an envelope MUST explicitly exclude `hop_count`.
- **Consequences**: Packets remain verifiable by destination and intermediate hops while allowing relays to track TTL/hop bounds without invalidating cryptographic authenticity.
