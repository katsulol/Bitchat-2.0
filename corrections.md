Yes. I reviewed the audit’s bug history and the supplied Rust API, Android BLE service, activity, manifest, Dart bridge, and relevant UI code.

**The biggest problem is more fundamental than the three phone-test failures: the supplied Flutter app bypasses your Rust core.** It sends raw text directly over BLE, so the Rust encryption, deduplication, fragmentation, and relay engine are not protecting or routing those mobile messages.

Your error history is useful, but several explanations in it are hypotheses presented as established causes. Applying only the proposed MAC-address, dedup, and MTU fixes would leave significant architectural and security problems.

Below is a source-based review and an implementation plan. I have not executed the repository or tested the phones; hardware-specific causes remain hypotheses until the tests below isolate them. The underlying Noise, fragmentation, and relay implementations are not included in the six supplied files, so their correctness cannot be established from this audit.

## 1. Critical findings: fix these first

### A. The mobile application does not use the Rust messaging pipeline

**Location:** `app/lib/mesh_bridge.dart`

The supplied implementation:

- Generates its own identifiers using Dart `Random()`.
- Constructs packets as one type byte followed by UTF-8 text.
- Sends those packets straight to Kotlin.
- Decodes received BLE bytes straight into chat messages.
- Never calls the supplied Rust API.

Consequently, for this mobile path:

| Claimed capability | What the supplied code actually does |
|---|---|
| Ed25519 identity | Displays a randomly constructed hexadecimal string |
| Encrypted direct messages | Sends plaintext with a `0x01` prefix |
| Multi-hop relaying | No forwarding in the displayed Dart/Kotlin receive path |
| Deduplication | Appends every received packet |
| Fragmentation | Sends the whole buffer to GATT |
| Hop count | Hardcodes incoming messages to `1` |
| Routing statistics | Increments UI-side counters |

This does **not** establish that your Rust work is absent or broken. It establishes that the phone app is not exercising it.

**Required fix**

Make Rust the sole owner of identity, message serialization, crypto, routing, and message-level deduplication.

The intended pipeline should be:

**Send:**  
Flutter text → Rust send API → Rust outbound frames → BLE transport → phone.

**Receive:**  
Phone → BLE transport/reassembly → Rust receive API → Rust validation/routing/decryption → Flutter message event.

Remove the Dart `[0x00/0x01, ...utf8.encode(text)]` protocol from the production path. Do not create a second implementation of your mesh protocol in Kotlin.

**Prevention:** Add an integration test that fails if a UI send bypasses the Rust send API or if a native receive callback directly creates a chat bubble.

### B. The UI currently labels plaintext as E2E

**Locations:** `mesh_bridge.dart`, `main.dart`

`sendDirect()` transmits UTF-8 plaintext. The UI then shows a lock icon and an `E2E` label based only on whether the message is a broadcast.

**Immediate fix:** Remove the lock and encrypted label from this path. Disable private sending until an authenticated session is ready, or explicitly label an intentionally separate diagnostic mode as plaintext.

A packet type byte is a routing label; it provides no confidentiality or sender authentication.

**Prevention:** Encryption status must come from the Rust security result, independently of `isBroadcast`. Use distinct fields for message audience and security state.

### C. Rust direct sending has a plaintext fallback

**Location:** `core/src/api.rs`, `send_to()`

If no completed session exists, the function calls:

`svc.node.send_to(target, text.into_bytes())`

The accompanying comment suggests handshake initiation, but the wrapper does not explicitly initiate a handshake here. Without the underlying implementation, that behavior cannot be assumed.

**Required fix:** Fail closed. Until handshake initiation and queueing are implemented, return an error such as “Secure session is not ready” and do not transmit the text.

Once implemented:

1. Start or join a handshake.
2. Hold the message in a bounded pending queue.
3. Send only after identity verification and session completion.
4. Report handshake timeout or failure.
5. Never silently switch to plaintext.

### D. Rust treats decryption failures as displayable text

**Location:** `core/src/api.rs`, `poll_inbound()`

The code tries `decrypt_inbound()`. If it fails, it falls through to:

`String::from_utf8_lossy(&msg.payload)`

That mixes legitimate public messages with failed encrypted-message processing.

**Required fix:** Dispatch by the validated protocol message type first.

- Handshake → handshake handler.
- Explicit public chat → public-message decoder.
- Encrypted chat → authenticate/decrypt; failure means reject.
- Unsupported type → reject.

Do not display a ciphertext buffer as text after authentication failure.

**Prevention:** Tests must confirm that corrupted ciphertext, unknown sessions, and invalid authentication tags produce **no chat message**.

## 2. The three current phone failures

### Failure 1: “Four devices” when there are three phones

**Confirmed:** Your peer list uses Bluetooth addresses, and both client-side and server-side callbacks add entries.

**Not proven:** That the exact extra entry was caused by address rotation. Dual connections, stale state after restarting, and missing cleanup are also plausible from this code.

There is an additional issue: the scan response advertises a ShortId, but the scanner does not extract and associate it with the connection. The claimed negotiated identity mapping is not present in the supplied service.

**Correct fix**

Maintain separate concepts:

- **Link:** A particular GATT connection, role, and connection generation.
- **Peer:** A verified cryptographic identity.
- **Routing ShortId:** A compact identifier associated with that identity.

One peer can have several links. Count peers, not links.

The registry should support:

- Link → verified peer.
- Peer → set of ready links.
- Disconnecting one link without removing a peer that still has another.
- Removing stale links on stop, timeout, and Bluetooth shutdown.
- Excluding the local identity.

Advertisement ShortIds can be discovery hints; they are not identity proof.

**Acceptance test:** In a fully connected three-phone test, each phone shows two remote peers. If displaying a total including self, that total is three. Document which count your UI uses.

### Failure 2: Duplicate messages

**Confirmed:** `writeFrame()` sends through both outgoing writes and incoming notifications. The receiver renders every callback.

Using one preferred link per peer will reduce redundant traffic, but it is insufficient on its own: legitimate mesh flooding also creates duplicates.

**Correct fix**

1. Generate the message ID once in Rust at message creation.
2. Preserve it across forwarding, retransmission, and alternate paths.
3. Use the Rust seen-cache for routing decisions.
4. Apply message delivery deduplication after the appropriate validation.
5. Expose the same ID to Flutter for local-echo reconciliation.

Keep caches bounded and time-limited. Fragment tracking must distinguish individual fragments from completed messages; receiving one fragment must not suppress the rest.

**Do not deduplicate by message text.** Two intentional sends of “hello” must remain two messages.

**Acceptance tests:**

- One message arriving over two links → one bubble.
- The same message returning through a relay → no additional bubble.
- Two separately sent identical texts → two bubbles.
- A reflected local send → update the original bubble, not append another.

### Failure 3: Redmi cannot exchange messages

The supplied code establishes several transport defects:

- No negotiated MTU tracking.
- No BLE segmentation in this send path.
- MTU request and discovery separated by a guessed 300 ms delay.
- No descriptor-write completion handling.
- No send queue.
- No per-client notification subscription state.
- No meaningful checking of write/notification results.

These are enough to investigate before attributing the problem to a chipset.

The audit’s claims that a particular MediaTek driver silently drops these writes, or that MIUI specifically requires a shared `descriptor.value` assignment, are **not demonstrated by the supplied evidence**.

**Correct fix: implement a per-link state machine**

Connection → service discovery → characteristic validation → MTU handling → notification subscription completion → protocol handshake → ready.

Serialize asynchronous operations. A delay is not proof that the preceding operation completed.

Use the actual negotiated MTU. Ordinary notification/write payload capacity is MTU minus three bytes; the default MTU of 23 leaves 20 bytes. Your stated 31-byte envelope alone already exceeds that.

Therefore:

- Support operation at the default MTU.
- Segment larger frames before sending.
- Reassemble bytes before protocol decoding.
- Bound frame size, concurrent assemblies, and reassembly lifetime.
- Track MTU on both client and server connection paths.
- Never assume that requesting 512 means 512 was granted.

Before adding a second fragmentation format, inspect the existing Rust fragmentation layer and define precisely which layer handles BLE-sized segments.

**Acceptance test:** Explicitly test the default MTU path, including messages that cross the 20-byte boundary. A system that works only after a large MTU negotiation is incomplete.

## 3. Additional Android defects

### A. CCCD state must be per client

**Location:** `onDescriptorWriteRequest()`

Currently, any write to the CCCD adds the device to `serverConnectedDevices`. It does not distinguish enabling notifications from disabling them.

Also, devices are already added when they connect, so `writeFrame()` can attempt notifications before subscription.

**Fix:**

- Keep connection state and subscription state separate.
- Validate descriptor UUID, parent characteristic, offset, length, and value.
- Accept notification enable or disable as appropriate.
- Reject unsupported prepared writes unless you implement them.
- Notify only subscribed clients.
- Implement descriptor reads using that client’s subscription state.
- Clear subscriptions on disconnect.

Assigning `descriptor.value` alone cannot represent different subscription states for multiple clients.

### B. GATT writes are accepted indiscriminately

**Location:** `onCharacteristicWriteRequest()`

The implementation acknowledges success and forwards the payload without validating the characteristic, offset, or prepared-write flag.

**Fix:** Validate first, then acknowledge acceptance. Reject unsupported prepared writes and nonzero offsets. Do not forward partial prepared writes as complete mesh frames.

For writes that do not request responses, reject invalid input locally and increment a diagnostic counter.

### C. Operations and notifications need queues

**Location:** `writeFrame()`

Writes can overlap with service discovery, descriptor subscription, and other writes. Return values are ignored.

**Fix:**

- Serialize client GATT operations per connection.
- Check immediate enqueue/start results.
- Track callback completion and timeout.
- Pace notifications using completion feedback.
- Bound pending bytes and frames.
- Return backpressure instead of silently dropping.

For the initial reliability baseline, acknowledged writes are easier to diagnose than `WRITE_TYPE_NO_RESPONSE`. Their acknowledgment still does not mean that the destination application delivered the message.

### D. Connection timeout does not close the pending connection

The eight-second timer only removes an address from `connectingDevices`. The returned `BluetoothGatt` is not retained immediately.

An old attempt can still complete while a new attempt starts.

**Fix:**

- Store each pending GATT handle immediately.
- Use an atomic gate for starting attempts.
- Assign an attempt generation/token.
- On timeout, close that specific handle.
- Ignore stale callbacks.
- Apply bounded retry backoff with jitter.

A stale disconnection callback must not remove a newer connection stored under the same address.

### E. “Connected” is emitted before the link is usable

`onPeerConnected()` fires before discovery, subscription, and protocol identity verification.

**Fix:** Expose separate connecting, transport-ready, and verified-peer-ready states. Enable chat only at the required state.

### F. Advertising starts before service registration completes

`addService()` is asynchronous, but `startMesh()` immediately proceeds to advertising.

**Fix:** Check the service-add result and start connectable advertising after successful `onServiceAdded()`. Report failure as startup failure.

### G. Discovery matching is overly broad

The scanner accepts UUID strings containing `"FD6F"` and even arbitrary raw advertisement bytes containing the two-byte sequence.

**Fix:** Match the exact service UUID and validate protocol service data. Remove substring and arbitrary-byte matching.

Also replace the borrowed Bluetooth-base service/characteristic identifiers with project-specific UUIDs. Recalculate advertisement sizes when doing so.

### H. Stop/restart cleanup is incomplete

The supplied BLE service has no `onDestroy()` cleanup. Posted startup and scan-refresh callbacks can outlive a stop request. Bluetooth-off handling does not clear the runtime.

**Fix:**

- Make start and stop idempotent.
- Track a desired-running flag and generation.
- Cancel pending startup, reconnect, refresh, and operation callbacks.
- Close pending and established GATT handles.
- Stop advertising/scanning.
- Close the server.
- Unregister the receiver.
- Clear peer/subscription state and notify observers.
- Restart on Bluetooth-on only if the user still wants the mesh running.

### I. Foreground-service failure is hidden

The service catches `Throwable` around promotion and continues. The activity tries `startService()` after a foreground-service start failure.

That does not establish a valid background-running service.

**Fix:** Start through an eligible visible-user flow after permissions are granted. If promotion fails, report a specific startup error and stop that attempt. Do not claim background operation succeeded.

### J. The activity owns the supposedly background service

`MainActivity.onDestroy()` stops the BLE service. More fundamentally, frame delivery depends on the activity’s MethodChannel listener and the Dart bridge.

**Fix:** Decide explicitly:

- **Foreground-only prototype:** Stop on leaving and label the limitation.
- **Background mesh:** Put the ongoing transport-to-Rust runtime in a service-owned architecture, or an explicitly managed background Flutter runtime.

A foreground notification alone does not keep an activity-owned message pipeline functioning.

## 4. Rust API defects beyond encryption

### A. ShortId parsing can panic on Unicode input

`hex_to_short_id()` checks byte length and then slices a UTF-8 string at fixed byte positions. A string with multibyte characters can pass the length check and panic at a non-character boundary.

**Fix:** Validate ASCII hexadecimal bytes before slicing, or decode directly from byte pairs with checked conversion.

**Tests:** Wrong lengths, non-ASCII text, invalid hex, lowercase/uppercase, and arbitrary UTF-8 must return errors without panicking.

### B. Polling errors are silently discarded

This loop hides errors:

`while let Ok(true) = svc.node.poll_step(...).await {}`

Handshake processing also discards its result.

**Fix:** Distinguish progress, idle, and failure. Record or propagate failures. Give each poll invocation a bounded work budget.

### C. A write lock spans potentially substantial async work

`poll_inbound()` holds the global write lock while stepping the mesh and handling messages.

**Fix:** First bound the work performed per call. Longer term, use a task that owns the node and accepts commands through bounded channels. This makes runtime ownership and backpressure clearer.

### D. Flutter delivery lacks message IDs

`BridgeMessage` contains no message ID.

**Fix:** Add the actual Rust message ID and an explicit security state to delivery events. Do not invent the underlying field name until inspecting the core message type.

### E. Initialization can replace a live runtime

`init()` replaces the singleton with a new node.

**Fix:** Make repeated initialization with the same configuration return the existing identity, or require an explicit controlled shutdown before replacement.

## 5. Dart and UI reliability fixes

In addition to reconnecting Rust:

- **Stop swallowing every exception.** Show startup, send, and transport errors.
- **Stop reporting success when the service is null.** `MainActivity` currently returns success after `bleService?.writeFrame(...)`, even if nothing ran.
- **Distinguish accepted, queued, transmitted, and delivered.** The present counters do not establish delivery.
- **Retain and cancel stream subscriptions.**
- **Detach the MethodChannel handler on bridge disposal.**
- **Prevent callbacks from writing to closed streams.**
- **Bound the in-memory chat list.**
- **Await send acceptance before clearing the composer**, or preserve failed drafts.
- **Avoid converting a failed direct conversation into public sending.** When a selected peer disappears, the current UI resets the recipient to broadcast. Keep the recipient selected and mark it unavailable instead.
- **Remove fabricated hop counts.** Display routing metadata from Rust.
- **Load the real public identity from Rust.** `Random.secure()` would still not turn a fabricated string into an Ed25519 public key.

That automatic recipient reset is particularly important: a disconnection should never make the next private-intended message public.

## 6. Corrections to the audit’s twelve historical bugs

| Historical item | Assessment and prevention |
|---|---|
| 1. Panic lint failures | Replacing panic-prone calls is sensible. Clippy lints require a Clippy CI job; ordinary `cargo check` does not establish those checks passed. Explicit-call linting also misses risks such as invalid string slicing. |
| 2. Noise replay/reordering | A bitmap replay window alone does not prove out-of-order decryption works. Inspect explicit nonce handling and the session API. Update replay state only after successful authentication. Test forged high counters, replay, reordering, and restart. |
| 3. NDK mismatch | Missing `source.properties` indicates an unusable installation/path. The error alone does not prove AGP requires the exact replacement version claimed. Pin a compatible installed version and reproduce it in CI. |
| 4. Early foreground-service permissions | Permission-before-start is the right direction. Also report denial and avoid running after failed promotion. |
| 5. Notification callback overloads | Supporting both is reasonable. Deprecation alone does not prove that older overrides are universally ignored. Trace callbacks on each phone. |
| 6. GATT 133 | Connection storms are plausible; the timeout fix is incomplete because it does not own and close pending handles. |
| 7. PUB/E2E header | A type flag fixes classification only. It does not implement encryption. |
| 8. MIUI installation restriction | Treat the shell installation command as an observed workaround, not a universal bypass. Record OS settings and obtain installation consent through supported flows. |
| 9. Advertising size | Splitting advertisement and scan response is useful. The stated size calculation mixes UUID widths; recalculate using the actual chosen encoding and verify advertising callbacks. |
| 10. Hardware scan filtering | Software filtering is a diagnostic alternative. The claimed cross-chipset failure mechanism needs evidence. Current raw-byte matching introduces false positives. |
| 11. Foreground-service restriction | Falling back to a normal service is not a general solution to background-start restrictions. Fix lifecycle eligibility and report failure. |
| 12. Scan permission behavior | The universal “100% scan drop” explanation is not established. Use a version-specific permission policy and isolate permission denial from scanner failure. Reassess the location assertion when implementing proximity features. |

The same evidence discipline should apply to test counts: your briefing says 25 tests, while the audit reports 54. Record the commit, command, test output, and scope alongside each count.

## 7. Implementation order

Do this in small commits so that each change has an observable effect.

### Step 1 — Make the current prototype honest

Remove encrypted labels from the raw BLE path, disable insecure direct sending, expose startup/send failures, and stop resetting lost recipients to broadcast.

**Exit condition:** No plaintext packet is represented as an encrypted message.

### Step 2 — Stabilize BLE as a byte transport

Implement link state, operation queues, CCCD tracking, actual MTU tracking, bounded segmentation/reassembly, connection ownership, and complete cleanup.

Use a clearly separate diagnostic payload path during transport testing.

**Exit condition:** All three phone pairs exchange short and fragmented byte payloads in both directions.

### Step 3 — Reconnect Flutter to Rust

Use Rust identity and send APIs; feed native frames into Rust; drain Rust outbound frames; deliver only Rust message events to Flutter.

Keep a single runtime pump—avoid overlapping asynchronous periodic polls.

**Exit condition:** Phone sends and receives visibly affect Rust routing counters and use the real identity.

### Step 4 — Bind links to verified identities

Associate transport links with authenticated peers and expose unique peers to the UI.

**Exit condition:** Duplicate links do not create duplicate users; a stale callback cannot remove a live peer.

### Step 5 — Enforce encrypted-session behavior

Remove plaintext fallback, implement handshake lifecycle, reject failed decryption, and expose security status.

**Exit condition:** A relay can forward a direct message without obtaining its plaintext.

### Step 6 — Verify actual mesh forwarding

Test a three-phone chain with A and C unable to communicate directly.

**Exit condition:** A→C delivery depends on B. When B’s relay path is removed, delivery stops or queues according to the implemented behavior.

Only then move on to Iroh integration. Otherwise you will be debugging gateway routing on top of an unstable local transport.

## 8. Physical-phone test plan

Use all three pairs:

- Oppo ↔ OnePlus
- Oppo ↔ Redmi
- OnePlus ↔ Redmi

For each pair, reverse which phone starts first.

| Test | What it isolates |
|---|---|
| Very short payload, both directions | Basic write/notification path |
| Payload crossing 20 bytes | Default-MTU handling |
| Larger payloads and Unicode | Segmentation and byte reassembly |
| Burst of distinct messages | Queueing and backpressure |
| Two identical texts with different IDs | Correct dedup semantics |
| Same ID over two links | Duplicate suppression |
| Notification enable, disable, re-enable | Per-client CCCD behavior |
| Bluetooth off/on | Cleanup and recovery |
| Repeated start/stop | Stale callbacks and resource leaks |
| Screen locked/background | Actual service-runtime behavior |
| Permission denied/revoked | Controlled failure |
| Three-phone triangle | Flooding and duplicate paths |
| Three-phone chain | Real relay behavior |

For Redmi specifically, start with **short bidirectional traffic while all apps remain visible**. If that works and larger traffic fails, investigate MTU/segmentation. If only one direction fails, inspect subscriptions and notification completion. If failure appears only after backgrounding, investigate lifecycle and power management.

Change one variable at a time.

## 9. Instructions to prevent recurrence

Put these requirements into your next phase specification:

> **Architecture**
> - Rust owns identity, protocol encoding, crypto, routing, and message IDs.
> - Kotlin transports opaque bytes and manages BLE links.
> - Flutter renders validated events and submits user intent.
>
> **Security**
> - No private-message plaintext fallback.
> - No encrypted label without a successful security result.
> - No authentication-failure-to-text fallback.
> - Discovery identifiers are untrusted until authenticated.
>
> **Asynchronous behavior**
> - Every pending operation has an owner, timeout, and cancellation path.
> - Callback state changes verify the connection generation.
> - Readiness depends on successful completion, not fixed sleeps.
>
> **Resource limits**
> - Bound queues, seen-caches, pending handshakes, reassemblies, and message history.
> - Define backpressure and timeout behavior explicitly.
>
> **Evidence**
> - Separate observed symptom, code-confirmed defect, suspected cause, and verified fix.
> - Every bug fix adds a regression test.
> - Simulator success and physical-phone success are recorded separately.
> - Do not mark work complete based only on an APK building.

Run formatting, Rust tests, Clippy, Flutter analysis/tests, and Android build/lint checks in CI. Add parser fuzzing and deterministic simulations for loss, duplication, reordering, malformed fragments, and session teardown.

For phone logs, record connection generation, role, state transition, negotiated MTU, operation result, frame length, and drop reason. Avoid message plaintext, secret keys, and unnecessary persistent identity logs.

**What this review establishes:** The immediate priority is to repair the mobile-to-Rust integration and BLE lifecycle, not merely hide duplicate bubbles. The supplied source explains several failures directly; it does not justify declaring Redmi’s chipset the cause.

Would you like me to continue with a concrete patch specification for `api.rs`, `BitchatBleService.kt`, `MainActivity.kt`, and `mesh_bridge.dart`, separating exact replacements from changes that require the missing core interfaces?