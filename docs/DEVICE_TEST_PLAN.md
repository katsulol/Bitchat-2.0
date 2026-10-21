# Bitchat 2.0 Physical Device Test Plan

> [!IMPORTANT]
> **Status: UNVERIFIED (Awaiting Physical Hardware Verification)**
> All Phase 2 Part B components (`BridgeTransport`, native Kotlin BLE GATT service, platform channels, and Flutter UI) have passed unit tests, widget tests, static analysis, and simulated mesh integration. Physical RF propagation, OS Bluetooth stack behavior, and Android background lifetime must be verified using this protocol on physical hardware.

---

## 1. Test Objectives & Equipment

### Objectives
1. Verify dual-role BLE GATT (simultaneous peripheral advertising and central scanning) between real Android radios.
2. Confirm multi-hop store-and-forward relaying over physical distances where Phone A and Phone C cannot directly communicate.
3. Validate Android Foreground Service persistence across screen-off (Doze mode) events.
4. Measure real-world transmission latency and battery consumption baseline.

### Equipment Required
- **2 to 3 Android devices**: Android 12 (API 31) through Android 15 (API 35) with BLE 4.2+ support.
- **Physical environment**:
  - Test 1, 3, 4, 5: Standard indoor room.
  - Test 2 (Line Relay): Long hallway, multi-room building with concrete/brick walls, or outdoor area providing at least 25–40 meters separation between Phone A and Phone C.

---

## 2. Device Setup & Installation

1. **Prerequisites on Developer Machine**:
   - Flutter SDK 3.47+ (`flutter doctor` clean for Android).
   - Android Studio with Android SDK Platform 34/35 and Command-line Tools.
2. **Compile Core Rust Binary for Android**:
   ```bash
   # Install cargo NDK targets if not present:
   rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
   
   # Build core staticlib/cdylib:
   cargo build --manifest-path core/Cargo.toml --target aarch64-linux-android --release
   ```
3. **Build and Install App on Devices**:
   ```bash
   cd app
   flutter build apk --debug
   adb -s <DEVICE_SERIAL> install build/app/outputs/flutter-apk/app-debug.apk
   ```
4. **App Permissions**:
   - Launch app on each phone.
   - Accept runtime permissions: **Nearby Devices** (`BLUETOOTH_SCAN`, `BLUETOOTH_ADVERTISE`, `BLUETOOTH_CONNECT`) and **Notifications** (`POST_NOTIFICATIONS`).
   - Verify persistent notification appears in Android status tray: *"Bitchat Mesh Service: Active mesh node (scanning & advertising)"*.

---

## 3. Test Procedures

### Test 1: Two-Node Direct Discovery and Bidirectional Exchange
- **Setup**: Place Phone A and Phone B on a table separated by 1–2 meters.
- **Procedure**:
  1. Open Bitchat 2.0 on Phone A and Phone B. Note each phone's displayed `ShortId`.
  2. Within 5–15 seconds, confirm Phone A shows Phone B's `ShortId` in the *Active Neighbors* chip list, and vice versa.
  3. On Phone A, type `"Hello from Phone A"` and tap send.
  4. Record on Phone B: Does the message appear in the feed? Is the `[PUB]` badge present?
  5. Repeat in reverse from Phone B to Phone A.
  6. Repeat with an authenticated private message by tapping Phone B's chip on Phone A (verifying `[E2E]` badge).
- **Pass Criteria**: Both public and E2E messages arrive reliably within < 1.5 seconds.

---

### Test 2: Three-Node Multi-Hop Line Relay (A ↔ B ↔ C)
- **Setup**:
  - Position Phone A in Room 1.
  - Position Phone C in Room 3 or down a corridor such that direct RSSI between A and C is undetectable (out of radio range: verify A does NOT see C in *Active Neighbors*).
  - Position Phone B in the middle (Room 2) equidistant to both Phone A and Phone C.
- **Procedure**:
  1. Confirm Phone A sees only Phone B in *Active Neighbors*.
  2. Confirm Phone C sees only Phone B in *Active Neighbors*.
  3. Confirm Phone B sees both Phone A and Phone C.
  4. From Phone A, send broadcast: `"Multi-hop test A to C"`.
  5. Observe Phone C: Verify message arrives. Tap *Debug Stats* to confirm `relayed_count >= 1` on Phone B and `received_count >= 1` on Phone C.
  6. From Phone C, reply back to Phone A.
- **Pass Criteria**: Phone C receives Phone A's transmission despite zero direct radio link between A and C.

---

### Test 3: Intermediate Relay Termination & Reconvergence
- **Setup**: Same as Test 2 (Line topology A ↔ B ↔ C actively communicating).
- **Procedure**:
  1. Force-kill the app on Phone B (swipe away from Android Recents).
  2. Wait 10 seconds. Verify Phone A and Phone C update their *Active Neighbors* lists and drop Phone B.
  3. Attempt to send a message from Phone A. Confirm Phone C does not receive it.
  4. Relaunch Bitchat 2.0 on Phone B.
  5. Allow 15 seconds for BLE discovery and connection establishment.
  6. Send a new message from Phone A.
- **Pass Criteria**: Relay link recovers automatically upon Phone B's restart without requiring app restarts on Phone A or Phone C.

---

### Test 4: Screen-Off / Android Doze Persistence
- **Setup**: Phone A and Phone B paired and connected.
- **Procedure**:
  1. On Phone B, turn off screen and leave device untouched on table for 5 minutes (triggering Android Doze maintenance window).
  2. Verify foreground service notification remains present on lock screen.
  3. From Phone A, send 3 distinct messages at 1-minute intervals.
  4. After 5 minutes, turn on Phone B's screen (without unlocking).
  5. Unlock Phone B and inspect message log.
- **Pass Criteria**: All 3 messages received in order with zero packet loss during screen-off period.

---

### Test 5: Battery Consumption Baseline (30-Minute Screen-Off)
- **Setup**: Fully charged Android device (or record starting battery percentage at >= 80%).
- **Procedure**:
  1. Record starting battery percentage and battery temperature.
  2. Leave Bitchat 2.0 foreground service running with 1 active neighbor.
  3. Turn off screen and leave device idle for exactly 30 minutes.
  4. Note final battery percentage and temperature.
- **Pass Criteria**: Battery drain does not exceed 3% over 30 minutes in idle mesh discovery mode.

---

## 4. Test Execution Record Table

Fill out this verification matrix during physical execution:

| Test ID | Description | Device Models & Android Versions | Expected Result | Measured Latency / Battery | Pass / Fail | Notes & Observations |
|---|---|---|---|---|---|---|
| **TEST-01** | 2-Phone Direct Discovery & Pub/E2E Exchange | Phone A: <br> Phone B: | Discovery < 15s; Message latency < 1.5s | Latency: _____ ms | `[ ] PASS` <br> `[ ] FAIL` | |
| **TEST-02** | 3-Phone Line Relay (A ↔ B ↔ C) | Phone A: <br> Phone B: <br> Phone C: | Message arrives at C via B; Hop count = 1 | Latency: _____ ms | `[ ] PASS` <br> `[ ] FAIL` | Distance A-C: _____ m |
| **TEST-03** | Relay Termination & Reconvergence | Phone A: <br> Phone B: <br> Phone C: | Loss detected; Reconnects after restart | Reconnect: _____ s | `[ ] PASS` <br> `[ ] FAIL` | |
| **TEST-04** | Screen-Off 5-Min Doze Delivery | Phone A: <br> Phone B: | All packets received while screen off | Lost packets: _____ | `[ ] PASS` <br> `[ ] FAIL` | Foreground service verified |
| **TEST-05** | 30-Minute Screen-Off Battery Baseline | Device: <br> Android OS: | Battery drain <= 3% in 30 min | Start: ___% <br> End: ___% <br> Delta: ___% | `[ ] PASS` <br> `[ ] FAIL` | Screen off, idle BLE scan/adv |
