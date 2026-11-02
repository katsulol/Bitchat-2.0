# Phase 5 — Trust, Resource-Aware Relaying, Coarse Proximity and Volatile Chat

Status: proposed implementation specification. Read fully before coding.
Baseline: uploaded PHASE_4_FINAL_REPORT.md and COMPLETE_WORK_LOG_CODES_AND_TESTS.md; inspect actual repository HEAD before applying changes.
Planning allocation: original Phase 5 share 15%, cumulative estimate 85%. Percentages measure planned effort, not security or hardware verification.
Execution: Gate 0 repairs and evidence reconciliation → Part A Rust implementation → HARD STOP for human review → Part B Android/Flutter and physical tests after approval.

## 1. Current stage and evidence boundaries

The report lists commit 94104bd as its latest commit, 120 library tests plus two integration tests, Android key wrapping, SQLite outbox, public runtime integration, and physical tests on two OnePlus phones and a Redmi/Poco device. These are reported results, not independently executed checks.
The two uploaded reports substantially duplicate material; they are not two independent validations.
Reported hardware cases: direct BLE; three-phone line with 1h badge; outbox reconnection; WiFi/hotspot Iroh with Bluetooth off; simultaneous BLE/Iroh with BLE preferred.
The reported hybrid case does NOT establish two offline endpoints bridged through two online gateways. Preserve that distinction. A controlled three-phone line also needs direct-link exclusion and relay-removal controls.
Do not claim iOS support, cross-carrier hole punching, relay-free paths or background reliability from these cases.
Baseline reports contain discrepancies: replay window listed as 64 packets versus earlier 128; newest benchmark table returns to 94.8% versus an earlier 97.3%; 300 runs are claimed but only four topologies are shown. Resolve from source and raw artifacts.
Do not treat the snippets as the complete source. Confirm every proposed repair against current files; retain fixes already correctly implemented.

## 2. Goals and non-goals

Deliver:
1. Explicit peer/gateway authorization and small, bounded, local vouch-based onboarding.
2. Device-resource budgets that shift optional forwarding load without changing E2E security.
3. Coarse, local-only proximity presentation with honest uncertainty.
4. Best-effort volatile chat semantics across sender, recipient, UI and application storage.
5. Controlled evaluation of admission abuse, delivery/resource trade-offs and regression behavior.

Do not add cover traffic, Merkle synchronization, physical store-carry-forward, voice/video, large attachments, ML routing, identity tokens, global reputation, public contact graphs or iOS in this phase.
Do not invent cryptographic primitives. Preserve sender–recipient Noise sessions, protected ACKs, retry logical/packet ID separation and bounded forwarding.

## 3. Gate 0 — Repair regressions before new features

### 0.1 Gateway auto-approval regression — mandatory

bridge.rs excerpt approves any incoming gateway after IdentityBinding verifies. Binding proves control of keys; it does not authorize use of this device as a gateway.
Remove unconditional approval. Model Unrecognized, IdentityVerifiedPendingConsent, Approved, Revoked and Blocked separately. Identity verification alone cannot populate the approved set or expose route directories.
Preserve easy one-sided QR onboarding with a bounded invitation workflow:
- User explicitly opens gateway pairing; generate a high-entropy, single-use invitation capability using the existing secure RNG.
- Contact includes pinned host identity, protocol version, invitation and expiry. Invitation is secret: never log or persist it unnecessarily.
- Dialler verifies host identity before presenting invitation; host verifies dialler identity binding.
- Require host confirmation by default; optional explicit invitation-based auto-admission must be visibly enabled, short-lived, limited-use and gateway opt-in dependent.
- Consume invitation atomically after successful approved onboarding; cap outstanding invitations and pending sessions. Use monotonic expiry; process restart invalidates unpersisted invitations.
- Reject replays and wrong context; no bearer capability forwarded through route snapshots or BLE advertisements.
- Existing explicitly approved contacts reconnect without invitations; revocation overrides old invitations and vouchers.
Tests: valid unknown binding remains pending; invalid identity; invitation theft risk documented; expired/reused invitation; concurrent redemption; blocked contact; approval cancellation; gateway disabled mid-pairing.

### 0.2 Coverage is inferred, not proven

policy.rs removes a neighbor from uncovered when a duplicate sender merely lists it in reported_neighbors. This is inferred coverage; it is not evidence that that neighbor received this packet. Freshness and identity verification do not prove receipt.
Rename/document inferred versus packet-specific observed coverage. For reliable addressed traffic, do not suppress solely because of a third-party neighbor list. Use existing direct evidence or conservative forwarding; do not add global simulator knowledge.
Unknown/incomplete neighbor sets must not automatically prove complete coverage. Inspect the empty-list true branch and define explicit completeness/confidence.
Inject time and seeded randomness: excerpt calls Instant::now() and thread_rng(), undermining deterministic simulations. Production clock/RNG adapters may use real sources; evaluation must not.
Priority traffic bypasses suppression only within existing quotas and budgets; an unauthenticated priority flag must not grant unbounded forwarding.
Tests: stale/false neighbor list, asymmetric links, lost duplicate, incomplete discovery, forged priority, bridge cut and matched-seed reproducibility.
Re-evaluate instead of promising preservation of old percentage gains.

### 0.3 DEK lifecycle and storage integrity

Keystore excerpt regenerates DEK when wrapped file is absent/short and writes directly to the destination. That can lose access to an existing database after corruption or partial writes.
Implement versioned bounded wrapped-key records, checked lengths and atomic durable writes using appropriate Android facilities. Distinguish first use from missing/corrupt wrapping state with an existing database.
Never silently replace a missing/invalid KEK or DEK while an encrypted database exists. Fail closed with an actionable locked/unavailable state; explicit user-authorized destructive reset is separate.
Inspect actual hardware security level where available; report hardware-backed/software-backed/unknown accurately. AndroidKeyStore selection alone does not prove secure hardware storage.
Test truncated/oversized wrapped file, crash during write, deleted KEK, unwrap failure, corrupted database, cold restart, backup/restore mismatch and concurrent initialization.
Outbox excerpt authenticates identity, logical ID, mode and version but not expiry/state/attempt columns. Document the integrity model. Protect immutable expiry semantics and security-sensitive mutable fields by an authenticated record/transaction design, or explicitly narrow guarantees. Never claim full database rollback protection without external trusted state.
Crypto/key changes require human review notes. Keep plaintext DEK exposure limited across JNI/FFI; no debug logging, minimize copies and use best-effort clearing.

### 0.4 Strict identity parsing

Report fixes initialization by filtering non-hex characters. Identity parsers must reject malformed input, not transform it into another valid identity.
Use typed NotReady/Ready(public_identity) initialization events; UI placeholder text must never enter protocol parsing.
Permit only explicitly documented separators in user contact input. Require exact decoded length, checked ASCII hex and full identity/ShortId type distinction. Tests include Initializing..., Unicode, odd length, excessive length, inserted characters and valid upper/lowercase.

### 0.5 Retry and mode semantics

Inspect 12-attempt policy and router max-attempt branch together. Define whether attempts reset per retry round or become terminal; avoid permanent QueueBackoff loops and short retry budgets masquerading as a 24-hour outbox.
No route means Queued with reason, not repeated transmission attempts. Increment attempt count on an actual defined send attempt; handshakes use a separately bounded budget. ACK wait has a deadline.
On exhaustion, either explicit Failed or a documented bounded retry-round schedule until TTL; choose one in ADR and UI. Never silently extend TTL on reconnect.
Peer reconnect wakeups must use verified identity association, coalesce events and preserve hard limits. Do not let repeated claimed ShortIds reset limits forever.
Unify retry ownership in the Rust service runtime; inspect Dart outbox timer to prevent two independent retry pumps. Verify activity destruction does not terminate a claimed service-owned delivery loop.
Document current Privacy behavior: supplied router blocks all BLE mesh flooding and requires direct Iroh plus relay-disabled capability. This differs from earlier BLE fallback descriptions. Make exact policy explicit; do not silently relax it in Phase 5.
Verify relay restrictions at endpoint runtime, not just booleans passed to the router. Distinguish user selecting an outbound gateway from consenting to forward others' traffic.

### 0.6 Baseline artifacts

Record actual HEAD, clean/dirty state, toolchain, dependencies, schema/wire versions, app/native build hashes and executed test commands. Preserve user changes.
Confirm 64-versus-128 replay window and 16-fragment size limits from source; test negotiated MTU 23 plus larger MTUs without assuming a 512-byte link.
Reproduce all five intended topologies with seed lists, 3 policies and stated run count. Explicitly define delivery denominator, reachable-node denominator under hop cap, latency units and simulated link delays.
A 10-node line may exceed a seven-hop budget; distinguish unreachable-by-policy destinations from packet loss. Explain changes in Naive baseline between reports rather than attributing them to the new policy.
Store machine-readable raw runs and config beside tables, count control traffic and report uncertainty. Do not force a performance threshold by selecting seeds.
Gate 0 exit: repairs tested and committed; write PHASE5_BASELINE.md and PHASE5_GATE0_REVIEW.md. Stop only for an unresolved security decision; otherwise proceed through authorized Part A.

## 4. Architectural contracts

Suggested new modules, adapt to current tree without duplicate implementations:
- trust/model.rs, voucher.rs, store.rs, policy.rs
- mesh/resource.rs and mesh/proximity.rs
- storage/volatile.rs or existing memory store extensions
- tests/test_phase5_policy_e2e.rs
Rust owns authorization, voucher verification, resource decisions, proximity aggregation and volatile policy. Android supplies platform facts; Flutter displays typed state and explicit consent.
Use full verified PeerId for authorization; ShortId only for compact routing. Keep observed link identity separate from original message author.
One owner task serializes policy changes, route removal, session cancellation and queued work. No global lock across disk or network awaits.
Bound all inputs before allocations/signature verification where possible. All new wire formats versioned, canonical, domain-separated and negotiated; unknown mandatory semantics must fail explicitly.
No trust label, budget or proximity observation changes the verified conversation peer or bypasses E2E checks.

## 5. Part A Task 1 — Local trust and authorization

Use capabilities rather than one universal numeric score:
- Receive direct messages / allow message requests.
- Act as an approved introducer.
- Use this gateway for forwarding.
- Permit this gateway as an outbound next hop.
- Issue an introduction for another identity.
Define local Blocked as overriding every other decision. IdentityVerified alone means key ownership, not personal trust.
Direct user approval is explicit. Vouch-based introduction grants only the scoped capability permitted by local policy; never automatic permission for gateway use or introducer delegation.
Default new users can establish bounded identity setup and send a bounded message request; they do not need a voucher merely to exist. Keep separate global pending-request limits to limit identity-churn attacks.
Local block is not a global accusation. Do not share block lists by default. Authentication failures/temporary connectivity failures are diagnostics, not automatic reputational punishment.
Provide pure authorize(subject, capability, context) -> Allow/Deny/Pending with typed reason codes and expiry. UI prose is separate.
Tests: blocked overrides, no privilege escalation through multiple identities, expiry, unknown identity, local consent revoked while pending and trust changes during queued sends.

## 6. Part A Task 2 — Bounded vouch protocol

Scope: one-hop endorsements from locally approved introducers. No recursive global Web-of-Trust traversal in this phase.
Canonical signed body: domain bitchat2/vouch/v1, version, issuer full key, subject full key, intended verifier/audience, capability, random credential ID, issued time, expiry and revocation reference/version as designed.
Audience-bind private introductions to prevent replay into unrelated communities. Do not broadcast vouchers or the social graph. Exchange through authenticated encrypted sessions only and require explicit user action.
Use existing Ed25519 implementation; signature covers all fields and fixed encoding. Specify maximum lengths and allowed enums before decoding.
Default proposal: at most 64 locally approved introducers, 1024 active credentials, 16 KiB total import request and 10 introductions per issuer/hour; make limits configurable and justify from tests. Apply global limits as well as per-issuer limits.
Accept only vouchers signed by locally approved introducers for the relevant capability. Repetition, duplicate issuers, circular endorsements and self-vouching do not multiply trust.
Expire credentials with bounded lifetime and documented wall-clock uncertainty; local cached leases use monotonic time. Clock rollback must not make an already expired voucher live again. Refuse automatic elevation when clock validity is unknown; manual approval remains possible.
Revocation: signed issuer revocation bound to credential ID and audience; local removal is immediate. Cache revocations for at least the possible credential validity window, with bounds. Offline recipients may not learn revocation until reconnect: document this limitation.
Never punish an issuer automatically because an introduced peer later misbehaves. Attribution and collusion are not solved by signatures.
Store consent/credentials encrypted under the existing protected storage boundary; distinguish durable contacts from volatile chat bodies. Export only explicitly requested credentials.
Tests: mutation of every field, wrong audience, unknown issuer, issuer block, expired/future credential, duplicate import, revocation-before-voucher, old voucher replay, issuer removal, quota exhaustion and cyclic graph inputs.
Claim: local introduction-based admission and bounded abuse resistance, not proof of unique humans or a complete Sybil defense.

## 7. Part A Task 3 — Gateway authorization integration

Apply Task 1 capability checks at incoming connection admission, snapshot acceptance, forwarding admission and outbound route selection. Cryptographic connection authentication remains mandatory independently.
Route claims are not proof of reachability; vouchers do not make them proof. Preserve recipient E2E identity verification and no false Delivered from gateways.
On revocation/block: atomically withdraw routes, stop new forwarding admissions, close relevant connections as appropriate and invalidate pending work for those routes. Re-route local messages within unchanged TTL; do not mark them Delivered or erase them silently.
Cached invitations or route snapshots cannot undo block state. Persistent explicitly approved contacts migrate as approved; legacy auto-approved contacts migrate to pending review unless provenance establishes real consent.
Do not auto-share the recipient directory before authorization. Retain bounded local-only broadcasts and bridge provenance/loop rules.
Integration tests: unsolicited valid gateway, approved reconnection, lost approval mid-forward, two gateways one blocked, old snapshot after block and invitation races.

## 8. Part A Task 4 — Resource-budgeted relaying

Introduce ResourceSnapshot: charging, battery band, power-save state, thermal state, network metering, BLE readiness, foreground/background facts, timestamp and unknown values.
Platform values are hints; local user opt-in and limits are authoritative. Do not broadcast exact battery percent, device model or detailed power state. Remote capacity advertisements, if necessary, are coarse, authenticated, expiring claims with no security authority.
Keep mode privacy constraints and gateway opt-in separate from resource class. Being on WiFi does not imply charging, free bandwidth or permission to relay.
Profiles: Minimal, Balanced, Assisted and Paused. Proposed test configuration may set relative byte budgets 1:4:8; these are experimental policy parameters, not measured energy savings. Thermal restriction and user pause override charging.
Use bounded byte token buckets, bounded pending queues and per-peer fairness; reserve both queue and bandwidth for ACK/handshake/control without an unlimited bypass. Integrate existing fairness.rs rather than layering conflicting quotas.
Budget actual encoded bytes including fragments/retries/control. Distinguish originator traffic and transit traffic; never quietly drop local accepted messages to fund forwarding.
Lower-resource devices reduce optional forwarding load, but a critical bridge may still be necessary. No policy can guarantee delivery through a device that refuses forwarding. Measure the trade-off and queue/fallback appropriately.
Adjust relay delay conservatively with resource availability; retain coverage safeguards and hop/attempt limits. Prevent starvation with bounded aging; no untrusted priority field bypass.
Add hysteresis/minimum dwell and event coalescing to avoid oscillation near thresholds; inject clock/RNG. Lifetime resource exposure remains bounded under repeated reconnects.
Tests: unknown battery, charging but metered, low battery bridge, thermal critical, profile oscillation, forged advertised capacity, ACK starvation, fragment bursts, user disable and queue fairness.

## 9. Part A Task 5 — Coarse proximity, privacy by minimization

Implement a local display feature, not ranging or identity protection. Categories: Nearby, Further, Unknown; no meters or guarantees of physical distance. BLE RSSI is noisy and manipulable.
Use bounded per-link recent RSSI samples, robust smoothing, minimum sample count, stale expiry and hysteresis. Thresholds are configuration requiring physical calibration; do not advertise universal distance thresholds.
Use actual direct link observations only; do not display the remote sender as nearby merely because a relay is nearby. Associate categories with verified peers only after link binding; unverified devices remain generic pending observations.
No RSSI history persistence, no export over Iroh, no GPS collection, no signal fingerprint database. On disconnect/expiry show Unknown and discard samples.
Stable identity advertisements and connection behavior can still permit tracking. Coarsening the UI does not solve that. Rotating identifiers require a separate protocol design and are out of scope here.
Do not use proximity as authentication or to grant gateway/voucher privileges. Avoid routing decisions based solely on signal category.
Tests: outliers, stale samples, sparse observations, two links per peer, relay-origin confusion, rapid category changes and sample-memory bounds.

## 10. Part A Task 6 — Best-effort volatile conversations

Build on existing memory-only outbox instead of implementing a second queue. Define volatile session scope, retention timer, close behavior, background/lock behavior and crash behavior in VOLATILE_CHAT.md.
Privacy transport policy and volatile retention are distinct concepts. A retention choice must not silently relax relay restrictions. Defaults should match existing UI and be documented.
Negotiate recipient support using authenticated capability/policy frames before sending volatile-required content. If unsupported, reject or queue pending user action; never silently downgrade to durable chat.
Sender and receiver intentionally avoid persistent bodies, attachment previews, chat history, dedup receipts tied to bodies, logs, notification text, search indexes, analytics and restoration caches for these conversations.
ACK means volatile application acceptance, not durable retention. Duplicate suppression is bounded by memory retention; after restart no exactly-once guarantee. Dropping a volatile conversation cancels pending sends but cannot recall transmitted messages.
Long-term identity, explicitly approved contacts and vouchers may remain durable; disclose this distinction. Clear sensitive buffers where practical; Rust zeroization cannot guarantee clearing copies in managed runtimes or OS memory.
Recipient may use another client, take a screenshot or retain content. Expiry is local policy, not remote deletion proof. Android screenshot/task-preview mitigations are optional, tested UI measures, not a security guarantee.
Default proposal: 15-minute maximum pending lifetime inherited from Phase 4, explicit shorter conversation retention options, strict message/count/byte caps. Negotiate any receiver retention requirement and bound it locally.
Tests: unsupported peer, downgrade attempt, replayed capability, Normal-to-volatile switch, restart, timeout, duplicate delivery, pending-send cancellation and instrumented application storage writes on both ends.

## 11. Part A Task 7 — Integration and evaluation

Use public runtime APIs, not only hand-wired modules, for:
- Pending user request → explicit approval → fresh Noise handshake → queued send → lost ACK → retry → one display → Delivered.
- Introducer voucher → scoped admission → issuer revocation → policy change without identity/session confusion.
- Approved gateway → resource pressure → bounded queue → alternative route or backoff → eventual ACK.
- Volatile-required sender/receiver → no application content persistence → expiry/cancellation.
Use injected clock/RNG and bounded iteration budgets; matured timers and future-time advancement must remain correct. Network tests have bounded deadlines and classification separate from simulation.
Property/fuzz tests target voucher, invitation, policy negotiation and storage decoders, oversized lengths, enum values, fragments and replay states.
Evaluation variants: baseline Naive; old Adaptive; corrected CoverageAware; CoverageAware plus resource policy. Trust admission attacks are a separate workload, not mixed into network-reliability comparisons without explanation.
Matched seed/config runs across dense, line, geometric, bridge and intermittent topologies; include heterogeneous profiles and low-resource bridge cases. Report delivery-before-expiry, median/P95 latency, all bytes per delivered logical message, duplicates, queue pressure and per-node load.
Include discovery/voucher/control overhead. CPU/radio-byte proxies are not battery measurements. Do not hide negative results or compare different hop limits as if only policy changed.
Trust experiments: identity churn, voucher flood, approved malicious issuer, replay and false routes. Measure bounded memory/verification work and legitimate admission latency; no claim of universal attack prevention.
Proximity is evaluated for stability and uncertainty, not exact-distance accuracy.

## 12. Part A gate — mandatory stop

Deliver source commits and docs: PHASE5_BASELINE.md, PHASE5_GATE0_REVIEW.md, TRUST_PROTOCOL.md, RESOURCE_POLICY.md, PROXIMITY_LIMITATIONS.md, VOLATILE_CHAT.md, PHASE5_EVALUATION.md and PART_A_ACCEPTANCE_REVIEW.md.
Update existing DECISIONS.md, CRYPTO_REVIEW.md, PRIVACY_POLICY.md and PROGRESS.md without erasing historical failures.
Run exact workspace fmt, Clippy, tests, affected Android compile checks and decoder fuzz/property suites available in the repository. Record commands/results, not only total test count.
Acceptance table: implemented/tested; implemented/not tested; deferred to Part B; missing. Include source locations and exact test names. List security-sensitive changes for human review.
STOP before Part B even if all checks pass. A milestone report must not automatically authorize app/hardware work.

## 13. Part B Task 8 — Android and Flutter after approval

Audit current JNI/C ABI contracts; use existing core_ffi.dart integration rather than assuming FRB generated bindings. Never invent exports. Validate ownership, lengths, thread safety, errors and library ABI/build version.
Add typed UI: pending/verified/approved/introduced/blocked distinctions; gateway consent; pair invitation expiry; approval revoke; resource profile; coarse proximity with explanation; volatile retention and unsupported-peer warning.
Do not show introduced contacts as independently verified people. Explain issuer and granted capability without exposing a full social graph.
Implement native resource callbacks in the service; no unnecessary high-rate battery/RSSI polling. Use platform API availability checks and Unknown fallback. Avoid new broad permissions unless required and documented.
Single Rust runtime owns retry/policy timers. Flutter subscribes to state; activity recreation cannot duplicate jobs. Record process death and OS execution limits honestly.
Strict identity initialization replaces filtering. Add responsive narrow-screen and large-font tests. No sensitive notification previews for volatile chat; test task-switch previews and lifecycle settings.
Verify actual key security level and atomic wrapped-key handling on every available phone. Exclude applicable sensitive files from backup using supported Android-version mechanisms; test configured behavior.
Produce reproducible native builds and record library hashes in build diagnostics so stale .so files cannot masquerade as current Rust logic.

## 14. Part B Task 9 — Physical matrix

Use actual available phones: OnePlus Nord CE4, OnePlus Nord CE 3 Lite, Redmi Note 10 5G/Poco M3 Pro 5G as reported. Do not publish serial identifiers in public reports.
1. All pairs, both initiation directions: fresh authenticated BLE chat and reconnect.
2. Enforced three-phone line: block direct endpoint link ingress/egress, verify 1h, remove relay and verify delivery stops/queues. Label software-enforced topology; separately test physical separation if possible.
3. One-sided QR: host explicitly pairs, incoming peer pending/approved, stale invitation rejected, blocked peer cannot reconnect through a new voucher.
4. Two real networks: record direct/relay/unknown Iroh path and policy enforcement. WiFi/hotspot test is separate from cross-network NAT testing.
5. Full offline endpoints through two online gateways: fourth phone or a laptop with working BLE support required. Without that, record pending; Iroh-only laptop is a partial hybrid test.
6. Resource profiles: bridge on battery/charging/power-save; measure delivery and per-node work. Repeat matched-duration runs with controlled traffic/screen/network and available charge/energy counters; report uncertainty.
7. Proximity: stationary, moving, wall obstruction, phone orientation and stale disconnect. Verify categories do not claim meters or influence authorization.
8. Volatile chat: both apps, background/lock/restart, notification/task preview, app-controlled storage inspection, receiver without capability and cancellation while queued.
9. OS/transport: variable MTU, Bluetooth toggle, WiFi/cellular transition, activity recreation, service stop, keystore unavailable and disk-full rejection.
Only human-run results receive physical PASS; include timestamps, app/native commit, steps and evidence. Simulated device state is not measured power behavior.

## 15. Scope and final gate

This phase is complete only when promised core behavior and corresponding platform cases are either tested or explicitly excluded from release claims. Do not collapse pending full hybrid tests into a blanket Phase 5 PASS.
Final report: commits, source map, migration steps, exact checks, reproducible metrics, before/after defaults, security review list, physical evidence and unresolved limitations.
Research contribution wording: evaluated resource-aware forwarding and local introduction-based admission layered over encrypted BLE/Iroh delivery. No first-ever or universal superiority claim without a separate literature review.
Phase 6 remains cover traffic, bounded Merkle lazy sync, comprehensive hardening/evaluation and final documentation. Do not begin it automatically.

## CLI execution prompt

Read docs/PHASE_5.md and current source. Preserve current working changes. Implement Gate 0 and Part A in small reviewable commits, resolving source-backed defects before adding features. Use existing runtime/crypto/storage components. Keep protocol compatibility and capability negotiation explicit. Never convert key ownership into automatic gateway authorization. Do not promise battery savings, precise distance, remote deletion or hardware security without evidence. Run specified tests and produce the acceptance documents. Continue through authorized Part A without repetitive approvals; stop for destructive/security decisions when necessary. At the Part A gate, commit, report evidence and STOP. Do not begin Part B until human approval.
