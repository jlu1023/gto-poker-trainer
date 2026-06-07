# Provision the gto-server binary for the desktop app.
#
# The app spawns the server with a raw std::process::Command (see src-tauri/src/main.rs),
# NOT a Tauri externalBin sidecar: there is no externalBin entry and bundle.active = false.
# main.rs resolves the server as `gto-server.exe` BESIDE the app exe, so this script copies
# the freshly built engine-workspace server to exactly one place:
#   desktop/src-tauri/target/release/gto-server.exe
#
# (An earlier version also copied a triple-suffixed gto-server-x86_64-pc-windows-msvc.exe
# into src-tauri/binaries/ for a Tauri externalBin flow that was never wired - nothing
# consumed it. Removed, and the stale binaries/ copy is cleaned up below.)
#
# THE FOOT-GUN THIS SCRIPT GUARDS (run it or step on it): after ANY gto-server rebuild,
# re-run this script - otherwise the desktop app runs the STALE binary while the gates
# (which test target/release directly) pass against the fresh one. build-desktop.ps1 runs
# this for you on every build, which is the structural fix; verify_desktop's pre-flight is
# the independent drift tripwire.
#
# Run from anywhere; paths resolve relative to this script.

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
$src = Join-Path $repo "target\release\gto-server.exe"
if (-not (Test-Path $src)) {
    Write-Error "no built server at $src - run: cargo build --release -p gto-server"
}

$exeDir = Join-Path $PSScriptRoot "src-tauri\target\release"
New-Item -ItemType Directory -Force $exeDir | Out-Null
Copy-Item $src (Join-Path $exeDir "gto-server.exe") -Force

# clean up the orphaned externalBin copy from the earlier version, if present
$staleBin = Join-Path $PSScriptRoot "src-tauri\binaries\gto-server-x86_64-pc-windows-msvc.exe"
if (Test-Path $staleBin) {
    Remove-Item $staleBin -Force
    Write-Host "removed orphaned $staleBin (nothing consumed it)"
}

$stamp = (Get-Item $src).LastWriteTime
Write-Host "server provisioned beside the app exe from $src (built $stamp)"
