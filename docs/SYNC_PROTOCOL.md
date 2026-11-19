# Phase 6 — Task 9 Sync Protocol & Bounded Merkle Reconciliation (`BC2S` v1)

## 1. Status & Feature Classification
- **Sync Subsystem Status**: **Implemented & Simulator/E2E-Tested** — **Default OFF** (`SyncSessionManager::default().is_enabled() == false`).
- **Default Reconciliation Mode**: **`IdInventory` baseline** (compact 17-byte `(logical_id, kind)` list per frame).
- **Bounded `blake3` Merkle Mode (`use_merkle = true`)**: **Experimental** — retained with explicit empirical threshold guidance (see Section 5).
- **Human Cryptographic Review**: **Pending Human Crypto Review** (`docs/CRYPTO_REVIEW.md` covers the new `BC2S` Ed25519 `SyncRecord` signatures and storage subkey derivation).

---

## 2. Wire Protocol Specification (`BC2S` v1)

All reconciliation frames run strictly inside mutually authenticated Noise XX `ApplicationFrame` sessions over approved direct or gateway peers. Gateways see only opaque encrypted packets and never read sync roots, IDs, or record plaintext.

### 2.1 Transport Chunk Framing
Because `Envelope::MAX_ENVELOPE_LEN = 512` bytes and `pad_inner_payload(..., true)` selects the `256`-byte bucket when `2 + app_frame.len() <= 256` (`311`-byte `Envelope <= 512`), every `SyncFrame` is chunked safely inside the `256`-byte inner bucket:

| Field / Constant | Value | Purpose |
|---|---:|---|
| `SYNC_MAGIC` | `b"BC2S"` (4B) | Demultiplexes sync control chunks from user chat & cover traffic |
| `SYNC_WIRE_VERSION` | `1` (1B) | Rejects incompatible protocol versions fail-closed |
| Chunk Header | `9` bytes | `magic(4) \| ver(1) \| msg_seq(u16 BE) \| chunk_idx(u8) \| chunk_total(u8)` |
| `MAX_SYNC_CHUNK_PAYLOAD` | `136` bytes | `136 + 9 + 102 (AppFrame) + 2 = 249B <= 256` bucket (`311B Envelope <= 512`) |
| `MAX_SYNC_CHUNKS_PER_FRAME` | `80` chunks | Bounds single frame reassembly below `MAX_SYNC_RAW_FRAME_BYTES = 10,240B` |
| `MAX_SYNC_RECORDS_PER_FRAME` | `8` records | Caps per-batch record count (`MAX_BATCH_RAW_BYTES = 8,192B`) |
| `MAX_INVENTORY_ITEMS_PER_FRAME` | `64` items | Caps `InventoryOffer` length |

### 2.2 Frame Types (`FrameTag`)
1. `Hello (1)` / `HelloAck (2)`: Declares `conv_id` and `use_merkle`; responder returns `supported: false` if sync is disabled and aborts gracefully.
2. `InventoryOffer (6)`: Streams canonical `(logical_id, SyncRecordKind)` entries (`offset`, `total`, `done`) for `IdInventory` mode.
3. `RootOffer (3)`: Exchanges `(snapshot_id, count, horizon_unix, root_hash)` over the frozen `SyncSnapshot`. Identical `root_hash` finishes immediately in 3 rounds (`Hello` -> `HelloAck` -> `RootOffer` -> `Done`).
4. `NodeRequest (4)` / `NodeResponse (5)`: Traverses differing `[start, end)` key-space intervals (`KEYSPACE_END = 65_536`). Internal splits return `(mid, left_hash, right_hash)`; leaf buckets return `(logical_id, kind)` pairs.
5. `SnapshotMismatch (9)`: Emitted if local `SqliteSyncStore` mutates during traversal (`change_stamp()` O(1) epoch + SQLite `PRAGMA data_version` detection). Restarts snapshot up to `MAX_SNAPSHOT_RESTARTS = 2`, then aborts (`SNAPSHOT_RESTART_LIMIT`).
6. `RecordRequest (7)` / `RecordBatch (8)`: Requests/transfers missing or Tombstone-upgraded `SyncRecord` payloads (`RecordBatch` verified per-record before atomic SQLite commit).
7. `Done (10)` / `Abort (11)`: Explicit session completion or fail-closed error reason.

---

## 3. Deterministic Radix-Interval `blake3` Merkle Tree (`core/src/sync/merkle.rs`)

- **Canonical Ordering**: Records are deduplicated (with `Tombstone` dominance) and sorted by `(logical_id[0..2] BE, created_at_unix, logical_id, kind)` so index insertions/deletions in one bucket never cascade unrelated range hashes, and never rely on `HashMap` iteration.
- **Domain Separation**:
  - Empty range `[start, end)`: `blake3("bitchat2/merkle/empty/v1" || conv_id || start_be32 || end_be32)`
  - Leaf bucket (`<= LEAF_BUCKET_THRESHOLD = 8` items, or `end - start <= 1`, or `depth == MAX_MERKLE_DEPTH`):
    `blake3("bitchat2/merkle/leaf-range/v1" || conv_id || start_be32 || end_be32 || count_be32 || [created_at_unix_be64 || logical_id(16) || kind(1) || record_digest(32)]*)`
  - Internal node (`depth < MAX_MERKLE_DEPTH`):
    `blake3("bitchat2/merkle/node/v1" || conv_id || depth(1) || left_hash(32) || right_hash(32))`
  - Snapshot version ID (`16B` prefix):
    `blake3("bitchat2/merkle/snap/v1" || conv_id || horizon_unix_be64 || count_be32 || root_hash(32))`

---

## 4. Security & Resource Bounds

| Bound | Limit | Enforcement Location |
|---|---:|---|
| Retained archive capacity | `1024` records / conv | `store.rs` (`MAX_SYNC_RECORDS_PER_CONVERSATION`), advances `horizon_unix` on Tombstone eviction |
| Session transfer quota | `256 KiB` (`262,144 B`) | `protocol.rs` (`MAX_SYNC_SESSION_BYTES`), aborts with `SESSION_BYTE_QUOTA_EXCEEDED` |
| Merkle node query cap | `256` queries / session | `protocol.rs` (`MAX_MERKLE_NODES_PER_SESSION`), aborts with `MERKLE_NODE_BUDGET_EXCEEDED` |
| Merkle tree depth cap | `12` levels | `merkle.rs` (`MAX_MERKLE_DEPTH`) |
| Session wall-clock deadline | `30 s` | `protocol.rs` (`SYNC_SESSION_DEADLINE`) |
| Concurrent snapshot restarts | `2` restarts | `protocol.rs` (`MAX_SNAPSHOT_RESTARTS`) |
| Peer & mode eligibility | Normal mode + Approved peer only | Privacy (`mode=1`) and Volatile never write `bitchat_sync.db`; block/revoke calls `abort_peer` |
| Resurrection prevention | `Tombstone > Message` & `horizon_unix` | Verified in `record.rs::verify_for_conversation` & `store.rs::upsert_records_atomic`; TTL never extended |

---

## 5. Measured `IdInventory` Baseline vs Bounded `Merkle` Benchmark (`test_phase6_sync_e2e`)

Measured directly from `cargo test --manifest-path core/Cargo.toml --test test_phase6_sync_e2e -- --nocapture` (8/8 tests passing; fixed `31 B` payload per record -> `227 B` encoded `SyncRecord`, symmetric bidirectional differences per overlap level):

| Set Size (`n`) | Overlap | `IdInventory` Total Bytes | `IdInventory` Rounds | `Merkle` Total Bytes | `Merkle` Rounds | `Merkle` Nodes Queried | Outcome / Winner |
|---:|---:|---:|---:|---:|---:|---:|---|
| `0` | `100%` | `202` | `3` | `229` | `3` | `0` | Convergence PASS (`IdInventory` -27B) |
| `8` | `100%` | `347` | `3` | `229` | `3` | `0` | Convergence PASS (`Merkle` -118B) |
| `8` | `95%` | `1,037` | `5` | `1,368` | `6` | `1` | Convergence PASS (`IdInventory` -331B, -1 round) |
| `8` | `50%` | `2,549` | `5` | `2,880` | `6` | `1` | Convergence PASS (`IdInventory` -331B, -1 round) |
| `8` | `0%` | `4,562` | `5` | `4,893` | `6` | `1` | Convergence PASS (`IdInventory` -331B, -1 round) |
| `32` | `100%` | `782` | `3` | `229` | `3` | `0` | Convergence PASS (`Merkle` 3.4x fewer bytes) |
| `32` | `95%` | `1,964` | `5` | `3,476` | `13` | `8` | Convergence PASS (`IdInventory` -1,512B, -8 rounds) |
| `32` | `50%` | `9,212` | `7` | `11,601` | `18` | `11` | Convergence PASS (`IdInventory` -2,389B, -11 rounds) |
| `32` | `0%` | `17,642` | `11` | `20,031` | `22` | `11` | Convergence PASS (`IdInventory` -2,389B, -11 rounds) |
| `128` | `100%` | `2,595` | `3` | `229` | `3` | `0` | Convergence PASS (`Merkle` 11.3x fewer bytes) |
| `128` | `95%` | `6,318` | `5` | `12,005` | `38` | `33` | Convergence PASS (`IdInventory` -5,687B, -33 rounds) |
| `128` | `50%` | `36,315` | `19` | `45,195` | `62` | `43` | Convergence PASS (`IdInventory` -8,880B, -43 rounds) |
| `128` | `0%` | `70,035` | `35` | `80,166` | `84` | `49` | Convergence PASS (`IdInventory` -10,131B, -49 rounds) |
| `256` | `100%` | `5,061` | `5` | `229` | `3` | `0` | Convergence PASS (`Merkle` 22.1x fewer bytes) |
| `256` | `95%` | `11,970` | `7` | `22,430` | `71` | `64` | Convergence PASS (`IdInventory` -10,460B, -64 rounds) |
| `256` | `50%` | `72,501` | `35` | `90,971` | `126` | `91` | Convergence PASS (`IdInventory` -18,470B, -91 rounds) |
| `256` | `0%` | `139,941` | `67` | `158,539` | `158` | `91` | Convergence PASS (`IdInventory` -18,598B, -91 rounds) |
| `512` | `100%` | `9,993` | `9` | `229` | `3` | `0` | Convergence PASS (`Merkle` 43.6x fewer bytes, 3x fewer rounds) |
| `512` | `95%` | `23,820` | `13` | `42,711` | `129` | `118` | Convergence PASS (`IdInventory` -18,891B, -116 rounds) |
| `512` | `50%` | `144,873` | `67` | `182,243` | `251` | `184` | Convergence PASS (`IdInventory` -37,370B, -184 rounds) |
| `512` | `0%` | Aborted (`256 KiB`) | — | Aborted (`256 KiB`) | — | — | **Fail-Closed Quota PASS** (`SESSION_BYTE_QUOTA_EXCEEDED`, `<= 258 KiB`) |

### Engineering Threshold Recommendation
- Because uniform random UUID `logical_id` diffs spread across many key-space leaves, binary `NodeRequest`/`NodeResponse` traversal sends `64 B` of child hashes + extra BLE RTTs per differing node. Thus `IdInventory` is strictly smaller and lower-latency whenever peers actually differ (`0%`, `50%`, and `95%` scattered overlap).
- `Merkle` cuts wire bytes **11.3x–43.6x** at `n >= 128` when archives match (`100%` overlap root equality check) and stays available behind an explicit opt-in toggle (`use_merkle: bool`) labeled **Experimental**.
- Production default remains **Sync OFF**, and when user-enabled uses **`IdInventory`** by default.
