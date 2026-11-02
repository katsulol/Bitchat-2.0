# Coarse Proximity Classification & Privacy Limitations (`PROXIMITY_LIMITATIONS.md`)

## 1. Purpose & Non-Goals

`core/src/mesh/proximity.rs` provides a coarse, local-only signal strength indicator (`ProximityCategory`: `Nearby`, `Further`, `Unknown`) for direct BLE links.

- **Not Distance Measurement or Ranging**: BLE Received Signal Strength Indication (RSSI) varies by $15\text{–}30\text{ dB}$ due to hand grip, phone orientation, antenna design, human body absorption, multipath reflections, and walls. Bitchat 2.0 **never** displays distance in meters and never claims physical ranging accuracy.
- **Zero Security or Routing Authority**: Proximity categories are strictly UI informational hints. They are **never** used to authenticate a peer, auto-approve a gateway, elevate a voucher, or alter routing decisions.

---

## 2. Classification Algorithm (`ProximityTracker`)

1. **Direct-Link-Only Filtering (`hop_count <= 1`)**:
   - Multi-hop relayed frames (`hop_count > 1`) are ignored by `record_direct_link_rssi`. A nearby relay node forwarding a remote sender's packet can never make the remote sender appear `Nearby`.
2. **Verified Peer Binding (`bind_verified_peer`)**:
   - Link RSSI observations remain unattributed until the link is bound to a cryptographically verified `PeerId`.
3. **Outlier Rejection & Median Smoothing**:
   - Implausible hardware readings outside `[-110, -20] dBm` are discarded.
   - Each link retains at most `8` recent samples (`DEFAULT_MAX_SAMPLES_PER_LINK = 8`) across at most `64` tracked links (`MAX_TRACKED_LINKS = 64`).
   - Classification requires at least `3` non-stale samples (`DEFAULT_MIN_SAMPLES_REQUIRED = 3`) within the `15s` freshness window (`DEFAULT_SAMPLE_TTL = 15s`) and computes the **median** RSSI to resist single-packet multipath spikes.
4. **8 dB Hysteresis Deadband**:
   - Enters `Nearby` when median RSSI $\ge -65\text{ dBm}$ (`DEFAULT_NEARBY_ENTER_DBM`).
   - Remains `Nearby` until median RSSI drops $\le -73\text{ dBm}$ (`DEFAULT_NEARBY_EXIT_DBM`), preventing UI flapping near the boundary.
   - Resets to `Unknown` when samples expire (`> 15s`) or immediately upon link disconnect (`on_link_disconnected`).

---

## 3. Privacy & Tracking Limitations

- **Zero Persistence / Zero Export**: RSSI samples are never written to SQLite/disk, never exported over Iroh gateways, and never combined with GPS or location services.
- **RF Linkability Disclosure**: Coarsening UI display to `Nearby`/`Further`/`Unknown` prevents misleading the local user, but does **not** prevent an external RF observer with directional antennas from tracking stable BLE MAC addresses or `ShortId` beacons over the air. Synchronized MAC/ShortId rotation requires a separate link-layer protocol design and is outside Phase 5 scope.
