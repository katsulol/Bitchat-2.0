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
