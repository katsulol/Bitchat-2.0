# Phase 4 — Revised implementation contract

Status: proposed implementation; replaces the uploaded PHASE_4.md after human acceptance.
Project: Bitchat 2.0. Scope: routing, delivery, bounded outbox, Normal/Privacy policies.
Planning weight: 18%; cumulative effort estimate about 70%, not a verification score.

## 1. Authority, scope, and evidence

Implement against the current working repository, not copied audit excerpts. Preserve the Phase 3 baseline. Do Part A only, then stop for human review. Part B is specified here but is not authorized until that review.
The Phase 3 report claims 75 passing tests and physical successes; the later CLI account qualifies physical multi-hop as pending. Record those discrepancies rather than silently adopting the stronger claim.
The automated Iroh test's deployment is unknown until its source and invocation are inspected. Do not describe it as loopback, four real phones, or cross-network unless evidence establishes that.
The reported 200 simulation runs and benchmark numbers are baseline claims to reproduce from existing artifacts. Record topology definitions, per-topology seed counts, loss model, confidence intervals, and the denominator of delivery reliability.
No specification guarantees defect-free software. Passing gates establishes only the tested scope.

## 2. Non-negotiable invariants

- Rust owns protocol, identity, crypto, message IDs, routing and delivery state. Kotlin transports bytes and platform events; Flutter renders typed events.
- Gateway transport encryption and sender–recipient Noise encryption remain separate. No plaintext fallback for private messages.
- ShortIds and LINK_HELLO are claims until bound to verified full identities. Never manufacture identity from MAC addresses.
- Public broadcasts stay local by default. Reliable delivery and acknowledgments in this phase apply to addressed messages, not broadcast fan-out.
- Local send acceptance, remote gateway acceptance and recipient acknowledgment are different events.
- All queues, caches, streams, route tables, handshakes and reassemblies have byte/count/time bounds.
- No mesh-global lock across network or disk awaits; one owner task controls delivery transitions.
- No durable session-key restoration unless nonce persistence is separately designed and reviewed. Restart establishes fresh sessions.
- Keep existing dependency versions unless required changes are separately documented; use exact-version docs for Iroh/FRB APIs.
- Crypto-touching files receive CRYPTO-REVIEW notes and checklist entries. Crate reputation does not establish protocol security.

## 3. Task 0 — Baseline and compatibility audit

Before coding, record commit, dirty working tree, toolchain, dependencies, existing commands and physical evidence. Do not discard user changes. Create a Phase 4 branch/checkpoint.
Inspect current Transport, envelope, fragmentation, replay window, peer manager, gateway wire protocol, route table, service ownership and FRB interfaces.
Create docs/PHASE4_BASELINE.md, docs/DELIVERY_PROTOCOL.md, docs/PRIVACY_POLICY.md and an updated DECISIONS.md.
Record which bytes are authenticated: origin identity, destination identity, message type, IDs, session context and padding length. Mutable forwarding metadata must remain separate from immutable authenticated message semantics.
Define version/capability handling before wire changes. Reject incompatible peers explicitly; do not silently downgrade to the old handshake or plaintext format.
Inspect actual GATT MTU handling: the CLI's 512-byte fragmentation claim does not establish operation at negotiated MTU 23. Preserve or add lower-layer segmentation/reassembly and test variable MTUs.
Gate: existing tests remain green; unsupported assumptions are listed with concrete source locations to resolve.

## 4. Task 1 — Mesh reliability carry-over

The partitioned-bridge suppression failure is a topology/redundancy problem, not something battery-aware relaying automatically repairs.
Do not ship neighbor-list self-pruning as a proven solution. A reported neighbor relationship does not prove that neighbor received a particular packet, especially under loss or asymmetric links.
First reproduce naive and current adaptive results using identical seeds and traffic. Keep naive flooding as a selectable correctness baseline.
Implement a coverage-aware adaptive variant behind a policy switch:
- Bound and expire neighbor observations; distinguish observed, claimed and verified identities.
- Suppress only under the documented coverage rule when information is fresh and complete.
- Unknown, stale, truncated or inconsistent coverage defaults to forwarding within existing budgets.
- Include beacon transmission/byte costs, asymmetric links and beacon loss in evaluation.
- Unauthenticated neighbor claims must not alone authorize suppression of addressed reliable traffic; use conservative forwarding where verification is absent.
- Prevent synchronized beacon storms with bounded jitter and deterministic test clocks.
Reliable message attempts may use baseline flooding where adaptive behavior is unreliable. Do not declare this candidate fixed solely because the dense-cluster case improves.
Gate: all five topologies, before/after delivery, latency, transmissions, duplicates and beacon costs; report uncertainty and negative results. Select shipping policy from evidence, not a target percentage.

## 5. Task 2 — Session and identity hardening

Replace payload-length handshake detection with a versioned wrapper containing step, attempt identifier and session context. Dispatch by expected state; a step byte alone is insufficient.
Bind negotiated wrapper semantics to the authenticated transcript using supported protocol facilities; document exact bytes and review the change. Do not invent a custom handshake primitive.
Use one in-flight handshake per peer, bounded retry/timeouts and attempt generations. Stale responses cannot reset a newer or established session.
Keep deterministic collision resolution, but define ShortId collision handling and compare full identities once available. Pending claims must not overwrite an authenticated identity association.
Cache/resend appropriate handshake flights without re-running an already consumed Noise state transition. Test loss of each of the three Noise XX messages separately, including the final message.
Do not treat local handshake completion as proof the remote side received the final flight; recover when early application traffic reaches an unready peer.
Reject attempts whose authenticated remote identity differs from the intended recipient/contact. Update session mappings only after verification.
Tests: simultaneous initiation in both orders, loss/duplication/reordering of every flight, stale attempt, malformed step/version, existing-session injection, wrong identity and timeout cleanup.
Crypto tests: replay window updates only after successful tag verification; forged high nonce has no effect; separate directional keys; concurrent sends cannot reuse nonces; exhaustion forces fresh session; restart does not restore unsafe counters.
Gate: reviewer checklist and deterministic fault-injection tests pass; no user-visible tap-send-again path remains after Task 5 integration.

## 6. Task 3 — Authenticated gateway routes

Inspect current Iroh endpoint-to-Bitchat identity binding and manual gateway approval. Signing a snapshot authenticates its author; it does not prove its claimed reachability or make that gateway trustworthy.
Authenticate/authorize the gateway before admitting application traffic. Bind signing key to the authenticated connection; do not accept an arbitrary public key embedded in the snapshot.
Define canonical signed snapshots containing domain, version, gateway identity, connection/session epoch, sequence, bounded lease and recipient identities/ShortIds. Specify the encoding; avoid ambiguous concatenation or unconstrained JSON canonicalization.
Fresh session epochs must be bound to the authenticated connection/handshake. Reject old epochs after reconnect; accept monotonic sequences only within the negotiated epoch. Define sequence overflow/restart behavior.
Replace snapshots atomically per gateway; empty snapshots withdraw routes. Expire via local monotonic leases; disconnect immediately makes routes unusable. Bound signatures, recipients, updates and bytes.
Advertise only reachability supported by the current implementation. Direct BLE peers and verified multi-hop reachability are different. Never re-export Iroh-learned routes in this phase.
Keep alternative routes keyed by full recipient identity plus gateway. Preserve internet-crossing limits and relay-visible bridge provenance through BLE retransmissions.
Tests: tampering, wrong key/binding, malicious but correctly signed false claim, old epoch replay, reconnect, withdrawal, truncation, oversized snapshot, duplicate gateways and bridging loops.
Gate: false claims cannot produce Delivered, bypass E2E verification, or grow state without bounds.

## 7. Task 4 — Delivery protocol and acknowledgments

Separate logical messages from transport attempts:
- Logical message ID is generated once and bound inside authenticated application content.
- Retries retain that logical ID but create a fresh packet/attempt ID and fresh session nonce.
- Mesh deduplication uses packet identity (and fragment index as appropriate); recipient delivery deduplication uses authenticated sender identity plus logical ID.
Reusing the same packet ID forever may cause the mesh seen-cache to suppress legitimate retries. Reusing ciphertext/nonces may trigger replay rejection before a new acknowledgment can be generated.
Define authenticated application frames: version, kind (message/ack), logical ID, sender/recipient binding, body and any authenticated expiry semantics. Parse only after decryption and validate type/identity consistency.
Ack carries logical ID and is authenticated by the intended recipient's session. Never trust a gateway acknowledgment as delivery. No ACK-of-ACK.
Recipient handles a valid duplicate logical message by avoiding a second display and sending a fresh protected ACK. Ciphertext replay rejection remains strict.
Proposed logical states: Queued, InFlight, Delivered, Failed, Expired, Cancelled. Queued includes waiting for route/session/retry. InFlight means attempted and awaiting ACK, not proven receipt. Keep attempt state separately; retry cycles are normal.
Terminal states cannot be downgraded. Define bounded handling of late ACKs: record separately without resurrecting expired/deleted work. ACK races are serialized by the owner task.
Delivered means recipient application accepted the authenticated message; it does not mean read by a human. Normal mode may require durable receipt before ACK; Privacy mode is volatile acceptance.
Exactly-once display is scoped to the retained deduplication records. Normal mode uses atomic receipt/history dedup persistence; Privacy mode cannot promise it across process death or eviction.
Gate: lost message, lost ACK, repeated logical send, corrupted/forged ACK, wrong-peer ACK, reordered ACK, receiver restart, sender restart and packet-cache retry tests.

## 8. Task 5 — Outbox and persistence

send_to validates input then returns either Accepted(logical_id, queued_state) or a typed rejection. Queue-full, invalid recipient, excessive size, locked storage and disk-full must not pretend to enqueue.
Return Normal-mode acceptance only after the required durable commit. Memory-mode acceptance is explicitly volatile. UI may show a pending operation while that commit occurs.
Queue plaintext only inside the protected storage boundary; encrypt for the active transport session at attempt time. Do not persist packets tied to dead session keys as the only recoverable message representation.
Use bounded OutboxStore implementations for SQLite and memory, with explicit schemas/migrations and transaction ownership. No plaintext fallback on database/key errors.
Preferred key design: a random versioned data-encryption key protected by a platform keystore/key-provider abstraction. Do not feed public identity bytes into HKDF; do not casually export the signing private key as storage key material.
Part A uses injected test keys and documents the production key-provider contract. Production persistence remains gated until Part B implements secure platform key storage; no hardcoded or plaintext production key file.
Use an established AEAD implementation with reviewed nonce generation and limits. Bind record type, schema, identity, logical ID, mode and key version as associated data as appropriate. Document metadata left visible in SQLite indexes, WAL and backups.
Never insert plaintext content into SQLite and encrypt it afterward. Deletion is logical deletion, not guaranteed physical erasure of flash/WAL/backups.
Per-message policy and expiry are stored with the accepted record. Normal default TTL 24 h; Privacy 15 min; configurable bounded limits. Use injected monotonic time while running and documented conservative wall-clock handling across restarts; clock rollback cannot extend TTL indefinitely.
Retry on relevant events plus capped exponential backoff/jitter; coalesce wakeups and serialize attempts. Bound attempts, pending bytes and per-recipient concurrency; avoid event-driven retry storms.
Cancellation removes pending work but cannot recall bytes already transmitted. Crash recovery treats uncertain transmissions as retryable with the same logical ID.
Gate: crash before/after acceptance, restart, disk-full, failed key access, tampered record, queue limits, expiry/time changes, cancellation and lost ACK tests.

## 9. Task 6 — Router and transport policy

Use a pure decision function with typed reason codes plus separately rendered explanations. Decision tests assert codes, not fragile prose.
Inputs: intended full identity, per-message policy, fresh routes, link readiness, observed Iroh paths, gateway opt-in, network hints, queue/attempt state and authenticated session status.
Separate control-plane routing from application readiness: lack of a session triggers a routed handshake rather than blocking the route needed to establish that session.
Normal preference: verified direct neighbor; known mesh path if actually implemented; available approved Iroh route; bounded mesh flood; queue. Do not invent a unicast path table when the existing mesh only floods.
After exhausting bounded candidates, queue/back off. Failure of one route is not immediately permanent failure of the logical message.
Distinguish sender-online direct-to-recipient Iroh from gateway-to-gateway bridging. Implement only supported endpoint reachability; list unsupported direct-to-recipient capability explicitly rather than claiming it.
Store actual transport observations separately from planned route. Multi-leg routes can include BLE and Iroh together. Hop convention in current UI is intermediate relays: 0h direct, 1h one relay; preserve and document it.
Iroh path observation supports direct, relay, mixed/changing and unknown. Read pinned-release APIs; do not infer path from whether a relay URL exists.
Privacy relay restriction must be enforced by transport configuration, not just a router preference. A path can migrate after connection establishment. Verify pinned Iroh support, including relay use during discovery/setup.
If a no-relay policy cannot be enforced to its advertised scope, block that Iroh candidate and explain why. Do not label a normal connection relay-disabled based on a one-time check. A dedicated endpoint may be necessary and must be evaluated before implementation.
DeviceState includes unknown values and network hints, not proof of internet reachability. Connectivity probes/actual connection state establish route usability. Battery-based throttling stays in Phase 5.
Gate: table-driven branch tests, session bootstrap, fallback bounds, unknown path, path migration, network flapping and policy enforcement.

## 10. Task 7 — Modes, padding and gateway fairness

Normal/Privacy are policy bundles; they do not establish stronger peer trust. Explain direct-IP exposure versus relay metadata exposure without claiming one path is universally safer.
Set policy at send acceptance, including overrides. Changing the default affects future messages; it must not silently persist existing Privacy messages or delete old Normal messages.
For pending messages requiring changed policy, use an explicit cancel/requeue action with documented logical identity handling. In-flight transmissions cannot be recalled.
Privacy scope: no intentional persistence of Privacy message bodies, outbox records or delivery history. Existing Normal records and long-term identity may remain. OS artifacts, crash dumps and past writes are outside a blanket no-disk guarantee.
Audit logger, analytics, UI state restoration, notifications and backups for content leakage. Test application-controlled writes with instrumented stores; never promise the whole OS writes nothing.
Pad authenticated application plaintext before encryption to documented bounded buckets, then encrypt, then fragment. Validate inner lengths after authentication. Do not pad ciphertext after encryption and call that authenticated padding.
Specify which chat/control types are padded and the effective maximum. Handshakes, BLE fragments and transport ACKs need not all be bucket-sized. Verify logical protected-record sizes and separately report actual BLE/Iroh wire overhead and remaining size/timing leakage.
Use bounded send jitter once per scheduled attempt; unrelated wakeups cannot endlessly reschedule delivery. Deterministic RNG/clock in tests, production randomness from an appropriate source.
Gateways are off by default. Toggle governs forwarding for others, separately from receiving one's own messages. On disable withdraw routes, stop new admissions and resolve bounded in-flight work explicitly.
Apply global connection/byte quotas and per-authenticated-gateway/peer quotas. Claimed ShortIds alone cannot secure fairness against identity spoofing. Account for fragments/retries/control traffic; reserve bounded control capacity to prevent starvation.
Gate: mode-switch matrix, per-message overrides, padding tamper/boundaries, storage-write audit, rate-limit evasion, toggle races and queue fairness tests.

## 11. Part A integration/evaluation gate — STOP HERE

Run fmt, Clippy with warnings denied, unit/property/simulator tests and affected integration tests after each logical commit. Derive exact commands from workspace configuration; record commands and outputs. Simulated timers must use virtual time; real-network tests use bounded deadlines and are classified separately.
Evaluate offline-to-online delivery, gateway absent/flapping, lost ACKs, cross-session retries, queue pressure, Normal/Privacy overhead and topology suppression. Include route/reason histograms and resource peaks.
Extend fuzz/property coverage for all new wire/storage decoders, excessive lengths, fragmentation and arbitrary malformed input. Never claim review based on test count alone.
Part A acceptance: reproducible evaluation; baseline BLE behavior preserved in tests; logical/packet IDs separated; authenticated ACKs; bounded retry; secure-storage contract; supported privacy policy enforcement; documentation and crypto-review list complete.
Mark production Android key integration and all unperformed hardware cases pending. Report known limitations and unresolved security review items. Commit and STOP for human review before Part B.

## 12. Part B — Platform integration after approval

Implement the production key provider, SQLite lifecycle, explicit secure-storage failure handling and backup policy. Demonstrate keystore availability from the service-owned runtime; do not assume Flutter UI lifecycle owns background keys.
Integrate owner-task events into Flutter. Avoid overlapping polling tasks; retain/cancel subscriptions. Show queued, awaiting ACK, delivered, failed, expired/cancelled and actionable rejection reasons.
Render path as observed multi-leg information or unknown; do not equate gateway acceptance with delivery. Show modes, gateway opt-in and a bounded diagnostics panel.
Native service restart/activity recreation must not duplicate runtimes or outbox pumps. Screen-off and process-kill behavior require physical evidence. Do not guarantee delivery while the OS has stopped execution.
Debug topology filters must cover every ingress/egress path and existing links, use stable test mappings and log dropped direct paths. Disable in release builds. Advertising power changes do not guarantee data-connection range isolation.
Add tools/headless-gateway only using repository-supported APIs. An Iroh-only laptop can be a remote endpoint/gateway test harness; it cannot deliver to a BLE-only recipient without a functioning BLE adapter and implementation.
Full four-role test requires four phones OR three phones plus a laptop that actually supports the destination BLE link. Label an Iroh-only laptop test as partial/hybrid, not full offline-endpoint delivery.

## 13. Physical test matrix

- Three-phone software-enforced line: A-C blocked in both directions, B relays; expect 1h with correlated logs. Disable B and verify A-C delivery stops. Repeat reverse direction.
- Physical-separation line: record link discovery and B-removal control; distance alone is not evidence of isolation.
- Cross-network Iroh: mobile data versus separate WiFi; record actual path over time. Test relay-assisted and direct paths where achievable, with supported configuration/evidence.
- Full offline-endpoint path: A BLE-only, B online, C online, D BLE-only; fresh Noise handshake, reply and no broadcast export. Record roles, adapters, internet state and traces.
- Receiver unavailable: accept queue, restore receiver, verify ACK lifecycle and dedup within documented retention. Repeat sender/receiver restart and ACK loss.
- Variable MTU, Unicode, large records, lost fragments, WiFi/cellular switch, Bluetooth toggle, service restart and queue overflow.
- Mode checks: no application persistence of Privacy content, no silent policy downgrade, documented padding at logical-record layer and wire overhead.
- Battery: repeated matched gateway-on/off runs with controlled traffic, screen state, radio conditions and initial charge; report coarse percentage and duration. Use energy/charge telemetry if available. A single 30-minute percent reading is exploratory, not a precise power claim.

## 14. Final report and out-of-scope

Report commit, exact commands, tests executed, benchmark artifacts, crypto files needing review, changed wire/storage versions, physical evidence, deviations and remaining risks. Update PROGRESS.md per test: not run, failed, simulated, desktop/network or physical—not one blanket PASS.
Out of scope: Web-of-Trust scoring, battery-based relay selection, fuzzy discovery, cover traffic, Merkle sync, iOS and claims of production security. Gateway authenticity/authorization and basic resource controls cannot be deferred to Web-of-Trust.
