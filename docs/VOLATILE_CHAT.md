# Best-Effort Volatile Conversations (`VOLATILE_CHAT.md`)

## 1. Scope & Honest Security Boundaries

`core/src/storage/volatile.rs` layers authenticated capability negotiation (`BC2V`), bounded in-memory conversation retention, and pending-send cancellation on top of the Phase 4 zero-disk `MemoryOutboxStore` (`MessageMode::Privacy`).

### What Volatile Chat Guarantees Locally
- **Zero Application Disk Persistence**: Volatile conversation messages are never written to `bitchat_outbox.db` (SQLite), application chat history files, search indexes, or application logs on either the sender or the consenting recipient.
- **Explicit Capability Negotiation & No Silent Downgrade**: A volatile-required conversation negotiates recipient support via `VolatileNegotiationFrame` (`Offer` / `Accept` / `RejectUnsupported` / `CloseSession`) inside an authenticated Noise XX `ApplicationFrame` before transmitting volatile content. If the remote peer does not support volatile chat, sending is rejected (`VolatileSendCheck::RejectedUnsupportedPeer`) and **never** silently downgraded to durable `MessageMode::Normal`.
- **Automatic Monotonic Expiry & Zeroize-on-Drop**: Displayed volatile messages expire after `effective_retention = min(sender_requested, recipient_accepted)` (clamped to `[10s..=900s]`, default max 15 minutes). On expiry or conversation close, `VolatileMessageEntry` buffers are zeroized in Rust heap memory (`zeroize::Zeroize`) and any still-pending (`Queued` / `InFlight`) outgoing sends in `OutboxManager` and `DeliveryCoordinator` are cancelled.

### What Volatile Chat Cannot Guarantee
- **No Remote Deletion Proof**: Once a message has been transmitted over BLE or Iroh and decrypted by the recipient, the sender cannot force deletion on a modified client, recall already-delivered packets, or prevent external photography/screenshots.
- **Managed Runtime & OS Memory Copies**: While `bitchat2_core` zeroizes Rust buffers on drop, copies passed across JNI/FFI into the Dart VM / Android UI heap or OS swap/compositor buffers are subject to garbage collection and cannot be cryptographically guaranteed erased without process termination.
- **Restart & Deduplication Boundary**: Because volatile messages leave zero durable receipts on disk, process restart clears all volatile conversation state and in-memory deduplication caches (`InboxDeduplicator`).
- **Durable Identity & Trust State**: Long-term node identity keys, explicitly approved contacts, and durable vouchers remain stored in their protected keystore/trust store; only volatile conversation state and message bodies are ephemeral.
