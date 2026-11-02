# Architectural Decision Records (ADR)

## ADR-0001: Pure Rust Core Workspace with hand-written wire codec
- **Context**: Bitchat 2.0 needs deterministic, compact, BLE-friendly packet framing without serialization overhead.
- **Decision**: `bitchat2_core` implemented as standalone Rust workspace member with zero non-Rust dependencies in Phase 1. Hand-written big-endian codec instead of Serde for byte-exact wire envelopes.
- **Consequences**: Exact byte layout control, minimal binary footprint, verifiable memory bounds.

## ADR-0002: Relay-Mutable Fields Excluded from Signatures and MACs
- **Context**: Mesh relays need to increment `hop_count` as packets traverse nodes up to `MAX_HOPS = 7`.
- **Decision**: Any digital signature or message authentication code (MAC) computed over an envelope MUST explicitly exclude `hop_count`.
- **Consequences**: Packets remain verifiable by destination and intermediate hops while allowing relays to track TTL/hop bounds without invalidating cryptographic authenticity.

## ADR-0003: Stateless Noise Transport with Explicit Nonce and Sliding Replay Window
- **Context**: Mesh networks experience packet loss, delays, and out-of-order delivery. Stateful transport mode in Noise permanently desynchronizes after a single dropped packet because nonces are implicitly incremented.
- **Decision**: Adopt snow's `StatelessTransportState`. Every encrypted frame explicitly carries an 8-byte big-endian nonce `[nonce: 8B] || [ciphertext + 16B Poly1305 tag]`. Receivers maintain a 128-packet sliding replay window (bitmask) to reject duplicates and stale nonces while accepting moderate out-of-order arrivals.
- **Consequences**: Eliminates session death from dropped packets. Memory overhead is 24 bytes per session. Bitmask check is constant-time $O(1)$. Replay window advances only after successful AEAD MAC verification, preventing blind denial-of-service desync.

## ADR-0004: Domain Separation in Ed25519 Static Key Handshake Binding
- **Context**: Cross-protocol or cross-context signature replay attacks can occur if identity keys sign raw public keys without context.
- **Decision**: Prepend fixed context prefix `b"bitchat2/noise-static/v1"` before signing or verifying the Noise static public key.
- **Consequences**: Cryptographically binds the signature to Bitchat 2.0 Noise static key verification and prevents reuse in other signature contexts.

## ADR-0005: Exact LRU Deduplication Cache as Default, Bloom Filter as Compact Alternative
- **Context**: Mesh flooding requires nodes to detect and drop previously seen packets to avoid broadcast storms and loops. A Bloom filter saves RAM but exhibits a non-zero false positive rate (FPR), which causes genuine, unseen messages to be silently dropped.
- **Decision**: Make `LruSeenCache` (exact hash map with bounded capacity and TTL eviction) the default deduplication cache in `bitchat2_core`. Retain `BloomSeenCache` (dual rotating Bloom filters with double hashing) as an optional, space-constrained alternative for constrained embedded microcontrollers or Phase 6 scaling benchmarks.
- **Consequences**: 0% false positive rate guarantees no genuine user messages are erroneously suppressed. Memory cost for 1,000 entries is ~24 KB (negligible on modern smartphones and BLE companion devices). Bloom filter provides $O(1)$ constant bit-level overhead when memory is strictly constrained.

## ADR-0006: Adaptive-Delay and Counter-Based Broadcast Suppression Policy
- **Context**: Naive uncontrolled mesh flooding causes severe packet collisions, channel saturation, and power drain in dense radio environments (the classic broadcast storm problem).
- **Decision**: Implement `AdaptiveFlood` combining density-scaled random forwarding delay $[0, D(\text{neighbor\_count})]$ and counter-based duplicate suppression threshold $k=2$. Envelopes flagged with priority bypass delay and are never suppressed. Immediate direct unicast neighbors bypass the mesh flood.
- **Attribution & Literature Inspiration**: This policy is inspired by general broadcast-storm mitigation concepts in ad-hoc networking literature, including adaptive-delay and counter-based schemes such as those discussed by Wang, Li, and Li. It is an independent engineering synthesis designed for BLE mesh characteristics and is NOT a verbatim reproduction or formal implementation of any specific published academic algorithm.
- **Consequences**: In dense clusters, redundant retransmissions are suppressed once duplicate neighborhood reception confirms coverage, reducing contention while preserving high reachability. Priority alerts proceed unimpeded. Extension point left for Phase 5 device-class and power-aware routing.

## ADR-0007: Dual-Role BLE GATT Architecture via Native Android Kotlin Platform Channel
- **Context**: A mesh peer on mobile must act as BOTH a BLE peripheral (advertising service UUID `0xFD6F`, hosting a GATT server with Write and Notify characteristics) and a BLE central (scanning for peers, connecting, subscribing to notifications, writing fragments). Existing Flutter plugins either only support Central (`flutter_blue_plus`) or have outdated unmaintained Android 13/14 permissions (`flutter_ble_peripheral`), failing to synchronize simultaneous scanning and advertising within an Android Foreground Service.
- **Decision**: Implement a native Android Kotlin PlatformChannel service (`BitchatBleService.kt` + `MainActivity.kt`). The service runs in the foreground with a persistent notification (`connectedDevice` type), managing simultaneous `BluetoothLeScanner` and `BluetoothLeAdvertiser` + `BluetoothGattServer`. Flutter communicates with this native service via standard `MethodChannel("com.bitchat.mesh/ble")` and `EventChannel("com.bitchat.mesh/events")`.
- **Consequences**: Zero unmaintained third-party plugin dependencies. Guaranteed compatibility with Android 12 through 15 runtime permissions (`BLUETOOTH_SCAN`, `BLUETOOTH_ADVERTISE`, `BLUETOOTH_CONNECT`, `POST_NOTIFICATIONS`). The Android Foreground Service prevents OS battery optimizers / Doze mode from shutting down background mesh discovery.

## ADR-0008: Separation of Logical Message ID and Transport Packet ID
- **Context**: In Phase 1–3, `msg_id` in the envelope header was computed as a hash of the raw wire payload. When messages were retried, reusing the same envelope caused intermediate relays' `SeenCache` to suppress retransmissions; conversely, generating a new nonce produced a new payload hash, which receivers could not correlate with the original message.
- **Decision**: Introduce a 16-byte `Logical Message ID` bound inside the authenticated application frame (inside Noise AEAD ciphertext). The 8-byte `Packet ID` in the envelope header is strictly ephemeral and regenerated for every transmission attempt or fragment set.
- **Consequences**: Intermediate mesh nodes perform deduplication using ephemeral `Packet ID` without suppressing retries. Recipient endpoints deduplicate display using `(authenticated_sender, logical_id)` to guarantee exactly-once display.

## ADR-0009: End-to-End Authenticated Delivery Protocol with Strict Terminal States
- **Context**: Users require reliable direct messaging with verifiable receipt confirmation. Relaying nodes or gateways cannot be trusted to attest delivery of end-to-end encrypted payloads.
- **Decision**: Implement an end-to-end `ApplicationFrame::Ack` generated solely by the destination endpoint after AEAD verification. Logical message states follow a strict monotonic finite state machine: `Queued -> InFlight -> {Delivered, Failed, Expired, Cancelled}`. Terminal states cannot be downgraded or mutated by late-arriving packets. Duplicate received messages trigger a fresh ACK under the active session without duplicate display.
- **Consequences**: Cryptographically verifiable delivery guarantees across arbitrary relays and gateways. Prevents phantom delivery confirmations and terminates retry loops cleanly.

## ADR-0010: Bounded Outbox Architecture with Injected Key Provider
- **Context**: Reliable delivery requires an outbox that survives temporary link disconnections, session renegotiations, and platform sleep cycles, while strictly observing Normal vs Privacy mode constraints.
- **Decision**: Define a bounded `OutboxStore` trait implemented for both ephemeral In-Memory storage (mandatory for Privacy Mode) and encrypted SQLite storage (for Normal Mode). Record payloads are encrypted using AEAD (ChaCha20-Poly1305) with versioned data encryption keys. In Part A, an injected key provider allows deterministic unit testing; Part B integrates the platform Android Keystore.
- **Consequences**: Privacy mode achieves an application-managed zero-disk guarantee for message bodies (no intentional persistence to SQLite, temp files, or logs; excludes OS-level swap, OEM crash dumps, and hardware forensics). Normal mode achieves encrypted persistence without plaintext leakage in SQLite WAL or index files.

## ADR-0011: Pure Router Decision Function with Typed Reason Codes
- **Context**: Message routing across heterogeneous links (direct BLE, multi-hop BLE mesh, Iroh P2P, Iroh gateway relay) involves multiple constraints (session availability, link readiness, mode policies, rate limits). Imperative routing logic scattered across async tasks leads to race conditions and test fragility.
- **Decision**: Implement a pure, synchronous routing decision function `Router::decide(...) -> RouteDecision`. The decision engine takes an immutable snapshot of network state, identity sessions, and message policy, and returns a deterministic action with a typed enum `RouteReason` (e.g., `DirectBleAvailable`, `SessionHandshakeRequired`, `IrohGatewayRouteFound`, `RelayRestrictedInPrivacyMode`, `NoRouteAvailable`).
- **Consequences**: Enables table-driven and property-based unit testing of all routing branches without async timing hazards. UI and logging consume structured, type-safe reasons.

## ADR-0012: Authenticated Inner Bucket Padding
- **Context**: Variable-length ciphertexts leak information about message length, language patterns, and conversational structure. Padding ciphertext after encryption cannot be authenticated against truncation or manipulation.
- **Decision**: Pad plaintext prior to AEAD encryption into fixed bucket sizes (64, 128, 256, 512 bytes). The inner length prefix is authenticated under the AEAD tag. Padded flag is tracked in frame metadata.
- **Consequences**: Hides message length across radio and gateway transports. Unpadded handshakes and low-level control frames avoid wasted BLE bandwidth.

## ADR-0013: Explicit Gateway Authorization and Single-Use Pairing Invitations
- **Context**: In Phase 4, incoming gateway bridge peers that passed Ed25519 identity-binding verification were automatically approved in `BridgeService`. Proving ownership of an Ed25519 key pair does not prove permission to consume a node's gateway bandwidth or inject route snapshots.
- **Decision**: Separate identity verification from gateway authorization. Incoming bridge peers must be explicitly pre-authorized in `TrustStore` (`UseLocalGateway` / `AcceptGatewayRoutes`) or present a valid, unexpired, single-use `GatewayPairingInvitation` signed under `b"bitchat2-gateway-invite-v1"`.
- **Consequences**: Strangers cannot self-authorize gateway access. Pairing tokens are consumed on first admission and cannot be replayed.

## ADR-0014: Scoped One-Hop Capability Vouchers without Sybil Identity Claims
- **Context**: Offline and semi-connected groups need a way for a trusted operator to introduce peers to a gateway or relay without manual QR scanning between every pair of devices. However, open decentralized trust graphs are vulnerable to Sybil amplification and transitive pollution.
- **Decision**: Implement one-hop Ed25519 `CapabilityVoucher` (`b"bitchat2/capability-voucher/v1"`) granting a strict subset of capabilities (`DirectMessage | UseLocalGateway | AcceptGatewayRoutes |VolatileChat`). `VouchOthers` is never delegable via voucher (`A -> B -> C` is structurally impossible). Enforce per-issuer active caps (8), hourly issuance rate limits (4/hr), global vouched caps (64), max 7-day expiry, and cascade invalidation on issuer revocation.
- **Consequences**: Bounds worst-case compromise of a single trusted voucher issuer without claiming global Sybil resistance.

## ADR-0015: Three-State Resource-Budgeted Relaying with Traffic-Class Token Buckets
- **Context**: Unbounded third-party BLE mesh flooding under low battery or thermal throttling can starve the device's own user messages, E2E delivery ACKs, and Noise handshakes.
- **Decision**: Introduce `ResourceBudgetManager` (`Normal`, `Constrained`, `Minimal`) with battery hysteresis (`20%/25%` and `10%/15%`) and three independent token buckets (`SelfOriginatedAndControl`, `TrustedPeerRelay`, `ThirdPartyMeshFlood`) weighted 8:4:1 in `Minimal` mode.
- **Consequences**: Self-originated messages, handshakes, and ACKs never starve during third-party broadcast storms or low-battery states.

## ADR-0016: Coarse Local BLE Proximity Categories and Honest Volatile Chat Boundaries
- **Context**: Converting raw BLE RSSI to meter distances produces false precision due to multipath fading and body shadowing. Similarly, calling disappearing messages "guaranteed remote deletion" misleads users about compromised recipients.
- **Decision**: Classify BLE RSSI into four coarse local categories (`Immediate`, `Near`, `Far`, `Unknown`) using a 5-sample median window, 5–6 dB hysteresis, and 30s staleness decay. Implement `VolatileSessionManager` requiring mutual authenticated negotiation (`VolatileChat`) and `ZeroizeOnDrop` in-memory retention while documenting that a modified recipient cannot be forced to delete received plaintext.
- **Consequences**: Eliminates false spatial precision and aligns volatile chat security claims with verifiable local enforcement.


