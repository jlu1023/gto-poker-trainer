# THE canonical build for the GTO Lab desktop app. Use this, not a bare
# `cargo build --release` in src-tauri (that produces a dev-mode binary whose webview
# points at http://localhost:5173 and shows ERR_CONNECTION_REFUSED on a bare double-click).
#
# Does, in order:
#   1. Build the viz frontend (viz/dist) - the bundle the app EMBEDS at compile time.
#   2. Build gto-server (engine workspace, release).
#   3. Provision it beside the app exe (provision-sidecar.ps1) - closes the stale-binary
#      foot-gun structurally: every build re-provisions.
#   4. Build the app with --features custom-protocol, so generate_context! EMBEDS viz/dist
#      instead of resolving devUrl. This is the one flag that distinguishes a real artefact
#      from a dev-mode binary.
#
# Run from anywhere; paths resolve relative to this script. Pass -SkipViz to reuse the
# existing viz/dist (faster rebuilds when only the Rust shell changed).

[CmdletBinding()]
param([switch]$SkipViz)

$ErrorActionPreference = "Stop"
$desktop = $PSScriptRoot
$repo = Split-Path -Parent $desktop
$srcTauri = Join-Path $desktop "src-tauri"

function Step($n, $msg) { Write-Host "`n[$n] $msg" -ForegroundColor Cyan }

# ---- 1. viz frontend (the embedded bundle) ----
if ($SkipViz) {
    if (-not (Test-Path (Join-Path $repo "viz\dist\index.html"))) {
        Write-Error "-SkipViz given but viz/dist/index.html is missing - run without -SkipViz"
    }
    Step 1 "viz build SKIPPED (reusing existing viz/dist)"
} else {
    Step 1 "building viz frontend (npm run build) -> viz/dist"
    npm --prefix (Join-Path $repo "viz") run build
    if ($LASTEXITCODE -ne 0) { Write-Error "viz build failed" }
}

# ---- 2. gto-server (engine workspace, release) ----
Step 2 "building gto-server (cargo build --release -p gto-server)"
cargo build --release -p gto-server --manifest-path (Join-Path $repo "Cargo.toml")
if ($LASTEXITCODE -ne 0) { Write-Error "gto-server build failed" }

# ---- 3. provision beside the app exe ----
Step 3 "provisioning gto-server beside the app exe"
& (Join-Path $desktop "provision-sidecar.ps1")

# ---- 4. the app, WITH the production feature ----
Step 4 "building gto-lab-desktop (cargo build --release --features custom-protocol)"
cargo build --release --features custom-protocol --manifest-path (Join-Path $srcTauri "Cargo.toml")
if ($LASTEXITCODE -ne 0) { Write-Error "desktop app build failed" }

# ---- artefacts ----
$appExe = Join-Path $srcTauri "target\release\gto-lab-desktop.exe"
$sidecar = Join-Path $srcTauri "target\release\gto-server.exe"
Write-Host "`nBuilt artefacts:" -ForegroundColor Green
Get-Item $appExe, $sidecar | Select-Object FullName, Length, LastWriteTime | Format-Table -AutoSize
Write-Host "Launch: $appExe" -ForegroundColor Green
