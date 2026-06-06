# Provision the gto-server sidecar for the desktop app.
#
# Copies the freshly built engine-workspace server into the two places the desktop app
# can run it from:
#   1. desktop/src-tauri/binaries/gto-server-x86_64-pc-windows-msvc.exe
#      (the target-triple name the Tauri CLI's externalBin handling expects for
#       `tauri dev` / bundled flows)
#   2. desktop/src-tauri/target/release/gto-server.exe
#      (beside the bare-exe artefact, which is the personal-use deliverable with
#       bundle.active = false)
#
# THE FOOT-GUN THIS SCRIPT GUARDS (run it or step on it): after ANY gto-server rebuild,
# re-run this script - otherwise the desktop app runs the STALE binary while the gates
# (which test target/release directly) pass against the fresh one.
#
# Run from anywhere; paths resolve relative to this script.

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
$src = Join-Path $repo "target\release\gto-server.exe"
if (-not (Test-Path $src)) {
    Write-Error "no built server at $src - run: cargo build --release -p gto-server"
}

$binDir = Join-Path $PSScriptRoot "src-tauri\binaries"
New-Item -ItemType Directory -Force $binDir | Out-Null
Copy-Item $src (Join-Path $binDir "gto-server-x86_64-pc-windows-msvc.exe") -Force

$exeDir = Join-Path $PSScriptRoot "src-tauri\target\release"
if (Test-Path $exeDir) {
    Copy-Item $src (Join-Path $exeDir "gto-server.exe") -Force
}

$stamp = (Get-Item $src).LastWriteTime
Write-Host "sidecar provisioned from $src (built $stamp)"
