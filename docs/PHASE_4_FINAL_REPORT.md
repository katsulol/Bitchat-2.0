# Bitchat 2.0: Complete Implementation Note, Source Code & Verification Logs

**Repository**: `https://github.com/katsulol/Bitchat-2.0.git`  
**Active Branch**: `phase-4-delivery`  
**Latest Commit**: `94104bd` (`fix(gateway): filter local identity from route announcements and active gateway routes`)

---

## 1. Chronological Git Commit Log (Phase 1 – Phase 4)

```text
94104bd fix(gateway): filter local identity from route announcements and active gateway routes
b74d5d3 feat(gateway): incoming mutual auto-approval on verified identity binding and live UI polling
27debf1 feat(delivery): exponential retry backoff and instant peer-reconnection outbox wakeup
4abae0d fix(ui): eliminate right overflow in diagnostics bottom sheet
8ce0b97 fix(phase-4): sanitize hex string parsing and update arm64-v8a native shared library
c9ad276 docs(phase-4): update progress log and crypto registry for Part B completion
e5cd18a feat(phase-4): integrate platform keystore, outbox delivery events, and diagnostics in Flutter UI
268ffcf feat(phase-4): implement service-owned Android Keystore DEK provider and backup exclusion
b8e7c83 feat(phase-4): integrate outbox and delivery lifecycle into public runtime and FFI
e248908 docs(phase-4): update Part A acceptance review with honest scope and boundary accounting
1e27fbe feat(phase-4): add framing boundary tests and protect control traffic from bandwidth starvation
af3ce82 feat(phase-4): bind gateway snapshots to authenticated connection epoch and add reconnect replay tests
94c0595 feat(phase-4): enforce privacy iroh relay restriction at transport capability layer
bbabd4e chore(phase-4): update evaluation results from benchmark run
1f9fea7 docs(phase-4): produce Part A acceptance review and verification audit
4534c47 test(phase-4): add e2e delivery integration test and clarify payload framing above 512 bytes
ec5e773 docs(phase-4): update crypto review registry and progress log for Part A completion
e1756f4 feat(phase-4): implement authenticated bucket padding, gateway rate limits, and control traffic reservation
f4456f6 feat(phase-4): implement pure decision router, typed route reasons, and transport policy
f6620c6 feat(phase-4): implement outbox persistence, AEAD record encryption, zero-disk privacy, and non-blocking simulation step_at
9c6ff25 feat(phase-4): implement delivery protocol, authenticated application frames, and deduplication
47c91e3 feat(phase-4): implement canonical signed route snapshots with epoch and sequence monotonicity
96f67ed feat(phase-4): implement typed versioned handshake frames, flight caching, and identity verification
bbed153 feat(phase-4): implement coverage-aware adaptive relay policy resolving partitioned bridge suppression
171c5d1 docs(phase-4): establish baseline audit, delivery protocol, privacy policy, and ADRs
153a457 docs(phase-4): establish Phase 4 revised implementation contract
d3e1cf9 feat(phase-3): complete multi-device BLE E2E routing, automated Iroh route sync, QR scanner, and link identity verification
1b80fe0 feat(phase-3): implement Iroh internet gateway bridge with end-to-end encryption
8de860a chore(deps): pin iroh 1.4.1 and configure Android NDK clang
52ad717 docs(phase2): freeze working Phase 2 baseline report
```

---

## 2. Architecture & File Inventory

```
Bitchat/
├── core/                                      # Rust Native Core Engine (bitchat2_core)
│   ├── src/
│   │   ├── identity.rs                        # Ed25519 keypairs, PeerId (32B), ShortId (8B)
│   │   ├── crypto.rs                          # Stateless Noise XX, ChaCha20-Poly1305, 64-pkt Replay Window
│   │   ├── envelope.rs                        # Wire framing codec, Inner bucket padding (64..512B)
│   │   ├── api.rs                             # Public runtime daemon, outbox pump, delivery callbacks
│   │   ├── ffi.rs                             # C-ABI exports for Flutter Dart FFI & Android JNI
│   │   ├── mesh/
│   │   │   ├── policy.rs                      # CoverageAwareFlood, AdaptiveFlood, NeighborTable
│   │   │   ├── router.rs                      # Pure synchronous Router::decide & RouteReason
│   │   │   ├── delivery.rs                    # ApplicationFrame (Message/Ack), InboxDeduplicator
│   │   │   ├── peer.rs                        # Noise XX session state, flight caching, tie-breaking
│   │   │   ├── relay.rs                       # Multi-hop relay engine, TTL=7 enforcement
│   │   │   ├── fragment.rs                    # 16-chunk packet fragmentation & reassembly
│   │   │   └── seen.rs                        # LruSeenCache (exact) & BloomSeenCache (compact)
│   │   ├── storage/
│   │   │   ├── outbox.rs                      # OutboxStore trait, DeliveryState FSM, MessageMode
│   │   │   ├── sqlite.rs                      # ChaCha20-Poly1305 encrypted SQLite store (WAL mode)
│   │   │   ├── memory.rs                      # Volatile heap-only store (Privacy Mode zero-disk)
│   │   │   ├── manager.rs                     # OutboxManager admission control & retry backoff
│   │   │   └── key.rs                         # Hardware DEK KeyProvider trait
│   │   └── gateway/
│   │       ├── bridge.rs                      # Iroh QUIC tunnel manager, mutual auto-approval
│   │       ├── wire.rs                        # Signed RouteSnapshot frames, length-prefixed codec
│   │       ├── routes.rs                      # Monotonic epoch/sequence route table
│   │       ├── identity_binding.rs            # Ed25519 <-> Iroh NodeId cryptographic binding
│   │       └── fairness.rs                    # Token-bucket rate limiter (64 KB/s cap, 25% control reserve)
│   └── tests/
│       ├── test_phase3_mesh_iroh_e2e.rs       # 4-node real Iroh QUIC + BLE mesh integration test
│       └── test_phase4_delivery_e2e.rs        # Full outbox -> relay -> ACK lifecycle integration test
└── app/                                       # Flutter Cross-Platform App & Android Service
    ├── lib/
    │   ├── main.dart                          # Chat UI, Reactive delivery badges, Diagnostics, QR Scanner
    │   ├── mesh_bridge.dart                   # Outbox pump timer, Delivery event stream, MethodChannels
    │   └── core_ffi.dart                      # Dart FFI bindings to libbitchat2_core.so
    └── android/app/src/main/kotlin/com/bitchat/bitchat2/
        ├── MainActivity.kt                    # Permission orchestration & Flutter engine bridge
        ├── BitchatBleService.kt               # Dual-role BLE GATT Server + Scanner Foreground Service
        └── AndroidKeystoreProvider.kt         # Hardware AndroidKeyStore AES-GCM-256 KEK/DEK wrapper
```

---

## 3. Key Implemented Source Code

### 3.1 Coverage-Aware Adaptive Routing (`core/src/mesh/policy.rs`)
Prevents broadcast storms while solving the "Premature Bridge Suppression" bug in bottleneck topologies:

```rust
/// Check whether a set of local neighbors is fully covered by overheard duplicate transmissions.
pub fn is_coverage_complete(
    &self,
    local_neighbors: &[ShortId],
    duplicate_sources: &[ShortId],
    from_peer: Option<ShortId>,
    now: Instant,
) -> bool {
    if local_neighbors.is_empty() {
        return true;
    }

    // Neighbors needing coverage exclude the sender we received this packet from
    let mut uncovered: HashSet<ShortId> = local_neighbors.iter().copied().collect();
    if let Some(p) = from_peer {
        uncovered.remove(&p);
    }

    if uncovered.is_empty() {
        return true;
    }

    // Each duplicate source can cover itself and its freshly reported neighbors
    for src in duplicate_sources {
        uncovered.remove(src);

        if let Some(obs) = self.entries.get(src) {
            // Only fresh observations (within 30s TTL) authorize coverage
            if let Some(rep_at) = obs.reported_at {
                if now.saturating_duration_since(rep_at) <= self.ttl {
                    for peer in &obs.reported_neighbors {
                        uncovered.remove(peer);
                    }
                }
            }
        }

        if uncovered.is_empty() {
            return true;
        }
    }

    false
}

impl RelayPolicy for CoverageAwareFlood {
    fn delay(&self, ctx: &RelayContext) -> Duration {
        if ctx.priority {
            return self.config.priority_delay;
        }
        let upper = self.window_upper_bound(ctx.neighbor_count);
        let upper_ms = upper.as_millis() as u64;
        if upper_ms == 0 {
            Duration::ZERO
        } else {
            let mut rng = rand::thread_rng();
            Duration::from_millis(rng.gen_range(0..=upper_ms))
        }
    }

    fn should_suppress(
        &self,
        ctx: &RelayContext,
        duplicate_sources: &[ShortId],
        from_peer: Option<ShortId>,
    ) -> bool {
        if ctx.priority {
            return false; // Priority SOS messages are never suppressed
        }
        if ctx.duplicates_heard < self.config.suppression_threshold as u32 {
            return false;
        }
        if let Ok(table) = self.table.read() {
            table.is_coverage_complete(&ctx.local_neighbors, duplicate_sources, from_peer, Instant::now())
        } else {
            false
        }
    }
}
```

---

### 3.2 Hardware-Backed Android Keystore DEK Provider (`AndroidKeystoreProvider.kt`)
Generates a 256-bit AES-GCM Key Encryption Key (KEK) in the Android hardware Secure Element/TEE and wraps the 32-byte SQLite Data Encryption Key (DEK):

```kotlin
object AndroidKeystoreProvider {
    private const val KEYSTORE_PROVIDER = "AndroidKeyStore"
    private const val KEK_ALIAS = "bitchat_outbox_kek"
    private const val WRAPPED_DEK_FILE = "bitchat_dek.enc"
    private const val GCM_TAG_LENGTH = 128
    private const val GCM_IV_LENGTH = 12
    private const val DEK_LENGTH = 32

    @Synchronized
    fun getOrCreateDek(context: Context): ByteArray {
        try {
            val keyStore = KeyStore.getInstance(KEYSTORE_PROVIDER).apply { load(null) }
            val kek = getOrCreateKek(keyStore)

            val dekFile = File(context.filesDir, WRAPPED_DEK_FILE)
            return if (dekFile.exists() && dekFile.length() > GCM_IV_LENGTH) {
                unwrapDek(dekFile.readBytes(), kek)
            } else {
                val newDek = ByteArray(DEK_LENGTH).apply { SecureRandom().nextBytes(this) }
                val wrapped = wrapDek(newDek, kek)
                dekFile.writeBytes(wrapped)
                newDek
            }
        } catch (e: Exception) {
            throw KeystoreLockedException("Failed to access Android Keystore DEK: ${e.message}", e)
        }
    }
}
```

---

### 3.3 ChaCha20-Poly1305 Encrypted SQLite Outbox (`core/src/storage/sqlite.rs`)
Encrypts every message record prior to SQLite `INSERT` with bound Associated Authenticated Data (AAD):

```rust
fn init_schema(conn: &Connection) -> Result<(), SendRejection> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS outbox (
            logical_id BLOB PRIMARY KEY,
            sender BLOB NOT NULL,
            recipient BLOB NOT NULL,
            mode INTEGER NOT NULL,
            state INTEGER NOT NULL,
            created_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL,
            attempt_count INTEGER NOT NULL,
            last_attempt_at INTEGER,
            key_version INTEGER NOT NULL,
            nonce BLOB NOT NULL,
            ciphertext BLOB NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_outbox_state ON outbox(state);
        CREATE INDEX IF NOT EXISTS idx_outbox_expires ON outbox(expires_at);",
    )
    .map_err(|e| SendRejection::StorageError(format!("Schema init failed: {e}")))
}

fn build_aad(
    key_version: u32,
    logical_id: &[u8; 16],
    sender: &PeerId,
    recipient: &PeerId,
    mode: MessageMode,
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(100);
    aad.extend_from_slice(b"bitchat2-outbox-v1");
    aad.extend_from_slice(&1u32.to_be_bytes()); // schema version 1
    aad.extend_from_slice(&key_version.to_be_bytes());
    aad.extend_from_slice(logical_id);
    aad.extend_from_slice(&sender.0);
    aad.extend_from_slice(&recipient.0);
    aad.push(mode as u8);
    aad
}
```

---

### 3.4 Pure Synchronous Decision Router (`core/src/mesh/router.rs`)
Evaluates transport candidates according to strict priority: `DirectBle -> IrohGateway -> MeshFlood -> QueueBackoff`:

```rust
pub fn decide(ctx: &RouteContext) -> RouteDecision {
    if ctx.attempt_count >= ctx.max_attempts {
        return RouteDecision {
            candidate: RouteCandidate::QueueBackoff { next_retry_after_secs: 60 },
            reason: RouteReason::ExhaustedAttemptsQueue,
            needs_handshake: false,
            explanation: "Attempt budget reached; backing off in queue",
        };
    }

    // 1. Direct BLE Neighbor
    if ctx.ble_radio_ready && ctx.is_direct_ble_neighbor {
        if !ctx.has_authenticated_session {
            return RouteDecision {
                candidate: RouteCandidate::DirectBle(ctx.target_short_id),
                reason: RouteReason::SessionHandshakeRequired,
                needs_handshake: true,
                explanation: "Direct BLE neighbor reachable; session handshake required",
            };
        }
        return RouteDecision {
            candidate: RouteCandidate::DirectBle(ctx.target_short_id),
            reason: RouteReason::DirectBleNeighbor,
            needs_handshake: false,
            explanation: "Direct BLE neighbor reachable with established session",
        };
    }

    // 2. Approved Iroh Gateway Route (respecting Privacy Mode relay block)
    if let Some(gw_id) = ctx.gateway_route_available {
        if ctx.gateway_opted_in {
            let blocked_by_privacy = ctx.mode == MessageMode::Privacy
                && ctx.privacy_block_relay
                && (!ctx.iroh_endpoint_relay_disabled || ctx.iroh_path != IrohPathObservation::Direct);
            if !blocked_by_privacy {
                return RouteDecision {
                    candidate: RouteCandidate::IrohGateway {
                        gateway_peer_id: gw_id,
                        via_node: Some(ctx.target_short_id),
                    },
                    reason: if ctx.has_authenticated_session {
                        RouteReason::IrohGatewayAvailable
                    } else {
                        RouteReason::SessionHandshakeRequired
                    },
                    needs_handshake: !ctx.has_authenticated_session,
                    explanation: "Routing via approved Iroh gateway",
                };
            }
        }
    }

    // 3. Bounded BLE Mesh Flood (only in Normal Mode; Privacy Mode enforces direct-only)
    if ctx.ble_radio_ready && ctx.mode != MessageMode::Privacy {
        return RouteDecision {
            candidate: RouteCandidate::MeshFlood { max_ttl: 7 },
            reason: if ctx.has_authenticated_session {
                RouteReason::MeshFloodCandidate
            } else {
                RouteReason::SessionHandshakeRequired
            },
            needs_handshake: !ctx.has_authenticated_session,
            explanation: "Disseminating via bounded BLE mesh flood",
        };
    }

    RouteDecision {
        candidate: RouteCandidate::QueueBackoff { next_retry_after_secs: 15 },
        reason: RouteReason::NoRouteAvailable,
        needs_handshake: false,
        explanation: "No eligible transport available; queued in outbox",
    }
}
```

---

### 3.5 Iroh Gateway Mutual Auto-Approval & Self-Route Loop Prevention (`core/src/gateway/bridge.rs`)
Automatically approves incoming QUIC dials once `IdentityBinding` Ed25519 signature verifies, and strips `my_short` and `remote_sid` to prevent self-route reflection loops:

```rust
// Mutual auto-approval upon verified Ed25519 + Iroh EndpointId binding
if let Err(e) = binding.verify(&remote_id) {
    stats_reader.write().await.auth_failures += 1;
    let _ = write_framed_message(&mut send, &GatewayControlMessage::Reject {
        reason: format!("Cryptographic binding verification failed: {e}"),
    }).await;
    return;
}
if !config.read().await.is_approved(&remote_id) {
    config.write().await.approve_gateway(remote_id);
}

// Self-route reflection filter during RouteSnapshot announcement
let remote_sid = crate::identity::PeerId(s.remote_bitchat_pubkey).short_id();
let session_recipients: Vec<_> = recipients
    .iter()
    .filter(|r| r.short_id_hex != remote_sid.to_string())
    .cloned()
    .collect();
```

---

### 3.6 Outbox Exponential Backoff & Instant Peer Reconnection Wakeup (`core/src/api.rs`)
Retries messages up to 12 attempts with capped exponential backoff (`1s, 2s, 4s, 8s, 15s...`), and resets backoff immediately when a peer connects over BLE:

```rust
// Exponential backoff schedule: 1s, 2s, 4s, 8s, 15s (capped)
if let Some(last) = rec.last_attempt_at {
    let shift = rec.attempt_count.saturating_sub(1).min(4);
    let backoff_secs = (1u64 << shift).min(15);
    if now_secs.saturating_sub(last) < backoff_secs {
        continue;
    }
}

// Instant wakeup when BLE neighbor connects
pub fn on_peer_connected(&mut self, short_id: ShortId) {
    self.connected_ble_neighbors.insert(short_id);
    let _ = self.outbox.store().reset_backoff_for_peer(&short_id);
}
```

---

## 4. Automated Test Suite Logs (`122 / 122 Passed`)

### Command Executed:
```powershell
cargo test --manifest-path core/Cargo.toml
```

### Verbatim Output Log:
```text
    Finished `test` profile [unoptimized] target(s) in 0.80s
     Running unittests src\lib.rs (C:\cargo-target\debug\deps\bitchat2_core-e4077169020113c5.exe)

running 120 tests
test api::tests::test_bridge_api_lifecycle ... ok
test crypto::tests::test_debug_redaction_never_prints_secret ... ok
test crypto::tests::test_handshake_frame_encoding_and_validation ... ok
test crypto::tests::test_handshake_rejects_missing_domain_separation_prefix ... ok
test crypto::tests::test_noise_xx_handshake_and_bidirectional_crypto ... ok
test crypto::tests::test_replay_window_updates_only_after_successful_tag_verification ... ok
test crypto::tests::test_separate_directional_keys ... ok
test crypto::tests::test_stateless_dropped_packet_does_not_break_later_packets ... ok
test crypto::tests::test_stateless_out_of_order_within_window_succeeds ... ok
test crypto::tests::test_stateless_packet_older_than_window_rejected ... ok
test crypto::tests::test_stateless_replay_rejected_by_replay_window ... ok
test crypto::tests::test_tampered_ciphertext_fails_decryption ... ok
test envelope::tests::proptest_envelope_encode_decode_roundtrip ... ok
test envelope::tests::test_pad_inner_payload_all_framing_boundaries ... ok
test envelope::tests::test_pad_inner_payload_behavior_above_512 ... ok
test envelope::tests::test_pad_to_smallest_bucket ... ok
test envelope::tests::test_padding_helper_reversible ... ok
test envelope::tests::test_reject_buffer_larger_than_max_envelope ... ok
test envelope::tests::test_reject_hop_count_exceeds_ttl ... ok
test envelope::tests::test_reject_payload_len_mismatch ... ok
test envelope::tests::test_reject_truncated_header ... ok
test envelope::tests::test_reject_ttl_exceeds_max_hops ... ok
test envelope::tests::test_reject_wrong_version ... ok
test envelope::tests::test_relay_increment_hop_count ... ok
test envelope::tests::test_unknown_msg_type_survives_roundtrip ... ok
test gateway::config::tests::test_gateway_authorization ... ok
test gateway::contact::tests::test_contact_roundtrip_and_verification ... ok
test gateway::contact::tests::test_malformed_contact_rejected ... ok
test gateway::contact::tests::test_oversized_contact_rejected ... ok
test gateway::contact::tests::test_tampered_contact_binding_rejected ... ok
test gateway::fairness::tests::test_control_plane_reservation_prevents_bandwidth_starvation ... ok
test gateway::fairness::tests::test_control_plane_reservation_prevents_chat_starvation ... ok
test gateway::fairness::tests::test_global_bandwidth_quota ... ok
test gateway::fairness::tests::test_peer_envelope_rate_limit ... ok
test gateway::identity_binding::tests::test_mismatched_endpoint_key_rejected ... ok
test gateway::identity_binding::tests::test_tampered_signature_rejected ... ok
test gateway::identity_binding::tests::test_unsupported_version_rejected ... ok
test gateway::identity_binding::tests::test_valid_binding_verification ... ok
test gateway::iroh_link::tests::test_iroh_clean_shutdown ... ok
test gateway::iroh_link::tests::test_iroh_isolated_connection_and_payload_exchange ... ok
test gateway::iroh_link::tests::test_iroh_rejects_wrong_alpn ... ok
test gateway::routes::tests::test_empty_snapshot_withdraws_routes ... ok
test gateway::routes::tests::test_route_table_operations ... ok
test gateway::routes::tests::test_snapshot_epoch_and_sequence_monotonicity ... ok
test gateway::routes::tests::test_snapshot_lease_expiration ... ok
test gateway::routes::tests::test_snapshot_reconnect_old_epoch_replay_rejected ... ok
test gateway::routes::tests::test_snapshot_sequence_overflow_rejected ... ok
test gateway::wire::tests::test_concatenated_frames_handled_correctly ... ok
test gateway::wire::tests::test_framed_message_roundtrip ... ok
test gateway::wire::tests::test_malformed_json_body_rejected ... ok
test gateway::wire::tests::test_oversized_length_prefix_rejected ... ok
test gateway::wire::tests::test_read_timeout ... ok
test gateway::wire::tests::test_route_snapshot_bounds_validation ... ok
test gateway::wire::tests::test_route_sync_frame_roundtrip ... ok
test gateway::wire::tests::test_zero_length_frame_rejected ... ok
test identity::tests::test_debug_redaction_never_prints_secret ... ok
test identity::tests::test_file_keystore_roundtrip ... ok
test identity::tests::test_identity_generate_sign_verify ... ok
test identity::tests::test_identity_tampered_message_fails ... ok
test identity::tests::test_memory_keystore_roundtrip ... ok
test identity::tests::test_short_id_derivation ... ok
test mesh::delivery::tests::test_ack_frame_roundtrip ... ok
test mesh::delivery::tests::test_ack_from_wrong_peer_rejected ... ok
test mesh::delivery::tests::test_application_frame_encoding_roundtrip ... ok
test mesh::delivery::tests::test_delivery_retry_exhaustion_and_expiry ... ok
test mesh::delivery::tests::test_delivery_state_lifecycle_and_terminal_guarantees ... ok
test mesh::delivery::tests::test_inbox_deduplicator_exactly_once_display ... ok
test mesh::fragment::tests::proptest_split_reassemble_roundtrip ... ok
test mesh::fragment::tests::test_duplicate_fragments_ignored ... ok
test mesh::fragment::tests::test_empty_frame_roundtrip ... ok
test mesh::fragment::tests::test_malformed_headers_rejected ... ok
test mesh::fragment::tests::test_missing_fragment_timeout_and_freed ... ok
test mesh::fragment::tests::test_multi_fragment_in_order ... ok
test mesh::fragment::tests::test_out_of_order_fragments ... ok
test mesh::fragment::tests::test_overflow_eviction ... ok
test mesh::fragment::tests::test_single_fragment_fast_path ... ok
test mesh::peer::tests::test_handshake_loss_and_flight_caching ... ok
test mesh::peer::tests::test_malformed_handshake_rejected ... ok
test mesh::peer::tests::test_relayed_e2e_encryption_over_line_topology ... ok
test mesh::peer::tests::test_simultaneous_handshake_collision ... ok
test mesh::peer::tests::test_stale_handshake_attempt_rejected ... ok
test mesh::peer::tests::test_wrong_remote_identity_rejected ... ok
test mesh::policy::tests::test_coverage_aware_prevents_partitioned_bridge_suppression ... ok
test mesh::policy::tests::test_coverage_aware_suppression_in_dense_cluster ... ok
test mesh::policy::tests::test_stale_coverage_falls_back_to_forwarding ... ok
test mesh::relay::tests::test_abc_line_topology_one_relay ... ok
test mesh::relay::tests::test_adaptive_flood_delay_scales_with_neighbor_count ... ok
test mesh::relay::tests::test_adaptive_flood_priority_messages_never_suppressed ... ok
test mesh::relay::tests::test_adaptive_flood_suppression_in_dense_cluster ... ok
test mesh::relay::tests::test_ring_topology_terminates ... ok
test mesh::relay::tests::test_ttl_stops_propagation_at_7_hops ... ok
test mesh::relay::tests::test_unicast_shortcut_skips_flood ... ok
test mesh::router::tests::test_router_attempt_exhaustion_queues_backoff ... ok
test mesh::router::tests::test_router_direct_ble_requires_session_handshake ... ok
test mesh::router::tests::test_router_gateway_opt_out_falls_back_to_mesh ... ok
test mesh::router::tests::test_router_iroh_gateway_selected_when_direct_ble_absent ... ok
test mesh::router::tests::test_router_prefers_direct_ble_neighbor ... ok
test mesh::router::tests::test_router_privacy_mode_allows_iroh_when_endpoint_relay_disabled_and_direct ... ok
test mesh::router::tests::test_router_privacy_mode_blocks_iroh_relay_and_falls_back_to_mesh ... ok
test mesh::router::tests::test_router_privacy_mode_blocks_iroh_when_endpoint_relay_enabled_even_if_direct ... ok
test mesh::seen::tests::test_bloom_measured_false_positive_rate ... ok
test mesh::seen::tests::test_bloom_no_false_negatives ... ok
test mesh::seen::tests::test_bloom_rotation_expiry ... ok
test mesh::seen::tests::test_lru_capacity_bound ... ok
test mesh::seen::tests::test_lru_expiry ... ok
test mesh::seen::tests::test_lru_no_false_negatives ... ok
test storage::tests::test_cancellation_and_terminal_immutability ... ok
test storage::tests::test_expiry_pruning ... ok
test storage::tests::test_keystore_locked_rejection ... ok
test storage::tests::test_queue_limits_and_payload_too_large ... ok
test storage::tests::test_restart_and_crash_recovery_from_file ... ok
test storage::tests::test_send_to_normal_mode_durable_commit_and_aead_privacy ... ok
test storage::tests::test_send_to_privacy_mode_zero_disk_guarantee ... ok
test storage::tests::test_tampered_ciphertext_fails_aead_authentication ... ok
test transport::bridge::tests::test_bridge_transport_inbound_and_outbound ... ok
test transport::bridge::tests::test_mesh_node_on_bridge_transport ... ok
test transport::sim::tests::test_link_removal_stops_delivery ... ok
test transport::sim::tests::test_seeded_loss_is_deterministic ... ok
test transport::sim::tests::test_three_nodes_line_topology_isolation ... ok
test transport::sim::tests::test_two_nodes_exchange_frames ... ok

test result: ok. 120 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.93s

     Running tests\test_phase3_mesh_iroh_e2e.rs
running 1 test
test test_phase3_four_node_e2e_mesh_over_real_iroh ... ok
test result: ok. 1 passed; 0 failed; finished in 0.72s

     Running tests\test_phase4_delivery_e2e.rs
running 1 test
test test_phase4_delivery_e2e_lifecycle ... ok
test result: ok. 1 passed; 0 failed; finished in 0.04s
```

---

## 5. Simulation Benchmark Log (300 Monte Carlo Runs)

| Topology | Policy | Avg Transmissions | Delivery % | Mean Latency | P95 Latency | Avg Duplicates | Relays Suppressed |
| :--- | :--- | :---: | :---: | :---: | :---: | :---: | :---: |
| **Line (10 nodes)** | `NaiveFlood` | 6.7 | 71.1% | 9.0 ms | 9.0 ms | 4.7 | 0.0 |
| | `AdaptiveFlood` | 6.1 | 62.8% | 82.1 ms | 157.5 ms | 4.2 | 0.0 |
| | `CoverageAware` | **6.8** | **72.2%** | 98.1 ms | 185.5 ms | 4.8 | 0.0 |
| **Dense Cluster (30 nodes)** | `NaiveFlood` | 30.0 | 100.0% | 10.0 ms | 10.0 ms | 768.3 | 0.0 |
| | `AdaptiveFlood` | 4.4 | 100.0% | 10.6 ms | 15.5 ms | 88.7 | 25.6 |
| | `CoverageAware` | **4.3** | **100.0%** | **10.5 ms** | **14.5 ms** | **86.4** | **25.7** |
| **Random Geometric (50 nodes)** | `NaiveFlood` | 49.0 | 99.3% | 10.0 ms | 10.0 ms | 363.4 | 0.0 |
| | `AdaptiveFlood` | 24.8 | 97.2% | 129.3 ms | 258.5 ms | 135.8 | 22.5 |
| | `CoverageAware` | **28.4** | **99.5%** | 125.2 ms | 244.5 ms | 170.9 | 20.4 |
| **Partitioned Bridge (31 nodes)** | `NaiveFlood` | 27.9 | 89.7% | 10.0 ms | 10.0 ms | 321.9 | 0.0 |
| | `AdaptiveFlood` | 5.3 | 60.2% | 39.0 ms | 72.5 ms | 47.7 | 13.7 |
| | `CoverageAware` | **9.4** | **94.8%** | 130.5 ms | 248.0 ms | **84.7** | **20.0** |

---

## 6. Physical 3-Device Hardware Verification & Bug Resolution Logs

### Physical Devices Under Test
1. **Phone A**: OnePlus Nord CE4 (`CPH2603` / ADB serial `IJGAUOLVSOB6GM8H`)
2. **Phone B**: OnePlus Nord CE 3 Lite (`CPH2467` / ADB serial `d4463804`)
3. **Phone C**: Redmi Note 10 5G / Poco M3 Pro 5G (`M2103K19I` / ADB serial `qcprj7wso7irxgjj`)

### Field Test Results
| Field Scenario | Transport Tested | Outcome | Verification Details |
| :--- | :--- | :---: | :--- |
| **1. Direct BLE Chat** | Pure BLE (Offline) | **PASS** | Bidirectional Noise XX handshake + instant `Delivered` green double-check badge. |
| **2. 3-Phone Multi-Hop Relay** | Pure BLE Line (`A -> B -> C`) | **PASS** | Phone B relayed ciphertext silently; Phone C displayed `1h` hop badge and returned Noise ACK. |
| **3. Offline Outbox Recovery** | BLE Disconnected -> Reconnected | **PASS** | Receiver BT off -> message stayed `InFlight` in encrypted SQLite -> Receiver BT on -> auto-delivered immediately. |
| **4. Iroh QUIC Tunnel** | Wi-Fi / Hotspot P2P QUIC | **PASS** | Single-sided QR scan established mutual QUIC connection (`QUIC Conns: 1`) and delivered messages with BT off. |
| **5. Hybrid BLE + Iroh** | Simultaneous BLE + Iroh | **PASS** | Router prioritized direct BLE neighbor, synced remote routes cleanly without self-route loops. |

### Bugs Discovered & Fixed During Physical Testing
1. **Crash on Startup (`StringIndexOutOfBoundsException: length=15; index=15`)**:
   - *Cause*: `hexStringToByteArray` attempted to parse `"Initializing..."` (15 chars) before FFI `.so` symbols loaded.
   - *Fix (`8ce0b97`)*: Filtered non-hex characters, enforced even length check, and recompiled `libbitchat2_core.so` for `aarch64-linux-android`.
2. **Diagnostics Bottom Sheet Right Overflow (38px)**:
   - *Cause*: Long cryptographic IDs overflowed horizontal `Row` on narrow phone screens.
   - *Fix (`4abae0d`)*: Wrapped title in `Expanded` with ellipsis and made stat cards responsive.
3. **Premature `Failed` Badge After 3 Seconds Offline**:
   - *Cause*: Outbox retried 5 times at 500ms intervals (2.5s total) and marked message `Failed` before user could toggle Bluetooth back on.
   - *Fix (`27debf1`)*: Implemented 12-attempt exponential backoff (`1s, 2s, 4s, 8s, 15s...` up to ~90s) and `reset_backoff_for_peer` instant trigger on `on_peer_connected`.
4. **Iroh `QUIC Conns: 0` After One-Way QR Scan**:
   - *Cause*: Host rejected incoming QUIC handshake because dialing peer was not pre-added to host's whitelist before receiving `Hello`.
   - *Fix (`b74d5d3`)*: Enabled mutual auto-approval upon cryptographic verification of `IdentityBinding` in `Hello` frame, plus 500ms live UI dialog polling.
5. **Self-Identity Appearing in Gateway Peers Bar**:
   - *Cause*: When Phone A and B connected over both BLE and Iroh, Phone A announced Phone B back to Phone B over Iroh.
   - *Fix (`94104bd`)*: Filtered `my_short` and `remote_sid` in `announce_local_peers`, inbound snapshot ingestion, and `gateway_get_routes`.
