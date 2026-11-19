# Bitchat 2.0 — Bounded Conversation Sync Design (`BC2S` v1)

> **Status**: **Design Specification (Task 8) — Optional Experimental Feature (Default OFF, Pending Human Crypto Review)**  
> **Scope Boundary**: Opt-in reconciliation of **retained Normal-mode direct conversation records only** between mutually approved participants (`AuthOutcome::Allow`). Privacy-mode and Volatile (`BC2V`) messages are permanently excluded. No global state tree and no gateway-readable conversation archive exist.

---

## 1. Current Storage Inventory & Gap Analysis

| Store | Location | Purpose | Current Behavior for History | Sync Relationship |
|---|---|---|---|---|
| **Transient Outbox (`bitchat_outbox.db`)** | `core/src/storage/sqlite.rs` | Retry queue for outbound `MessageMode::Normal` messages (`1000` records, `64 KiB` / msg, default 24h TTL) | Stores pending/in-flight/terminal outbound delivery metadata; does **not** store received inbound messages or provide a durable multi-device conversation archive | Not the sync archive; sync reads/writes a dedicated opt-in archive (`bitchat_sync.db`) |
| **Memory Outbox** | `core/src/storage/memory.rs` | Volatile queue for `MessageMode::Privacy` (`64` records max, `900s` default TTL) | Zero SQLite persistence; dropped on exit | **Excluded from sync** |
| **Volatile Sessions (`BC2V`)** | `core/src/storage/volatile.rs` | Per-conversation ephemeral chat (`ZeroizeOnDrop`) | RAM-only retention with `10s..=900s` TTL | **Excluded from sync** |
| **Encrypted Trust DB (`bitchat_trust.db`)** | `core/src/trust/sqlite.rs` | Local approvals, blocks, scoped vouchers, gateway approvals | Local consent ledger only | **Excluded from sync** (zero trust-graph leakage) |
| **Opt-in Sync Archive (`bitchat_sync.db`)** *(New, Default OFF)* | `core/src/sync/store.rs` | Bounded, per-conversation retained archive of `MessageMode::Normal` records & deletion tombstones | Created lazily ONLY if user/test explicitly enables sync | **Authoritative input/target for `BC2S` sync** |

---

## 2. Cryptographic & Authorship Design (Existing Primitives Only)

No new cryptographic primitives are introduced. All encryption, hashing, and signatures reuse pinned dependencies:

1. **At-Rest Archive Encryption (`bitchat_sync.db`)**:
   - Subkey derived strictly from platform DEK: `blake3::derive_key("bitchat2.sync.sqlite.v1", &dek.key)` (zeroized after setup; never derived from public keys).
   - Authenticated per-row `ChaCha20Poly1305` AEAD with random 96-bit `OsRng` nonce and AAD binding `(version, conv_id, logical_id, author, recipient, created_at_unix, expires_at_unix, kind)`.
   - Per-conversation horizon metadata stored in `sync_conversations` protected with local peer identity binding in `sync_meta` (`Trust`/outbox storage fails closed if DEK is missing or corrupt).
2. **Transport Secrecy & Endpoint Authorization**:
   - All `BC2S` frames travel exclusively inside established, mutually authenticated Noise XX `ApplicationFrame`s (`MsgType::Chat` payload with `SYNC_MAGIC = b"BC2S"`). Gateways cannot read sync metadata or roots unless they are an explicit endpoint participant.
   - Before exposing any root hash, Merkle range, inventory, or record, local runtime MUST confirm `AuthOutcome::Allow` for the remote peer and verify the requested `conversation_id` equals `conversation_id(local_peer, remote_peer)`.
   - If an authorized peer is revoked or blocked (`trust_set_peer_state`), any active sync session with that peer aborts immediately.
3. **Immutable Record Authentication (`SyncRecordV1`)**:
   - Noise XX authenticates the immediate link peer, which is **not** sufficient on its own for replayed/synced history provenance. Therefore every syncable record carries an Ed25519 signature over canonical bytes:
     ```text
     sign_payload =
       b"bitchat2/sync-record/v1"      (23 B domain separator)
       || version: u8                  (1 B, == 1)
       || conv_id: [u8; 32]            (32 B)
       || logical_id: [u8; 16]         (16 B)
       || author: [u8; 32]             (32 B Ed25519 pubkey)
       || recipient: [u8; 32]          (32 B Ed25519 pubkey)
       || created_at_unix: u64_be      (8 B)
       || expires_at_unix: u64_be      (8 B)
       || kind: u8                     (1 B: 1 = Message, 2 = Tombstone)
       || body_len: u32_be             (4 B; 0 for Tombstone, <= 4096 for Message)
       || body_hash: [u8; 32]          (blake3(body))
     ```
   - `conv_id` is canonically derived so both endpoints agree deterministically:
     `conv_id = blake3(b"bitchat2/conv/v1" || min(A, B) || max(A, B))` (lexicographical `[u8; 32]` comparison).
   - **Record validation rules upon receipt**:
     1. `conv_id == conversation_id(local_peer, session_peer)` (rejects cross-conversation record injection).
     2. `{author, recipient} == {local_peer, session_peer}` and `author != recipient`.
     3. `Identity::verify(&author, &sign_payload, &author_signature)` succeeds.
     4. `now_unix < expires_at_unix` (expired records rejected; TTL is never extended).
     5. `created_at_unix >= local_sync_horizon_unix` (prevents deleted/evicted history resurrection).
     6. `kind == Tombstone` requires `body.is_empty()`.
   - **Key Compromise / Human Review Caveat**: Flagged for human crypto review — `SyncRecordV1` uses long-term identity Ed25519 signatures on retained Normal-mode archive items; Compromise of a participant's long-term signing key allows signing backdated Normal-mode records within `[local_sync_horizon_unix, now]`. Privacy/Volatile messages are explicitly excluded so forward-secret ephemeral chats never enter signed archival records.

---

## 3. Deletion, Expiry, and Non-Resurrection Rules

1. **Deletion (`SyncRecordKind::Tombstone`)**:
   - Deleting a retained Normal-mode message inserts/syncs a signed `Tombstone` record with the target's `(logical_id, created_at_unix, expires_at_unix)` (plaintext body wiped, replaced by empty body `len = 0`).
   - Wait: only records created locally OR deletions issued by a participant (`author` of tombstone is either original author or local participant overriding local view — locally signed `Tombstone` with `logical_id`, `created_at_unix`, `expires_at_unix` prevents resurrection!). In pairwise 2-party sync, either `A` or `B` can issue a signed `Tombstone` for a `logical_id` inside their shared `conv_id`!
   - **Dominance rule**: For the same `(conv_id, logical_id)`, a valid `Tombstone` ALWAYS wins over a `Message`. An incoming `Message` for a `logical_id` that already has a `Tombstone` is dropped cleanly without overwriting or notifying UI.
2. **Tombstone Retention & Horizon Watermark (`sync_horizon_unix`)**:
   - Tombstones are retained for `TOMBSTONE_RETENTION_SECS = 7 days (604_800s)` or capacity eviction (oldest records when reaching `1024` records/conversation).
   - Whenever an expired or evicted `Tombstone` at `created_at_unix = t` is removed from `bitchat_sync.db`, local storage advances `sync_horizon_unix = max(sync_horizon_unix, t + 1)` atomically inside the same SQLite transaction.
   - Any future incoming record (from any peer) with `created_at_unix < local_sync_horizon_unix` is rejected locally because local deletion knowledge for that timestamp window has already expired/compacted.
   - **Incomplete deletion knowledge defense**: `local_sync_horizon_unix` is mutated ONLY by local tombstone compaction/eviction (or local explicit history clear), NEVER raised merely because a remote peer claims a lower horizon, and never inflated on behalf of another peer without local confirmation — preventing a malicious peer from using fake horizons to erase active history while still preventing old evicted deletions from silently reimporting locally.
3. **Expiry & Capacity Pruning**:
   - Normal-mode retained records retain their finite `expires_at_unix` (`<= 7 days` default sync retention ceiling, never exceeding original expiry). Records past `now_unix >= expires_at_unix` are pruned locally and rejected if replayed by a peer; sync NEVER extends TTLs.

---

## 4. Resource Bounds & Compatibility

| Parameter | Limit | Rationale |
|---|---|---|
| `MAX_SYNC_RECORDS_PER_CONVERSATION` | `1024` | Bounded storage, bounded Merkle depth (`2^10`) |
| `MAX_SYNC_BODY_BYTES` | `4096` | Matches `DEFAULT_MAX_APP_BODY` |
| `MAX_MERKLE_DEPTH` | `12` | Bounds recursion/traversal safely past `1024` records |
| `MAX_MERKLE_NODES_PER_SESSION` | `256` | Prevents range-request amplification/DoS |
| `MAX_SYNC_SESSION_BYTES` | `256 KiB` (`262,144 B`) | Prevents BLE/gateway sync bandwidth exhaustion |
| `SYNC_SESSION_DEADLINE_SECS` | `30 s` | Bounded session lifetime |
| `MAX_SYNC_RECORDS_PER_FRAME` | `8` (and bounded `<= 4000 B` serialized chunk) | Respects Noise/fragment frame limits |
| `MAX_SNAPSHOT_RESTARTS` | `2` | Prevents livelock on concurrent mutations |

- **No Silent Unbounded Fallback**: Exceeding byte, node, frame, record, or deadline bounds immediately aborts the sync session with a typed `Abort` reason and preserves partial validated commits without corrupting state.
- **Backward / Unsupported Peer Compatibility**: Sync starts with `Hello { use_merkle }` inside an `ApplicationFrame`. If the remote peer has sync disabled or runs an older version without sync, it responds `HelloAck { supported: false }` (or drops cleanly) and normal Noise messaging, outbox delivery, and ACKs continue uninterrupted.
- **No Session/Key Restoration**: Sync never exports, imports, or restores Noise session keys, nonces, or trust records.

---

## 5. ID-Inventory Baseline vs. Bounded `blake3` Merkle Tree

We specify **both** protocols over the same snapshot and record verification path so Task 9 can quantitatively measure real transmitted wire bytes, round-trips, and CPU work across set sizes (`0..=512`) and overlap levels (`0%..=100%`):
- **`IdInventory` baseline**: Initiator sends sorted `(logical_id, created_at_unix, kind)` summaries (`25` bytes per record) up to `1024` records (`<= 25.6 KiB` total). When `N == 0` or `N <= 64`, `IdInventory` finishes in **1–2 rounds** with lower overhead than multi-round Merkle tree descent.
- **Bounded `blake3` Merkle reconciliation**: Exchange `RootOffer` (`root_hash: [u8; 32]`, `count`, `snapshot_id`). On equal sets (`100%` overlap), root hashes match and sync completes in **1 round (~80 bytes total)** regardless of `N`! On partially overlapping large sets (`N >= 128`, sparse deltas), range splitting descends only differing subtrees, saving substantial wire bytes over sending all IDs. Task 9 implements both, benchmarks exact wire cost, and documents the concrete crossover threshold.
