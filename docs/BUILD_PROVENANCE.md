# Bitchat 2.0 — Build Provenance, CI Pipeline & Artifact Ledger (`Task 11`)

## 1. Provenance Metadata Contract

Compile-time provenance is injected by `core/build.rs` and surfaced via `bitchat_build_info()`, `bitchat_abi_version()`, `bitchat_check_abi_version(2)`, and `bitchat_get_diagnostics()`:

| Provenance Field | Value / Contract |
|---|---|
| `CORE_BUILD_VERSION` | `"v2.0.6-phase6"` |
| `CORE_ABI_VERSION` | `2` (`CoreFfi.expectedAbiVersion = 2`) |
| `app/pubspec.yaml` version | `2.0.6+6` (`versionName = 2.0.6`, `versionCode = 6`) |
| `BITCHAT_GIT_COMMIT` | 12-char `git rev-parse --short=12 HEAD` embedded at compile time |
| `BITCHAT_BUILD_DIRTY` | `"true"` if tracked files modified at build time, `"false"` on clean commit |
| `BITCHAT_BUILD_TARGET` | `"aarch64-linux-android"` (Android ARM64) / `"x86_64-pc-windows-msvc"` (Host tests) |
| `BuildConfig.ALLOW_DEV_TOPOLOGY_OVERRIDES` | `false` in `release`, `true` in `debug` |
| `BuildConfig.SIGNING_PROVENANCE` | `"EXTERNAL_RELEASE_KEY"` (with secrets) or `"DEBUG_TEST_KEY_NOT_FOR_PRODUCTION"` (with `-PallowDebugTestSigning=true`) |

### Incompatible / Stale Native Library Detection
- **Rust FFI (`core/tests/test_phase6_ffi_lifecycle.rs`)**: `bitchat_check_abi_version(2) == 0`, `bitchat_check_abi_version(1) == -1`, `bitchat_check_abi_version(79) == -1` (populates `bitchat_last_error()` with `"ABI_MISMATCH: host expects ABI v..., native library provides ABI v2..."`).
- **Flutter UI (`app/lib/core_ffi.dart`, `app/test/phase6_ux_test.dart` Test 1)**: `CoreFfi.loadLibrary()` checks `_checkAbiVersion(2) == 0` before initializing encrypted SQLite databases and displays the fail-closed `Native ABI mismatch (fail-closed)` banner on mismatch.

---

## 2. Verified Build Artifacts & SHA-256 Hashes (Task 11 Build at Base Commit `35629c0`)

| Artifact | Path | Size (Bytes) | SHA-256 Checksum |
|---|---|---:|---|
| Native ARM64 Library | `app/android/app/src/main/jniLibs/arm64-v8a/libbitchat2_core.so` | `25,426,160` | `DA4787A18BB51034B4538308C0CA8FF8FC91D9AA639189FAB0628DEC13BB6473` |
| Labeled Release Test APK | `app/build/app/outputs/flutter-apk/app-release.apk` | `84,132,881` | `3BFF46D8A6A3FEB55550E8A9CB9B34791979F631E7144BEAD11F5AEAFFECCAC4` |

> **Note**: In Task 15 (final release handoff), `libbitchat2_core.so` and `app-release.apk` are rebuilt one final time at the final release commit and recorded in `FINAL_REPORT_AND_HANDOFF.md`.

---

## 3. Reproducible Build & Verification Commands

### 3.1 Workspace Rust Format, Lint, Property & E2E Tests
```powershell
cargo fmt --manifest-path core/Cargo.toml -- --check
cargo clippy --manifest-path core/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path core/Cargo.toml --lib proptest
cargo test --manifest-path core/Cargo.toml --all --quiet
```

### 3.2 Native ARM64 Library Build (`aarch64-linux-android`, NDK `30.0.16248370`)
```powershell
cargo build --manifest-path core/Cargo.toml --target aarch64-linux-android --release
Copy-Item "C:\cargo-target\aarch64-linux-android\release\libbitchat2_core.so" `
          "app\android\app\src\main\jniLibs\arm64-v8a\libbitchat2_core.so" -Force
Get-FileHash "app\android\app\src\main\jniLibs\arm64-v8a\libbitchat2_core.so" -Algorithm SHA256
```

### 3.3 Flutter Analysis, Widget Tests & APK Build
```powershell
Push-Location app
flutter analyze
flutter test

# Verify fail-closed production release signing (exits non-zero without external secrets):
flutter build apk --release

# Build labeled non-production test APK:
flutter build apk --release -PallowDebugTestSigning=true
Get-FileHash "build\app\outputs\flutter-apk\app-release.apk" -Algorithm SHA256
Pop-Location
```

---

## 4. CI Workflow Summary (`.github/workflows/ci.yml`)

1. **`rust-core`**: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `--lib --bins`, `proptest` parser fuzz smoke checks, `--test "*"`, bounded 300s network/gateway E2E step (`test_phase6_trust_persistence_e2e`, `test_phase6_release_blockers_e2e`), and sanitized `cargo metadata` dependency/license artifact.
2. **`security-hygiene`**: Fails if any `.jks`, `.keystore`, `.p12`, `.key`, `key.properties`, or `bitchat_release_secrets*` file is tracked in git; verifies `ALLOW_DEV_TOPOLOGY_OVERRIDES = false` and `FAIL-CLOSED RELEASE SIGNING` in `app/android/app/build.gradle.kts`.
3. **`flutter-app`**: `flutter pub get`, `flutter analyze`, `flutter test`.
4. **`android-provenance`**: Builds `arm64-v8a` `libbitchat2_core.so` via `cargo-ndk`, packages `app-release.apk` with `-PallowDebugTestSigning=true`, and uploads a sanitized `PROVENANCE_MANIFEST.json` containing only commit & SHA-256 hashes (zero keys, identities, serials, or message bodies).
