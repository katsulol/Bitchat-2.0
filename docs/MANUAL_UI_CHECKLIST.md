# Bitchat 2.0 — Task 10 Manual Mobile UI & Accessibility Checklist

Run this checklist alongside `flutter analyze` and `flutter test` (`app/test/phase6_ux_test.dart`) on physical Android test phones. Mark actual observations alongside `docs/DEVICE_TEST_RESULTS_FINAL.md` during human-run Task 13.

## 1. Fail-Closed Identity, Keystore, ABI & Permissions
- [ ] Launch fresh install -> Verify `My ID:` (`16`-hex ShortId) and full `64`-hex Ed25519 `PeerId` render cleanly; provenance badge shows expected `ABI v2` and commit hash in Diagnostics.
- [ ] Simulate stale/mismatched `.so` (e.g., ABI v1 build against v2 Dart) -> Verify red fail-closed banner (`Native ABI mismatch (fail-closed)`) appears and storage does NOT open.
- [ ] Deny `BLUETOOTH_SCAN` / `BLUETOOTH_CONNECT` runtime permission -> Verify orange banner displays missing permissions with `Retry` button; granting via `Retry` resumes BLE scan.
- [ ] Lock/corrupt Keystore DEK test -> Verify `Encrypted storage locked (fail-closed)` banner prevents silent key regeneration.

## 2. Trust, Pending Requests & 1-Hop Voucher Boundaries
- [ ] Receive direct encrypted message from unrecognized peer -> Verify message is queued under **Pending Message Requests** inside **Trust & Vouchers** dialog.
- [ ] Tap **Approve** -> Sender moves to Approved state cleanly.
- [ ] Tap **Block** -> Sender moves to Blocked state cleanly; subsequent frames dropped without notification leakage.
- [ ] Open Voucher Issuance / Import section -> Confirm copy explicitly states `"Scope: 1-hop introduction only (subject -> audience, bounded TTL). Never transitive, never auto-approves relay gateways, and does NOT prove unique human identity."`.

## 3. Routing, Delivery State Accuracy & Gateway Consent
- [ ] Select peer and send Normal message while recipient BLE/gateway is absent -> Draft clears upon queueing (`Queued`); failed send preserves draft text in input box without clearing.
- [ ] Disconnect currently selected private peer -> Recipient bar remains selected as `(Offline)` and NEVER silently reverts to public `Broadcast`.
- [ ] Observe gateway forward -> Status shows `InFlight` (`Waiting for recipient acknowledgment.`), NEVER `Delivered` until end-to-end ACK returns (`Delivered via <Transport> (acknowledged).`).
- [ ] Verify Cancelled / Failed / Expired message badges show exact honest state and allow cancelling Queued/InFlight messages.
- [ ] Open **Iroh Gateway** sheet -> Verify pending gateway peers require explicit **Approve** before forwarding; **Revoke/Block** halts relay immediately.

## 4. Volatile Chat, Privacy & Lock-Screen / Keyboard Hygiene
- [ ] Toggle Volatile Chat (`5m` or `15m`) on unsupported or negotiating peer -> Composer displays `"sending blocked (no silent downgrade)"`; attempting send rejects and preserves draft text.
- [ ] With active Volatile session (`Active`): confirm `FLAG_SECURE` screenshot/recents protection enables, `autocorrect`/`enableSuggestions` disable on input box, and tapping `Cancel` (back to persistent chat) clears `FLAG_SECURE`.
- [ ] Lock phone with BLE mesh foreground notification active -> Confirm notification lock-screen visibility (`VISIBILITY_SECRET`) hides message content and peer previews.

## 5. Proximity, Resource Profiles & Experimental Controls
- [ ] Adjacent direct BLE peer chip displays coarse category (`Immediate`, `Near`, `Far`) or `Unknown` when stale/unbound/via-gateway — NEVER distance in meters.
- [ ] Open **Diagnostics** sheet -> Switch between `Receive only`, `Minimal`, `Balanced`, and `Help the network` chips; confirm transit counters update and charging/Wi-Fi disclaimer remains visible.
- [ ] Under **Experimental Controls**: verify both **Traffic Padding** and **Normal-Mode Sync** start **Default OFF** with explicit threat-model / crypto-review disclaimers; verify peer sync failure shows SnackBar error without crashing.

## 6. Viewport & Accessibility Sweep
- [ ] Test at narrow screen width (`320x640`) and Accessibility Font Scale `1.8x` -> Zero Flutter `RenderFlex overflow` warnings across main screen, Trust sheet, Gateway sheet, and Diagnostics sheet.
