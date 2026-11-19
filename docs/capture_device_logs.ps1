param(
    [string]$Scenario = "General",
    [string]$Profile = "Balanced",
    [string]$OutDir = "docs/device_logs"
)

$ErrorActionPreference = "Continue"
$ts = Get-Date -Format "yyyyMMdd_HHmmss"
$runDir = Join-Path $OutDir "${Scenario}_${Profile}_${ts}"
New-Item -ItemType Directory -Force -Path $runDir | Out-Null

Write-Output "=== Bitchat 2.0 Physical Verification Log & Telemetry Capture ==="
Write-Output "Scenario: $Scenario | Profile: $Profile | Output: $runDir"

$devicesRaw = & adb devices 2>&1
$serials = @()
foreach ($line in $devicesRaw) {
    if ($line -match "^(\S+)\s+device$") {
        $serials += $Matches[1]
    }
}

if ($serials.Count -eq 0) {
    Write-Warning "No physical Android devices detected via 'adb devices'. Physical tests remain Pending."
    "STATUS: PENDING (No physical adb devices attached at $ts)" | Set-Content (Join-Path $runDir "status.txt")
    exit 0
}

$idx = 0
foreach ($s in $serials) {
    $idx++
    $label = "Device_${idx}"
    $devDir = Join-Path $runDir $label
    New-Item -ItemType Directory -Force -Path $devDir | Out-Null

    $model = (& adb -s $s shell getprop ro.product.model 2>&1 | Out-String).Trim()
    $sdk = (& adb -s $s shell getprop ro.build.version.sdk 2>&1 | Out-String).Trim()
    $rel = (& adb -s $s shell getprop ro.build.version.release 2>&1 | Out-String).Trim()

    "Redacted_ID: $label`nModel: $model`nAndroid_Release: $rel (SDK $sdk)`nCaptured_At: $ts" |
        Set-Content (Join-Path $devDir "device_meta_redacted.txt")

    # Capture battery charge counter (uAh), voltage, and thermal snapshot
    & adb -s $s shell dumpsys battery > (Join-Path $devDir "battery_snapshot.txt") 2>&1

    # Capture Bitchat service & MainActivity logcat lines, redacting any device serials
    $rawLogs = & adb -s $s logcat -d -v time -s BitchatBleService:I MainActivity:I AndroidKeystoreProvider:I flutter:I 2>&1 | Out-String
    $redactedLogs = $rawLogs.Replace($s, $label)
    $redactedLogs | Set-Content (Join-Path $devDir "bitchat_logcat_redacted.log")

    Write-Output "Captured redacted telemetry for $label ($model, Android $rel) -> $devDir"
}
