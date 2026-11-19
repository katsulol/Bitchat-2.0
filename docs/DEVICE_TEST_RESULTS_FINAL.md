# Bitchat 2.0 — Physical Multi-Device Verification Matrix & Results (`Task 13`)

## 1. Honest Execution Scope & Verification Policy

> **IMPORTANT — NO FABRICATED HARDWARE CLAIMS**:
> Per `PHASE_6.md` Task 13 rules: *"CLI may prepare harnesses/checklists and analyze logs; it cannot claim phone tests it did not observe/run through authorized instrumentation. Record device models/OS, app/native hashes, steps, timestamps, results and redacted evidence. Missing device means Pending, not PASS."*
>
> All Rust core unit, property, and end-to-end multi-node tests (`216/216` passing) and Flutter widget/integration tests (`13/13` passing) have been executed and verified on the build host. Because no physical Android phones are attached via `adb` during this unattended CLI build session, **every physical device test below is explicitly recorded as `Pending (Human Physical Run Required)`**.

---

## 2. Build & Native Artifact Identity for Physical Testing

Install the verified Phase 6 APK on each test phone (`adb install -r app/build/app/outputs/flutter-apk/app-release.apk`) and confirm the provenance fields in the **Diagnostics** sheet match the final release build recorded in `docs/BUILD_PROVENANCE.md` and `FINAL_REPORT_AND_HANDOFF.md`:

- **App Version**: `2.0.6+6` (`v2.0.6-phase6`, `CORE_ABI_VERSION = 2`)
- **Automated Log & Battery Capture Script**: `docs/capture_device_logs.ps1`
- **Manual UI Checklist**: `docs/MANUAL_UI_CHECKLIST.md`

### Device Roster Template (To Be Filled During Physical Run)

| Role | Device Label | Make / Model | Android OS | BLE 5.0+ Adv/Scan | Keystore Level (`Diagnostics`) | Serial (Redacted) |
|---|---|---|---|---|---|---|
| **Phone A** | Offline Endpoint A | *Pending* | *Pending* | *Pending* | *Pending* | `REDACTED-A` |
| **Phone B** | BLE + Iroh Gateway B | *Pending* | *Pending* | *Pending* | *Pending* | `REDACTED-B` |
| **Phone C** | BLE + Iroh Gateway C | *Pending* | *Pending* | *Pending* | *Pending* | `REDACTED-C` |
| **Phone D** | Offline Endpoint D | *Pending* | *Pending* | *Pending* | *Pending* | `REDACTED-D` |

---

## 3. Mandatory 12-Scenario Physical Verification Matrix

| # | Scenario | Required Setup & Verification Steps | Sim / Host E2E Coverage | Physical Status | Redacted Evidence / Log Reference |
|---:|---|---|---|---|---|
| **1** | **Pairwise BLE Handshake & Reconnect (Both Directions)** | Test all available phone pairs (`A->B`, `B->A`). Verify `LINK_HELLO` exchange, Noise XX 3-message handshake (`(E2E Encrypted)` badge), message + ACK (`0h`), and automatic session recovery after toggling Bluetooth off/on. | **Simulator PASS** (`test_rb4`, `test_phase6_auth_e2e`) | **Pending** (Human Physical Run) | Capture via `docs/capture_device_logs.ps1 -Scenario 1` |
| **2** | **3-Phone Enforced Line (`A <-> B <-> C`)** | Enforce line topology (either physical separation where `A` and `C` cannot hear each other directly, or software block of direct `A<->C` link — label clearly). Send `A -> C`; verify `1h` hop badge on `C`, `transit_packets_forwarded > 0` on `B`, and confirm stopping `B` pauses/queues delivery until `B` resumes. | **Simulator PASS** (`test_rb3`, `test_resource_forwarding_e2e`) | **Pending** (Human Physical Run) | Capture via `docs/capture_device_logs.ps1 -Scenario 2` |
| **3** | **Full 4-Role Chain (`Offline A -> BLE -> Online B -> Iroh -> Online C -> BLE -> Offline D`)** | `A` and `D` have Wi-Fi/Cellular OFF (BLE only). `B` and `C` have BLE + Internet ON and mutually approve each other in **Iroh Gateway** sheet. Send `A -> D`; verify fresh E2E Noise session between `A` and `D`, reply `D -> A`, and verify `B`/`C` logs show zero plaintext or broadcast export. *(Note: Iroh-only laptop without BLE is partial; full PASS requires 4 phones or functioning laptop BLE peripheral/central.)* | **Simulator/Localhost PASS** (`test_rb4_four_role_chain_a_ble_b_iroh_c_ble_d`) | **Pending** (Human Physical Run) | Capture via `docs/capture_device_logs.ps1 -Scenario 3` |
| **4** | **Separate Real Networks & Live Iroh Path Observation** | Connect Gateway `B` to Wi-Fi ISP #1 and Gateway `C` to Cellular carrier (not same hotspot). Observe `Gateway Observed Path` in **Diagnostics** (`Direct` vs `Relay` vs `Mixed`) over 5 minutes. Do not label same-hotspot NAT tests as cross-network hole-punching proof. | **Localhost QUIC PASS** (`test_t5_live_iroh_path_observation`) | **Pending** (Human Physical Run) | Capture via `docs/capture_device_logs.ps1 -Scenario 4` |
| **5** | **Lost ACK, Receiver Absent, Queued Reconnect, Restart, TTL Expiry & Storage-Full** | Send to offline peer (`Queued`), force-stop & reopen sender (`bitchat_outbox.db` restores inflight/queued record), bring receiver online -> auto-delivers & transitions to `Delivered`. Test `2m` deadline expiry (`Expired`) while receiver stays offline. | **Simulator PASS** (`test_rb1_restart_recovery_and_outbox_cap`, `test_delivery_e2e`) | **Pending** (Human Physical Run) | Capture via `docs/capture_device_logs.ps1 -Scenario 5` |
| **6** | **Approval, 1-Hop Vouch, Block & Revocation Across Restart** | Send from unapproved `A` to `B` -> lands in `B`'s **Pending Message Requests**. Approve via UI or import 1-hop voucher (`A -> B -> C`, verify non-transitive to `D`). Block `A`, force-stop and restart `B`, verify `bitchat_trust.db` keeps `A` blocked and gateway consent persists without re-prompting. | **Simulator PASS** (`test_phase6_trust_persistence_e2e`, `test_rb5`) | **Pending** (Human Physical Run) | Capture via `docs/capture_device_logs.ps1 -Scenario 6` |
| **7** | **Resource Profile Transit Enforcement (`Receive only` / `Minimal` / `Balanced` / `Help network`)** | On relay `B` in `A <-> B <-> C`, switch `B` to `Receive only` (`Paused`) -> confirm transit forwarding stops (`Dropped by policy` increments) while `B`'s own direct messages still work. Switch `B` to `Balanced` -> transit forwarding resumes. | **Simulator PASS** (`test_resource_forwarding_e2e`, `test_rb6`) | **Pending** (Human Physical Run) | Capture via `docs/capture_device_logs.ps1 -Scenario 7` |
| **8** | **BLE MTU Extremes (23B Default & 247B+), Unicode & Fragment Recovery** | Exchange multi-paragraph UTF-8/Emoji messages (~350B) across devices with default 23B ATT MTU (20B payload) and high MTU (247B+). Walk out of range mid-transfer to trigger partial fragments and verify clean 15s garbage collection + retry recovery. | **Simulator PASS** (`test_rb8_ble_mtu_20_fragmentation_loss_and_reassembly_gc`) | **Pending** (Human Physical Run) | Capture via `docs/capture_device_logs.ps1 -Scenario 8` |
| **9** | **Radio & Android Lifecycle Transitions** | Toggle Bluetooth OFF/ON, switch Wi-Fi to Cellular, lock screen for 3 minutes, rotate screen (activity recreation), and test foreground service survival (`BitchatBleService`). Confirm `uiGeneration` prevents stale callback crashes. | **Host FFI Lifecycle PASS** (`test_phase6_ffi_lifecycle`) | **Pending** (Human Physical Run) | Capture via `docs/capture_device_logs.ps1 -Scenario 9` |
| **10** | **Root Identity Upgrade & Fail-Closed Keystore Lock** | Upgrade over previous APK install -> verify `My ID` (`ShortId` & `PeerId`) is preserved after `identity.key` -> `identity.enc` (`BCID` v1) migration. Verify missing/locked Keystore displays `Encrypted storage locked (fail-closed)` without regenerating identity. | **Simulator PASS** (`test_phase6_identity_migration_e2e`, `test_rb7`) | **Pending** (Human Physical Run) | Capture via `docs/capture_device_logs.ps1 -Scenario 10` |
| **11** | **Volatile Chat Capability, No-Downgrade & Storage Audit** | Enable Volatile Chat (`5m`) between `A` and `B`. Verify `FLAG_SECURE` blocks screenshots, lock-screen notification hides preview (`VISIBILITY_SECRET`), unsupported peer blocks send without silent persistent fallback, and `adb shell` inspection of `bitchat_outbox.db` & `bitchat_sync.db` contains zero volatile rows. | **Simulator PASS** (`test_rb2_volatile_enforcement_and_zero_sqlite_artifacts`, `phase6_ux_test`) | **Pending** (Human Physical Run) | Capture via `docs/capture_device_logs.ps1 -Scenario 11` |
| **12** | **Experimental Cover Traffic & Normal-Mode Sync (Default OFF)** | Confirm both switches default to OFF. Enable Cover Traffic on `A` & `B` -> verify `dummy_frames_sent` & `dummy_frames_discarded` increment with zero chat clutter. Enable Sync -> send Normal message, delete message (Tombstone), run Sync (`IdInventory` & `Merkle`), verify deletion is not resurrected and volatile/Privacy messages never sync. | **Simulator PASS** (`test_phase6_cover_traffic_e2e`, `test_phase6_sync_e2e`) | **Pending** (Human Physical Run) | Capture via `docs/capture_device_logs.ps1 -Scenario 12` |

---

## 4. Matched Physical Battery & Energy Telemetry Protocol

> **Note**: Simulator numbers in `docs/EVALUATION_FINAL.md` measure configured byte/work policy proxies (`transmissions`, `wire_bytes_total`, `relays_suppressed`), **not** physical battery percentage savings. Real energy claims require completing the matched protocol below on physical phones.

### Controlled Measurement Procedure
1. **Pre-Run Standardization**:
   - Use the same relay phone (`Phone B`), unplugged from USB (`adb tcpip 5555` or unplug between start/end snapshots), screen brightness fixed at 25% (or screen off with foreground service active), identical distance (`1.5 m`) between `Phone A`, `Phone B`, and `Phone C`, starting between `80%` and `85%` SoC, `ThermalState::Nominal`.
2. **Matched 30-Minute Workload Runs** (3 repetitions per policy: `Help the network`, `Balanced`, `Minimal`, `Receive only`):
   - `Phone A` injects 1 broadcast + 1 routed message every 15 seconds (`120` logical events per 30-minute run).
   - Reset Android batterystats before each 30-minute run:
     ```powershell
     adb shell dumpsys batterystats --reset
     adb shell dumpsys battery
     ```
   - At `t = 30 min`, capture charge counter (`uAh`), voltage (`mV`), temperature (`0.1 C`), UID BLE scan/tx times, and Diagnostics counters (`transit_packets_forwarded`, `transit_bytes_forwarded`, `transit_packets_dropped_policy`):
     ```powershell
     ./docs/capture_device_logs.ps1 -Scenario BatteryRun -Profile Balanced
     ```
3. **Required Reporting Metrics**:
   - Report raw `Charge counter` delta (`ΔuAh`), estimated energy (`mWh`), `transit_bytes_forwarded`, and ambient/battery temperature across all 3 runs per profile. Do not extrapolate coarse `1%–2%` battery-drop integers into unsupported percentage-saving claims.
