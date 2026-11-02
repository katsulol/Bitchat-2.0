# Bitchat 2.0 Resource-Budgeted Relaying Policy (`RESOURCE_POLICY.md`)

## 1. Overview & Honest Claims Boundary

`core/src/mesh/resource.rs` implements local device-resource budgeting for optional mesh transit forwarding.

- **Workload Control, Not Claimed Battery Percentage**: The relative transit byte budgets (`1:4:8` across `Minimal`, `Balanced`, and `Assisted`) are experimental policy parameters that bound radio/CPU forwarding workload under low battery or thermal stress. They are **not** claimed as measured mAh battery savings until physical matched-duration hardware energy tests are executed in Part B.
- **No Raw Telemetry Broadcast**: Exact battery percentage, device model, charging state, and thermal telemetry are local platform hints (`ResourceSnapshot`) and are **never** broadcast on the mesh or Iroh wire.

---

## 2. Resource Profiles & 1:4:8 Byte Token Buckets

| Profile | Relative Ratio | Transit Byte Budget | Per-Neighbor Cap (50%) | Max Transit Queue | Relay Delay Multiplier |
| :--- | :---: | :---: | :---: | :---: | :---: |
| **`Paused`** | `0x` | `0 B/s` | `0 B/s` | `0` | N/A (Transit disabled) |
| **`Minimal`** | `1x` | `8 KiB/s` (`8,192 B/s`) | `4 KiB/s` | `16` | `1.50x` (`15,000 bps`) |
| **`Balanced`** | `4x` | `32 KiB/s` (`32,768 B/s`) | `16 KiB/s` | `64` | `1.00x` (`10,000 bps`) |
| **`Assisted`** | `8x` | `64 KiB/s` (`65,536 B/s`) | `32 KiB/s` | `128` | `0.75x` (`7,500 bps`) |

---

## 3. Precedence, Safety Overrides & Hysteresis

1. **Immediate Safety Overrides (`Paused`)**:
   - `user_paused_relaying == true`, `!ble_ready`, or `ThermalState::Critical` immediately transitions the node to `RelayResourceProfile::Paused`, overriding charging state and any user profile override.
2. **Serious Thermal / Power-Save Cap (`Minimal`)**:
   - `ThermalState::Serious`, `power_save_mode == TriState::True`, or `BatteryBand::Critical` caps the active profile at `RelayResourceProfile::Minimal`, even if the device is plugged in.
3. **WiFi $\neq$ Free Relaying**:
   - `NetworkMetering::Unmetered` alone never elevates a node to `Assisted`. `Assisted` requires `charging == TriState::True`, `battery_band == BatteryBand::High`, `thermal_state == ThermalState::Nominal`, and `power_save_mode == TriState::False`.
4. **Upgrade Hysteresis (`min_upgrade_dwell`)**:
   - While safety downgrades take effect immediately (`0 ms` delay), profile upgrades (`Paused -> Minimal -> Balanced -> Assisted`) require `min_upgrade_dwell` (default `10s`) to have elapsed since the last transition, preventing rapid oscillation near battery or thermal thresholds.

---

## 4. Originator Protection & Control-Plane Reservation

- **Originator vs. Transit Separation (`TrafficOrigin`)**:
  - `TrafficOrigin::LocalOriginated` packets are **never** dropped by `ResourceBudgetManager::check_and_record`, even when `active_profile == RelayResourceProfile::Paused`. Local accepted messages are never sacrificed to fund or conserve transit forwarding.
- **Control-Plane Reservation (`25%`)**:
  - Aligned with `core/src/gateway/fairness.rs` (`CONTROL_PLANE_RESERVATION_RATIO = 0.25`), non-control `MsgType::Chat` transit traffic may consume at most `75%` of the active profile's queue depth and byte budget (`6 KiB/s` in `Minimal`), reserving the remaining `25%` (`2 KiB/s` in `Minimal`) for `MsgType::Handshake` and `MsgType::Ack` frames so E2E session setup and delivery receipts cannot be starved by transit chat bursts.
