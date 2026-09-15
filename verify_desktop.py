#!/usr/bin/env python3
"""
Desktop packaging gate: the Tauri app owns the server lifecycle, end to end.
Run with: python verify_desktop.py   (Python 3.12 with Playwright installed)

Asserts, in order (mirroring the P2 acceptance demos, scripted so they stay true):
  1. Pre-flight: no LISTENER on 7474 (TIME_WAIT remnants are kernel-owned and tolerated;
     a listener means a live server or squatter - the gate reports ownership and FAILS,
     it never kills what it did not start). The built app exe and the PROVISIONED
     sidecar (gto-server.exe beside it) must exist. The app exe must EMBED the current
     viz/dist (a dev-mode binary, built without --features custom-protocol, would point
     the webview at http://localhost:5173 and show ERR_CONNECTION_REFUSED).
  2. Cold launch with no terminals: the sidecar becomes healthy on loopback 7474.
  3. The packaged server found the REAL matrix: POST /api/solve for a registry
     EXPECTED_CACHED board must return "cached":true (an empty-cache cold start would
     stream a fresh solve instead - the amendment-1 failure this proves absent).
  4. THE WEBVIEW ACTUALLY LOADED THE VIZ (this is the assertion whose absence let a
     dev-mode binary pass a green gate): over WebView2's CDP endpoint, the page origin
     is http://tauri.localhost (the embedded bundle, NOT localhost:5173), the viz shell
     rendered, and driving a Solve for the cached board paints the 13x13 strategy grid
     (169 .cell, at least one populated .bars) - end to end, webview -> 7474 -> render.
  5. The window opened MAXIMISED: Win32 GetWindowPlacement showCmd == SW_SHOWMAXIMIZED,
     asserted, not assumed (tauri#11554).
  6. Teardown: graceful WM_CLOSE -> the app exits, ZERO gto-server processes remain,
     and NO listener remains on 7474. The teardown-gap acceptance, proven empirically.
"""

import ctypes
import ctypes.wintypes as wt
import json
import os
import re
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

import verifier_boards

REPO = Path(__file__).parent
APP_EXE = REPO / "desktop" / "src-tauri" / "target" / "release" / "gto-lab-desktop.exe"
SIDECAR = APP_EXE.parent / "gto-server.exe"
VIZ_DIST = REPO / "viz" / "dist"
SERVER_URL = "http://127.0.0.1:7474"
SW_SHOWMAXIMIZED = 3

# WebView2 reads --remote-debugging-port from this env var; we connect Playwright to it
# over CDP to inspect the page the user actually sees. Same artefact, byte-identical to a
# double-click; only the child env differs.
WEBVIEW2_ENV = "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS"
CDP_PORT = 9333
CDP_URL = f"http://127.0.0.1:{CDP_PORT}"

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


def dist_asset_names():
    """The hashed asset filenames Vite emitted into viz/dist/index.html, e.g.
    index-DR_FvkN7.js / index-Cv-s6Z4H.css. These are baked into the app exe by
    generate_context! ONLY when the binary is built with the custom-protocol feature."""
    html = (VIZ_DIST / "index.html").read_text(encoding="utf-8")
    return re.findall(r"assets/(index-[\w-]+\.(?:js|css))", html)


def exe_embeds_dist():
    """True if the app exe contains the current viz/dist asset filenames. A dev-mode
    binary (built without --features custom-protocol) embeds devUrl instead and contains
    NONE of them - the exact regression that shipped a webview pointed at 5173."""
    names = dist_asset_names()
    if not names:
        return False, "could not read asset names from viz/dist/index.html"
    blob = APP_EXE.read_bytes()
    missing = [n for n in names if n.encode() not in blob]
    if missing:
        return False, f"asset(s) NOT embedded in exe: {missing}"
    return True, f"embedded {names}"


def assert_webview_loaded():
    """Connect to WebView2 over CDP, prove the page is the EMBEDDED bundle (origin
    http://tauri.localhost, not localhost:5173), then drive a Solve for the cached board
    and assert the 13x13 strategy grid paints. Returns (ok, [messages])."""
    from playwright.sync_api import sync_playwright

    msgs = []
    with sync_playwright() as pw:
        # WebView2 needs a beat to expose the CDP endpoint after the window appears
        browser = None
        deadline = time.time() + 15
        last_err = None
        while time.time() < deadline:
            try:
                browser = pw.chromium.connect_over_cdp(CDP_URL)
                break
            except Exception as e:
                last_err = e
                time.sleep(0.5)
        if browser is None:
            return False, [f"could not connect to WebView2 CDP at {CDP_URL}: {last_err} "
                           f"(is {WEBVIEW2_ENV} set on the app's env?)"]

        # find the viz page among WebView2's contexts/targets
        page = None
        deadline = time.time() + 15
        while time.time() < deadline and page is None:
            for ctx in browser.contexts:
                for p in ctx.pages:
                    if p.url.startswith("http"):
                        page = p
                        break
                if page:
                    break
            if page is None:
                time.sleep(0.5)
        if page is None:
            return False, ["no http page found over CDP (webview never navigated)"]

        url = page.url
        if url.startswith("http://tauri.localhost"):
            msgs.append(f"PASS  webview origin is {url} (embedded bundle, not localhost:5173)")
        else:
            msgs.append(f"FAIL  webview origin is {url} - NOT the embedded bundle; a "
                        f"localhost:5173 origin means a dev-mode binary (rebuild with "
                        f"desktop/build-desktop.ps1)")
            return False, msgs

        # the shell must have executed (the JS bundle ran), not a blank/error document
        try:
            page.wait_for_selector("#solveBtn", timeout=10_000)
            msgs.append("PASS  viz shell rendered (#solveBtn present; JS bundle executed)")
        except Exception:
            body = (page.text_content("body") or "")[:200]
            return False, msgs + [f"FAIL  viz shell did not render; body starts: {body!r}"]

        # drive a Solve for the cached board and prove the grid paints (cache-first, instant)
        try:
            page.select_option("#tPot", "srp")
            page.select_option("#tDepth", "100")
            page.fill("#flop", CACHED_BOARD)
            page.click("#solveBtn")
            page.wait_for_selector(".grid .cell", timeout=30_000)
            cells = page.locator(".grid .cell").count()
            populated = page.locator(".grid .cell .bars").count()
            if cells == 169 and populated > 0:
                msgs.append(f"PASS  cached board {CACHED_BOARD} painted the 13x13 grid "
                            f"({cells} cells, {populated} populated)")
            else:
                msgs.append(f"FAIL  grid did not render correctly: {cells} cells "
                            f"(expected 169), {populated} populated (expected > 0)")
                return False, msgs
        except Exception as e:
            return False, msgs + [f"FAIL  driving a Solve for {CACHED_BOARD} did not paint "
                                  f"the grid: {e}"]

        browser.close()  # detach CDP before teardown so it cannot mask a survivor
    return True, msgs


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
    embed_ok, embed_msg = exe_embeds_dist()
    if not embed_ok:
        print(f"FAIL  app exe does not embed the current viz/dist ({embed_msg}); this is a "
              f"dev-mode binary - rebuild with desktop\\build-desktop.ps1")
        sys.exit(1)
    print(f"PASS  app exe embeds current viz/dist ({embed_msg})")
    pre_servers = set(procs_named("gto-server.exe"))

    try:
        # ---- 2. cold launch ----
        print("\n=== 2. Cold launch (no terminals) ===")
        # same artefact a human double-clicks; the only difference is the WebView2 CDP
        # hook in the child env, so section 4 can inspect the page the user sees
        child_env = dict(os.environ)
        child_env[WEBVIEW2_ENV] = f"--remote-debugging-port={CDP_PORT}"
        app = subprocess.Popen([str(APP_EXE)], env=child_env)
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

        # ---- 4. the webview actually loaded the viz (the blind spot, closed) ----
        print("\n=== 4. Webview loaded the viz (CDP) ===")
        wv_ok, wv_msgs = assert_webview_loaded()
        for m in wv_msgs:
            print(m)
        if not wv_ok:
            ok = False

        # ---- 5. maximised window, asserted ----
        print("\n=== 5. Window state ===")
        cmds = window_show_cmd(app.pid)
        if SW_SHOWMAXIMIZED in cmds:
            print(f"PASS  GetWindowPlacement showCmd == {SW_SHOWMAXIMIZED} "
                  f"(SW_SHOWMAXIMIZED); window opened maximised")
        else:
            print(f"FAIL  window showCmd values {cmds} lack SW_SHOWMAXIMIZED")
            ok = False

        # ---- 6. teardown: the server dies with the app ----
        print("\n=== 6. Teardown (the load-bearing acceptance) ===")
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
