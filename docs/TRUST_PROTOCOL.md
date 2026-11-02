# Bitchat 2.0 Scoped Trust & One-Hop Voucher Protocol (`TRUST_PROTOCOL.md`)

## 1. Security Model & Non-Goals

Phase 5 introduces explicit capability-based local authorization (`core/src/trust/model.rs`, `core/src/trust/policy.rs`) and bounded one-hop Ed25519 vouchers (`core/src/trust/voucher.rs`, `core/src/trust/store.rs`).

- **Key Ownership $\neq$ Personal or Gateway Trust**: Cryptographic verification (`Noise_XX_25519_ChaChaPoly_BLAKE2s` or signed `IdentityBinding`) proves control of a 32-byte Ed25519 `PeerId`. It transitions an unknown peer to `IdentityVerifiedPendingConsent` and **never** automatically grants `Approved` gateway forwarding or introducer privileges.
- **What Vouching Claims (and Does Not Claim)**: One-hop vouchers (`SignedVoucher`) provide local introduction-based admission and bounded abuse resistance when signed by a locally approved introducer (`Capability::ActAsIntroducer`). They **do not** prove unique human identity, do not form a global transitive Web-of-Trust, and do not provide universal Sybil resistance against a malicious or compromised introducer.

---

## 2. Scoped Capabilities (`core/src/trust/model.rs`)

| Capability (`#[repr(u8)]`) | Code | Delegable via One-Hop Voucher? | Description |
| :--- | :---: | :---: | :--- |
| `ReceiveDirectMessage` | `1` | **Yes** | Bypass initial pending-request queue limits for E2E chat. |
| `ActAsIntroducer` | `2` | **No** (Local consent only) | Authorized to sign one-hop `SignedVoucher` introductions accepted by this node. |
| `UseGatewayForForwarding` | `3` | **Policy-gated** (`allow_vouched_gateway_forwarding`) | Authorized to forward addressed mesh packets through this node's internet gateway bridge. |
| `PermitOutboundGateway` | `4` | **No** (Local consent only) | Approved by the local user as a trusted outbound gateway next hop. |
| `IssueIntroduction` | `5` | **No** (Local consent only) | Local capability to sign an introduction for another peer. |

### Precedence Rules (`TrustPolicy::authorize`)
1. **Local `Blocked`** (`PeerTrustState::Blocked`) overrides all capabilities, pairing invitations, and vouchers (`AuthOutcome::Deny`, `AuthReason::LocalBlocked`).
2. **Gateway Opt-In Check**: `UseGatewayForForwarding` requires `ctx.gateway_opted_in == true`.
3. **Explicit Local `Approved`**: Grants capabilities explicitly present in the peer's approved capability set (`AuthReason::ExplicitlyApproved`).
4. **One-Hop Voucher**: Grants `ReceiveDirectMessage` (or `UseGatewayForForwarding` when `allow_vouched_gateway_forwarding` is enabled) if a non-expired, non-revoked `SignedVoucher` from a currently approved introducer is present (`AuthReason::ValidOneHopVoucher`). If wall-clock validity is unknown (`ctx.now_unix == None`), automatic voucher elevation is refused (`AuthReason::ClockValidityUnknown`).
5. **Local `Revoked`**: Denies elevated capabilities (`AuthReason::LocalRevoked`).
6. **Default Message Requests**: Unrecognized or pending peers may submit bounded message requests (`AuthOutcome::Pending`) up to `max_pending_requests` (`32`).

---

## 3. Canonical Binary Wire Formats (`core/src/trust/voucher.rs`)

### 3.1 `SignedVoucher` (198 Bytes Fixed Binary)
- **Signing Domain**: `b"bitchat2/vouch/v1"` (17 bytes)
- **Wire Layout**:
  - `version: u8` (`1`, offset `0`)
  - `issuer: [u8; 32]` (full Ed25519 public key, offsets `1..33`)
  - `subject: [u8; 32]` (full Ed25519 public key, offsets `33..65`)
  - `audience: [u8; 32]` (intended verifier `PeerId`, offsets `65..97`)
  - `capability: u8` (`1` or `3`, offset `97`)
  - `credential_id: [u8; 16]` (CSPRNG random ID, offsets `98..114`)
  - `issued_at_unix: u64` (big-endian, offsets `114..122`)
  - `expires_at_unix: u64` (big-endian, offsets `122..130`, max lifetime `7 days = 604,800s`)
  - `revocation_seq: u32` (big-endian, offsets `130..134`)
  - `signature: [u8; 64]` (Ed25519 signature over `VOUCH_DOMAIN || bytes[0..134]`, offsets `134..198`)

### 3.2 `SignedVoucherRevocation` (189 Bytes Fixed Binary)
- **Signing Domain**: `b"bitchat2/vouch-revoke/v1"` (24 bytes)
- **Wire Layout**:
  - `version: u8` (`1`, offset `0`)
  - `issuer: [u8; 32]` (offsets `1..33`)
  - `subject: [u8; 32]` (offsets `33..65`)
  - `audience: [u8; 32]` (offsets `65..97`)
  - `credential_id: [u8; 16]` (offsets `97..113`)
  - `revocation_seq: u32` (big-endian, offsets `113..117`)
  - `revoked_at_unix: u64` (big-endian, offsets `117..125`)
  - `signature: [u8; 64]` (Ed25519 signature over `VOUCH_REVOKE_DOMAIN || bytes[0..125]`, offsets `125..189`)

---

## 4. Quotas, Dual-Clock Expiry, and Revocation Limitations

- **Default Quotas (`TrustQuotaConfig`)**:
  - Max approved introducers: `64`
  - Max active credentials: `1024`
  - Max import batch size: `16 KiB` (`16,384` bytes, checked before decoding/verifying)
  - Max vouchers per introducer per hour: `10`
  - Max pending consent requests: `32`
  - Max cached revocations: `2048`
- **Clock Rollback Protection**: Upon import, each voucher is assigned a monotonic deadline `expires_at_monotonic`. Once expired by either monotonic time or wall-clock time, `permanently_expired` is latched to `true` so a subsequent system clock rollback cannot resurrect an expired credential.
- **Offline Revocation Limitation**: Revocations are cached locally upon receipt (including out-of-order arrival before the voucher itself), and revoking an introducer immediately purges all vouchers issued by that introducer. However, an offline node cannot learn of a remote issuer's revocation until it reconnects and receives the `SignedVoucherRevocation`.
- **No Automatic Issuer Punishment**: Blocking an introduced `subject` immediately revokes that subject's capabilities and vouchers, but never automatically revokes or penalizes the `issuer` (preventing false-flag framing of honest introducers).
