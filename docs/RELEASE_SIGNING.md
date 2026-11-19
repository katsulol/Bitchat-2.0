# Bitchat 2.0 — External-Secret Production Release Signing Guide (`Task 11`)

## 1. Security Principle & Fail-Closed Guarantee

`app/android/app/build.gradle.kts` enforces **fail-closed release signing**:
- Production `assembleRelease` / `bundleRelease` builds **refuse to fall back to the Android debug key**.
- If external release signing credentials are missing and `-PallowDebugTestSigning=true` is not explicitly supplied, Gradle aborts immediately with:
  ```text
  FAIL-CLOSED RELEASE SIGNING: Production release build requires external signing secrets ...
  ```
- Signing keys, keystores (`*.jks`, `*.keystore`, `*.p12`), and `app/android/key.properties` are strictly excluded by `.gitignore` and verified absent by the `security-hygiene` CI job (`.github/workflows/ci.yml`).
- Release builds compile with `BuildConfig.ALLOW_DEV_TOPOLOGY_OVERRIDES = false`.

---

## 2. Generating a User-Owned Production Keystore (Offline / Outside Repo)

Run `keytool` (bundled with JDK 17+) in a secure directory **outside** the repository (for example, `C:\Users\<user>\.bitchat-secrets\` or an encrypted hardware token / offline vault):

```powershell
New-Item -ItemType Directory -Force -Path "$HOME\.bitchat-secrets" | Out-Null
keytool -genkeypair -v `
  -keystore "$HOME\.bitchat-secrets\bitchat2-release.jks" `
  -storetype JKS `
  -keyalg RSA -keysize 4096 `
  -validity 10000 `
  -alias bitchat2_release
```

### Mandatory Owner Backup Checklist
1. **Offline Backup**: Copy `bitchat2-release.jks` to at least two independent encrypted offline media (e.g., hardware-encrypted USB + offline password manager vault).
2. **Irreplaceability**: Android package upgrades (`com.bitchat.bitchat2`) verify the signing certificate across versions. Losing the release keystore prevents in-place upgrades without uninstalling and losing local device state.
3. **Never Commit or Upload**: Never place `.jks` files, base64 keystores, or passphrases inside git commits, public CI artifacts, or release notes.

---

## 3. Supplying Signing Secrets at Build Time

### Option A — Environment Variables (Recommended for CI & Automated Release Hosts)
```powershell
$env:BITCHAT_KEYSTORE_PATH = "$HOME\.bitchat-secrets\bitchat2-release.jks"
$env:BITCHAT_KEYSTORE_PASSWORD = "<your-store-password>"
$env:BITCHAT_KEY_ALIAS = "bitchat2_release"
$env:BITCHAT_KEY_PASSWORD = "<your-key-password>"

Push-Location app
flutter build apk --release
Pop-Location
```
This embeds `BuildConfig.SIGNING_PROVENANCE = "EXTERNAL_RELEASE_KEY"` and `BuildConfig.ALLOW_DEV_TOPOLOGY_OVERRIDES = false`.

### Option B — Untracked `app/android/key.properties` (Local Owner Workstation)
Create `app/android/key.properties` (gitignored):
```properties
storeFile=C:/Users/<user>/.bitchat-secrets/bitchat2-release.jks
storePassword=<your-store-password>
keyAlias=bitchat2_release
keyPassword=<your-key-password>
```
Verify git ignores it before building:
```powershell
git status --short
```

---

## 4. Building Labeled Non-Production Test APKs (Local Verification / CI)

When verifying release shrink/AOT packaging on developer devices or CI runners without production signing keys, pass the explicit opt-in property `-PallowDebugTestSigning=true`:

```powershell
Push-Location app
flutter build apk --release -PallowDebugTestSigning=true
Pop-Location
```

This stamps `BuildConfig.SIGNING_PROVENANCE = "DEBUG_TEST_KEY_NOT_FOR_PRODUCTION"` and `BuildConfig.ALLOW_DEV_TOPOLOGY_OVERRIDES = false` so test artifacts are unambiguously distinguished from production-signed releases.
