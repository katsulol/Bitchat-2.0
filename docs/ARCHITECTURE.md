# Bitchat 2.0 Architecture

## 1. System Overview

Bitchat 2.0 is a decentralized peer-to-peer and mesh messaging platform designed to operate under zero-connectivity constraints as well as hybrid mesh-internet topologies:
- **Local Mesh**: Nearby devices communicate via peer-to-peer Bluetooth Low Energy (BLE) mesh links, propagating messages hop-by-hop up to 7 hops without internet access.
- **Internet Bridging**: Nodes with internet connectivity act as transparent gateways, tunneling messages via Iroh (dial-by-public-key over QUIC) to remote gateways that rebroadcast into distant meshes.
- **Identity & Security**: Cryptographic Ed25519 keys replace phone numbers and centralized directories; sessions provide forward secrecy via Noise_XX with explicit Ed25519 static-key binding.

---

## 2. Layered Architecture

```mermaid
flowchart TD
    subgraph UI ["User Experience (Phase 2+)"]
        Flutter["Flutter / Dart UI"]
        FRB["flutter_rust_bridge FFI"]
    end

    subgraph Protocol ["Protocol & Routing (Phases 3-6)"]
        Modes["Normal / Privacy Mode Controller"]
        WoT["Web-of-Trust & Vouching Engine"]
        Router["Smart Routing & Delivery Manager"]
        IrohBridge["Iroh QUIC Gateway Bridge"]
    end

    subgraph CorePhase1 ["bitchat2_core (Phase 1 Foundation)"]
        Session["Session Layer\n(Noise_XX with Ed25519 Identity Binding)"]
        Envelope["Envelope Wire Format\n(31-byte header, hand-written binary codec, padding)"]
        Identity["Identity & KeyStore\n(Ed25519 signing key, PeerId, BLAKE3 ShortId)"]
        Transport["Transport Layer\n(async Transport trait: SimNetwork / BLE)"]
    end

    Flutter --> FRB
    FRB --> Modes
    Modes --> WoT
    Modes --> Router
    Router --> IrohBridge
    Router --> Session
    Session --> Envelope
    Envelope --> Transport
    Identity -.-> Session
    Identity -.-> Envelope
```

### Module Responsibilities

1. **`core::identity`**:
   - Manages cryptographic long-term identity via Ed25519 keypairs.
   - Computes canonical `PeerId` (32-byte public key) and `ShortId` (first 8 bytes of `blake3(public_key)`).
   - Enforces memory zeroization (`zeroize`) and redacted `Debug` representations.
   - Defines abstract `KeyStore` trait with `MemoryKeyStore` and development `FileKeyStore`.

2. **`core::envelope`**:
   - Formats and parses byte-exact, compact 31-byte binary headers without serialization frameworks.
   - Tracks packet lineage via immutable `msg_id` (`blake3(sender_pubkey || counter || blake3(payload))`).
   - Supports relay-mutable `hop_count` with maximum TTL of 7 hops.
   - Reversible length-prefixed size bucketing (`pad_to_bucket`) for metadata analysis protection.
   - Excludes relay-mutable `hop_count` from authentication bytes (`auth_bytes`).

3. **`core::crypto`**:
   - Negotiates mutual authentication and forward-secret session keys via `Noise_XX_25519_ChaChaPoly_BLAKE2s`.
   - Strongly binds ephemeral/static Noise keys to long-term Ed25519 identities via signed handshake payloads.
   - Enforces bidirectional ChaCha20-Poly1305 AEAD transport encryption with replay prevention.

4. **`core::transport`**:
   - `Transport` trait defines asynchronous `send`, `broadcast`, `recv`, and `neighbors`.
   - `SimNetwork` provides an in-memory testbed with configurable adjacency topologies, latency simulation, and deterministic seeded packet loss.

---

## 3. Inherited Concepts vs. Bitchat 2.0 Additions

| Concept / Mechanism | Origin / Inherited | Bitchat 2.0 Addition | Rationale |
|---|---|---|---|
| **Identity Model** | Inherited (Cryptographic keypair) | None (Ed25519 used directly) | Eliminates user registration, phone numbers, and central servers. |
| **Noise Protocol** | Inherited (`Noise_XX_25519_ChaChaPoly_BLAKE2s`) | **Identity-Bound Static Key Signing** | Standard Noise XX does not verify that a static key belongs to a specific long-term identity. Bitchat 2.0 requires Ed25519 signatures in the handshake payload. |
| **Mesh Propagation** | Inherited (Controlled flooding with 7-hop cap) | Planned adaptive backoff and deduplication | 7-hop TTL bounds broadcast storms while covering typical local clusters. |
| **Relay-Mutable Header** | Inherited (Hop counting in headers) | **MAC/Signature Exemption Rule (ADR-0002)** | Relays increment hop count without invalidating end-to-end payload authenticity or cryptographic signatures. |
| **Transport Model** | Single radio in reference apps | **Multi-Transport Smart Routing (BLE + Iroh QUIC)** | Transparently switches between short-range BLE mesh and global Iroh peer-to-peer gateways. |
| **Trust Evaluation** | Primitive or absent | **Web-of-Trust & Device-Class Relaying** | Distinguishes trusted peers, suppresses malicious flooders, and protects battery on constrained nodes. |
| **Metadata Protection** | Best-effort / unpadded | **Reversible Size Bucketing & Cover Traffic** | Obscures message size distributions to resist passive traffic analysis. |
