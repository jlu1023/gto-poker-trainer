#!/usr/bin/env python3
"""
Desktop packaging gate: the Tauri app owns the server lifecycle, end to end.
Run with: C:\\Users\\james\\AppData\\Local\\Programs\\Python\\Python312\\python.exe verify_desktop.py

Asserts, in order (mirroring the P2 acceptance demos, scripted so they stay true):
  1. Pre-flight: no LISTENER on 7474 (TIME_WAIT remnants are kernel-owned and tolerated;
     a listener means a live server or squatter - the gate reports ownership and FAILS,
     it never kills what it did not start). The built app exe and the PROVISIONED
     sidecar (gto-server.exe beside it) must exist.
  2. Cold launch with no terminals: the sidecar becomes healthy on loopback 7474.
  3. The packaged server found the REAL matrix: POST /api/solve for a registry
     EXPECTED_CACHED board must return "cached":true (an empty-cache cold start would
     stream a fresh solve instead - the amendment-1 failure this proves absent).
  4. The window opened MAXIMISED: Win32 GetWindowPlacement showCmd == SW_SHOWMAXIMIZED,
     asserted, not assumed (tauri#11554).
  5. Teardown: graceful WM_CLOSE -> the app exits, ZERO gto-server processes remain,
     and NO listener remains on 7474. The teardown-gap acceptance, proven empirically.
"""

import ctypes
import ctypes.wintypes as wt
import json
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

import verifier_boards

REPO = Path(__file__).parent
APP_EXE = REPO / "desktop" / "src-tauri" / "target" / "release" / "gto-lab-desktop.exe"
SIDECAR = APP_EXE.parent / "gto-server.exe"
SERVER_URL = "http://127.0.0.1:7474"
SW_SHOWMAXIMIZED = 3

CACHED_BOARD = "Ks7h2c"  # EXPECTED_CACHED, solved in the srp-v2 benchmark at 100bb


def listener_pids():
    """PIDs LISTENING on 7474 (TIME_WAIT etc. deliberately excluded)."""
    out = subprocess.run(
        ["netstat", "-ano", "-p", "TCP"], capture_output=True, text=True
    ).stdout
    pids = set()
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 5 and parts[1].endswith(":7474") and parts[3] == "LISTENING":
            pids.add(int(parts[4]))
    return pids


def procs_named(name):
    out = subprocess.run(
        ["tasklist", "/FI", f"IMAGENAME eq {name}", "/FO", "CSV", "/NH"],
        capture_output=True, text=True,
    ).stdout
    pids = []
    for line in out.splitlines():
        if line.startswith(f'"{name}"'):
            pids.append(int(line.split('","')[1]))
    return pids


def health_ok(timeout=1.5):
    try:
        with urllib.request.urlopen(f"{SERVER_URL}/api/health", timeout=timeout) as r:
            return r.status == 200
    except Exception:
        return False


def window_show_cmd(pid):
    """showCmd of the main visible window owned by pid, via EnumWindows +
    GetWindowPlacement (the title-bar window, not webview children)."""
    user32 = ctypes.windll.user32

    class WINDOWPLACEMENT(ctypes.Structure):
        _fields_ = [
            ("length", wt.UINT), ("flags", wt.UINT), ("showCmd", wt.UINT),
            ("ptMinPosition", wt.POINT), ("ptMaxPosition", wt.POINT),
            ("rcNormalPosition", wt.RECT),
        ]

    found = []

    @ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)
    def enum_cb(hwnd, _):
        owner_pid = wt.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner_pid))
        if owner_pid.value == pid and user32.IsWindowVisible(hwnd):
            wp = WINDOWPLACEMENT()
            wp.length = ctypes.sizeof(WINDOWPLACEMENT)
            if user32.GetWindowPlacement(hwnd, ctypes.byref(wp)):
                found.append(wp.showCmd)
        return True

    user32.EnumWindows(enum_cb, 0)
    return found


def main():
    print("=== Desktop packaging gate ===\n")
    verifier_boards.check_partitions()
    ok = True
    app = None
    launched_app_pid = None

    # ---- 1. pre-flight ----
    print("=== 1. Pre-flight ===")
    squatters = listener_pids()
    if squatters:
        print(f"FAIL  7474 already has listener(s) {sorted(squatters)}; this gate never "
              f"kills what it did not start - close them and rerun")
        sys.exit(1)
    print("PASS  no listener on 7474")
    if not APP_EXE.exists():
        print(f"FAIL  app exe missing: {APP_EXE} (build: cargo build --release in "
              f"desktop/src-tauri)")
        sys.exit(1)
    if not SIDECAR.exists():
        print(f"FAIL  provisioned sidecar missing: {SIDECAR} (run "
              f"desktop/provision-sidecar.ps1 after building gto-server)")
        sys.exit(1)
    src_server = REPO / "target" / "release" / "gto-server.exe"
    if src_server.exists():
        drift = src_server.stat().st_mtime - SIDECAR.stat().st_mtime
        if drift > 1.0:
            print(f"FAIL  provisioned sidecar is STALE: target/release/gto-server.exe is "
                  f"{drift:.0f}s newer (the amendment-2 foot-gun) - re-run "
                  f"desktop/provision-sidecar.ps1")
            sys.exit(1)
    print("PASS  app + provisioned sidecar present (sidecar not stale vs target/release)")
    pre_servers = set(procs_named("gto-server.exe"))

    try:
        # ---- 2. cold launch ----
        print("\n=== 2. Cold launch (no terminals) ===")
        app = subprocess.Popen([str(APP_EXE)])
        launched_app_pid = app.pid
        deadline = time.time() + 15
        while time.time() < deadline and not health_ok():
            time.sleep(0.3)
        if health_ok():
            print(f"PASS  sidecar healthy on {SERVER_URL} within budget")
        else:
            print("FAIL  sidecar never became healthy (15s budget)")
            ok = False

        # ---- 3. the packaged server found the real matrix ----
        print("\n=== 3. Cached benchmark board (matrix resolution) ===")
        key = verifier_boards.spot_key(100, CACHED_BOARD)
        body = json.dumps(
            {"board": [CACHED_BOARD[0:2], CACHED_BOARD[2:4], CACHED_BOARD[4:6]],
             "depth": 100, "pot_type": "srp"}
        ).encode()
        req = urllib.request.Request(
            f"{SERVER_URL}/api/solve", data=body,
            headers={"Content-Type": "application/json"}, method="POST")
        t0 = time.time()
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                stream = r.read().decode("utf-8", "replace")
            dt = time.time() - t0
            if '"cached":true' in stream:
                print(f"PASS  {key} served cached:true in {dt:.1f}s (real matrix found; "
                      f"no empty-cache cold start)")
            else:
                print(f"FAIL  {key} was NOT a cache hit ({dt:.1f}s) - the packaged server "
                      f"did not find the existing matrix (amendment-1 class failure)")
                ok = False
        except Exception as e:
            print(f"FAIL  solve request failed: {e}")
            ok = False

        # ---- 4. maximised window, asserted ----
        print("\n=== 4. Window state ===")
        cmds = window_show_cmd(app.pid)
        if SW_SHOWMAXIMIZED in cmds:
            print(f"PASS  GetWindowPlacement showCmd == {SW_SHOWMAXIMIZED} "
                  f"(SW_SHOWMAXIMIZED); window opened maximised")
        else:
            print(f"FAIL  window showCmd values {cmds} lack SW_SHOWMAXIMIZED")
            ok = False

        # ---- 5. teardown: the server dies with the app ----
        print("\n=== 5. Teardown (the load-bearing acceptance) ===")
        subprocess.run(["taskkill", "/PID", str(app.pid)], capture_output=True)
        deadline = time.time() + 10
        while time.time() < deadline and app.poll() is None:
            time.sleep(0.3)
        if app.poll() is None:
            print("FAIL  app did not exit within 10s of WM_CLOSE")
            ok = False
        else:
            print("PASS  app exited on graceful WM_CLOSE")
        app = None
        time.sleep(1.5)
        survivors = [p for p in procs_named("gto-server.exe") if p not in pre_servers]
        listeners = listener_pids()
        if not survivors:
            print("PASS  zero gto-server survivor processes")
        else:
            print(f"FAIL  gto-server survivor(s): {survivors} - the teardown gap is OPEN")
            ok = False
        if not listeners:
            print("PASS  no listener on 7474 (kernel TIME_WAIT remnants tolerated)")
        else:
            print(f"FAIL  listener(s) still on 7474: {sorted(listeners)}")
            ok = False
    finally:
        # the gate kills ONLY what it launched
        if app is not None and app.poll() is None:
            app.kill()
        if launched_app_pid is not None:
            for pid in procs_named("gto-server.exe"):
                if pid not in pre_servers:
                    subprocess.run(["taskkill", "/F", "/PID", str(pid)],
                                   capture_output=True)

    print("\nGATE PASSED" if ok else "\nGATE FAILED")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
