#!/usr/bin/env python3
"""
Phase 3b gate verification: solve-on-demand end to end.
Run with: C:\\Users\\james\\AppData\\Local\\Programs\\Python\\Python312\\python.exe verify_solve.py

Covers:
  1. cargo check + npm run build clean.
  2. A NEW legal 50bb flop: real solve, real streamed convergence (progress events > 0,
     exploitability hits the target), grid renders, result stored to the matrix.
  3. UI readout reconciles with the STORED solution on disk (AA cell aggregation + equity).
  4. Re-submitting the same flop: served from cache (zero progress events, near-instant,
     no new solve in the server log).
  5. A Phase 1 prebuilt board (AcKhQd) cache-hits: on-demand keys interop with the batch.
  6. Illegal board: inline error, submit disabled, no request.
  7. Server-side validation via direct API: 2-card board and duplicate card -> error events;
     malformed JSON -> HTTP 4xx.
  8. Zero console/page errors throughout.
"""

import json
import subprocess
import sys
import time
import urllib.request
import urllib.error
from pathlib import Path

VITE_PORT = 5247
APP_URL = f"http://localhost:{VITE_PORT}/"
SERVER_URL = "http://127.0.0.1:7474"
REPO = Path(__file__).parent
VIZ_DIR = REPO / "viz"
MATRIX_DIR = REPO / "matrix"
SERVER_EXE = REPO / "target" / "release" / "gto-server.exe"
SERVER_LOG = REPO / "out" / "verify_solve_server.log"

# board reservations live in verifier_boards.py (partitioned registry, disjointness
# asserted + consumables swept at setup); candidates are ace-free low/dry boards so the
# AA reconciliation cell is never board-blocked and the solve converges fast
import verifier_boards

CANDIDATES = verifier_boards.SOLVE_CANDIDATES

SOLVE_WAIT_MS = 240_000  # generous; server-side timeout is 300 s


def step_cargo_check():
    print("=== 1. cargo check --workspace --all-targets ===")
    r = subprocess.run(
        ["cargo", "check", "--workspace", "--all-targets"],
        cwd=str(REPO), capture_output=True, text=True,
    )
    if r.returncode != 0:
        print("FAIL  cargo check")
        print((r.stderr or r.stdout)[-3000:])
        return False
    print("PASS  cargo check clean")
    return True


def step_npm_build():
    print("\n=== 2. npm run build ===")
    r = subprocess.run(
        "npm.cmd run build", cwd=str(VIZ_DIR), shell=True,
        capture_output=True, text=True,
    )
    if r.returncode != 0:
        print("FAIL  npm run build")
        print((r.stderr or r.stdout)[-3000:])
        return False
    print("PASS  npm run build clean")
    return True


def pick_fresh_board():
    manifest_path = MATRIX_DIR / "manifest.json"
    cached = set()
    if manifest_path.exists():
        cached = set(json.loads(manifest_path.read_text())["spots"].keys())
    for board in CANDIDATES:
        if f"50bb_{board}_flop_srp-v1" not in cached and \
           f"50bb_{board}_flop_srp-v1-shrunk" not in cached:
            return board
    return None


def wait_health(timeout_s=30):
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(f"{SERVER_URL}/api/health", timeout=2) as r:
                if r.status == 200:
                    return True
        except Exception:
            time.sleep(0.4)
    return False


def expected_class_stats(key, label_ranks=("A", "A")):
    """Mean strategy/equity for a hand class, from the STORED summary sidecar on disk."""
    summary = json.loads((MATRIX_DIR / f"{key}.summary.json").read_text())
    root = summary["root"]
    hands, strategy, equity = root["hands"], root["strategy"], root["equity"]
    n = len(hands)
    n_actions = len(root["actions"])
    idxs = [i for i, h in enumerate(hands) if h[0] == label_ranks[0] and h[2] == label_ranks[1]]
    if not idxs:
        return None
    strat = [sum(strategy[i + a * n] for i in idxs) / len(idxs) for a in range(n_actions)]
    eq = sum(equity[i] for i in idxs) / len(idxs)
    return {"combos": len(idxs), "strategy": strat, "equity": eq,
            "exploit": summary["exploitability_pct_pot"], "actions": root["actions"]}


def submit_flop(page, board):
    page.fill("#flop", "")
    page.fill("#flop", board)
    page.click("#solveBtn")


def wait_terminal(page, timeout_ms):
    page.wait_for_function(
        "() => ['done','cached','error'].includes(document.querySelector('#solveStatus').dataset.state)",
        timeout=timeout_ms,
    )
    status = page.eval_on_selector("#solveStatus", "el => el.dataset.state")
    detail = page.eval_on_selector("#ssValue", "el => el.textContent")
    label = page.eval_on_selector("#ssLabel", "el => el.textContent")
    progress = int(page.eval_on_selector("#solveStatus", "el => el.dataset.progressCount"))
    return status, label, detail, progress


def assert_grid(page):
    page.wait_for_function(
        "() => document.querySelectorAll('.cell').length === 169", timeout=10_000)
    board_n = page.eval_on_selector_all(".board-area .card", "cs => cs.length")
    hero_n = page.eval_on_selector_all(".seat--hero .card", "cs => cs.length")
    return board_n == 3 and hero_n == 2


def click_class_cell(page, label):
    return page.evaluate(f"""
        () => {{
            const lbl = [...document.querySelectorAll('.cell-label')]
                         .find(el => el.textContent.trim() === '{label}');
            if (!lbl) return false;
            lbl.closest('.cell').click();
            return true;
        }}
    """)


def step_e2e(fresh_board):
    print("\n=== 3. End-to-end solve-on-demand (Playwright) ===")
    from playwright.sync_api import sync_playwright

    SERVER_LOG.parent.mkdir(exist_ok=True)
    server_log = open(SERVER_LOG, "w", encoding="utf-8")
    server = subprocess.Popen(
        [str(SERVER_EXE)], cwd=str(REPO),
        stdout=server_log, stderr=subprocess.STDOUT, text=True,
    )
    vite = subprocess.Popen(
        f"npm.cmd run dev -- --port {VITE_PORT} --strictPort",
        cwd=str(VIZ_DIR), shell=True,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
    )

    ok = True
    try:
        if not wait_health():
            print("  FAIL  solve server never became healthy")
            return False
        print(f"  server healthy at {SERVER_URL}")

        with sync_playwright() as pw:
            browser = pw.chromium.launch()
            page = browser.new_page()
            console_errors, page_errors = [], []
            page.on("console", lambda m: console_errors.append(m.text) if m.type == "error" else None)
            page.on("pageerror", lambda e: page_errors.append(str(e)))

            for attempt in range(12):
                try:
                    page.goto(APP_URL, timeout=5000, wait_until="domcontentloaded")
                    break
                except Exception:
                    if attempt == 11:
                        print(f"  FAIL  could not reach {APP_URL}")
                        return False
                    time.sleep(0.5)
            page.wait_for_selector("#solveBtn", timeout=15_000)

            key = f"50bb_{fresh_board}_flop_srp-v1"

            # ---- 3a. fresh solve with real streamed convergence ----
            print(f"  -- fresh solve: {fresh_board} (key {key}) --")
            t0 = time.time()
            submit_flop(page, fresh_board)
            status, label, detail, progress = wait_terminal(page, SOLVE_WAIT_MS)
            dt = time.time() - t0
            if status != "done":
                print(f"  FAIL  expected state 'done', got '{status}' ({label}: {detail})")
                return False
            if progress < 1:
                print(f"  FAIL  no progress events streamed (count={progress})")
                ok = False
            else:
                print(f"  PASS  solved in {dt:.1f}s with {progress} streamed convergence samples")
            if not assert_grid(page):
                print("  FAIL  grid/table did not render after solve")
                ok = False
            else:
                print("  PASS  board=3 hero=2 cells=169 rendered from the live solve")

            # the meta chip row is the post-removal "the UI registered this spot" signal
            # (text_content, not visibility: .meta is display:none under 1024px widths)
            spaced = " ".join(fresh_board[i:i + 2] for i in (0, 2, 4))
            meta_text = page.text_content("#meta") or ""
            if spaced in meta_text and "50bb" in meta_text:
                print(f"  PASS  meta chips show the solved spot ({spaced} · 50bb)")
            else:
                print(f"  FAIL  meta chips missing solved spot: '{meta_text.strip()}'")
                ok = False

            # stored result on disk + converged below target
            stats = expected_class_stats(key)
            if stats is None:
                print("  FAIL  no stored summary for the solved key (not cached to matrix?)")
                return False
            if stats["exploit"] <= 0.005 + 1e-6:
                print(f"  PASS  stored exploitability {stats['exploit']*100:.3f}% pot <= 0.5% target")
            else:
                print(f"  FAIL  stored exploitability {stats['exploit']*100:.3f}% pot above target")
                ok = False

            # ---- 3b. UI readout reconciles with the STORED solution (AA cell) ----
            print("  -- reconciliation: AA cell vs matrix/<key>.summary.json --")
            if not click_class_cell(page, "AA"):
                print("  FAIL  AA cell not found")
                ok = False
            else:
                time.sleep(0.25)
                stat_texts = page.eval_on_selector_all(".stat-v", "vs => vs.map(v => v.textContent)")
                eq_disp = float(stat_texts[1].replace("%", "").replace("chips", "").strip())
                diff = abs(eq_disp - stats["equity"] * 100)
                tag = "PASS" if diff <= 0.2 else "FAIL"
                print(f"  {tag}  AA equity ui={eq_disp:.1f}% stored={stats['equity']*100:.1f}% diff={diff:.3f}")
                if tag == "FAIL":
                    ok = False
                combos_txt = page.eval_on_selector(".detail-combos", "el => el.textContent")
                if combos_txt.strip().startswith(f"{stats['combos']} combo"):
                    print(f"  PASS  AA combo count = {stats['combos']} (weight-correct aggregation)")
                else:
                    print(f"  FAIL  combo count mismatch: ui '{combos_txt}' vs stored {stats['combos']}")
                    ok = False
                bar_texts = page.eval_on_selector_all(".dbar-val", "vs => vs.map(v => v.textContent)")
                for i, act in enumerate(stats["actions"]):
                    exp_pct = round(stats["strategy"][i] * 100)
                    disp = float(bar_texts[i].replace("%", "").strip())
                    tag = "PASS" if abs(disp - exp_pct) <= 1 else "FAIL"
                    print(f"  {tag}  AA {act}: ui={disp:.0f}% stored={exp_pct}%")
                    if tag == "FAIL":
                        ok = False

            # ---- 3c. same flop again: cache hit, no re-solve ----
            print("  -- cache hit: same flop re-submitted --")
            t0 = time.time()
            submit_flop(page, fresh_board)
            status, label, detail, progress = wait_terminal(page, 15_000)
            dt = time.time() - t0
            # no-re-solve is guarded by progress==0 here plus the server-log single-solve
            # check in 3f; the wall-clock bound only separates a cache load (~2-3s) from a
            # real solve (20s+). 3.0s flaked at 3.13s on a loaded machine with the cache
            # hit confirmed, so the bound carries margin now.
            if status == "cached" and progress == 0 and dt < 10.0:
                print(f"  PASS  served from cache in {dt:.2f}s with 0 progress events")
            else:
                print(f"  FAIL  expected instant cache hit; state={status} progress={progress} dt={dt:.2f}s")
                ok = False
            if not assert_grid(page):
                print("  FAIL  grid did not render on the cache hit")
                ok = False

            # ---- 3d. Phase 1 prebuilt board cache-hits (key interop) ----
            interop = verifier_boards.SOLVE_CACHE_INTEROP
            print(f"  -- cache interop: Phase 1 batch board {interop} --")
            t0 = time.time()
            submit_flop(page, interop)
            status, label, detail, progress = wait_terminal(page, 15_000)
            dt = time.time() - t0
            if status == "cached" and progress == 0:
                print(f"  PASS  Phase 1 entry served from cache in {dt:.2f}s (no re-solve)")
            else:
                print(f"  FAIL  expected cache hit on Phase 1 board; state={status} progress={progress}")
                ok = False

            # ---- 3e. illegal board: inline error, no request ----
            print("  -- illegal board: KsKs2c --")
            page.fill("#flop", "")
            page.fill("#flop", "KsKs2c")
            time.sleep(0.15)
            state = page.eval_on_selector("#solveStatus", "el => el.dataset.state")
            disabled = page.eval_on_selector("#solveBtn", "b => b.disabled")
            msg = page.eval_on_selector("#ssLabel", "el => el.textContent")
            if state == "invalid" and disabled and "duplicate" in msg:
                print(f"  PASS  inline error '{msg}', submit disabled")
            else:
                print(f"  FAIL  state={state} disabled={disabled} msg='{msg}'")
                ok = False

            # ---- console hygiene ----
            errs = console_errors + page_errors
            if errs:
                print(f"  FAIL  {len(errs)} console/page errors; first: {errs[0]}")
                ok = False
            else:
                print("  PASS  zero console/page errors across the whole session")

            browser.close()

        # ---- 3f. server log: the fresh board was solved exactly once ----
        server_log.flush()
        log_text = SERVER_LOG.read_text(encoding="utf-8")
        n_solves = log_text.count(f"solving 50bb_{fresh_board}_flop_srp-v1")
        if n_solves == 1:
            print(f"  PASS  server log shows exactly 1 solve of {fresh_board} across 2 submits")
        else:
            print(f"  FAIL  server log shows {n_solves} solves of {fresh_board} (expected 1)")
            ok = False

        return ok
    finally:
        for proc in (server, vite):
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
        server_log.close()


def step_api_validation():
    """Direct API: server-side validation independent of the client (server must be running)."""
    print("\n=== 4. Server-side API validation ===")
    server = subprocess.Popen(
        [str(SERVER_EXE)], cwd=str(REPO),
        stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT, text=True,
    )
    ok = True
    try:
        if not wait_health():
            print("  FAIL  server not healthy for API tests")
            return False

        def post_raw(body: bytes):
            req = urllib.request.Request(
                f"{SERVER_URL}/api/solve", data=body,
                headers={"Content-Type": "application/json"}, method="POST")
            try:
                with urllib.request.urlopen(req, timeout=20) as r:
                    return r.status, r.read().decode("utf-8", "replace")
            except urllib.error.HTTPError as e:
                return e.code, e.read().decode("utf-8", "replace")

        status, body = post_raw(json.dumps({"board": ["Ks", "7h"]}).encode())
        if status == 200 and "event: error" in body and "exactly 3 cards" in body:
            print("  PASS  2-card board -> SSE error event ('exactly 3 cards')")
        else:
            print(f"  FAIL  2-card board: HTTP {status}, body: {body[:200]}")
            ok = False

        status, body = post_raw(json.dumps({"board": ["Ks", "Ks", "2c"]}).encode())
        if status == 200 and "event: error" in body and "duplicate card" in body:
            print("  PASS  duplicate card -> SSE error event ('duplicate card')")
        else:
            print(f"  FAIL  duplicate card: HTTP {status}, body: {body[:200]}")
            ok = False

        status, body = post_raw(b"{not json")
        if 400 <= status < 500:
            print(f"  PASS  malformed JSON -> HTTP {status}")
        else:
            print(f"  FAIL  malformed JSON -> HTTP {status}")
            ok = False

        return ok
    finally:
        server.terminate()
        try:
            server.wait(timeout=5)
        except subprocess.TimeoutExpired:
            server.kill()


def main():
    print("=== Phase 3b Gate Verification ===\n")
    verifier_boards.prepare(REPO)
    fresh = pick_fresh_board()
    if fresh is None:
        print("FAIL  no un-cached candidate board left after the sweep (should be impossible)")
        sys.exit(1)
    print(f"fresh-solve candidate: {fresh}\n")

    results = [
        ("cargo check", step_cargo_check()),
        ("npm build", step_npm_build()),
        ("solve-on-demand E2E", step_e2e(fresh)),
        ("server-side validation", step_api_validation()),
    ]

    print("\n=== Summary ===")
    all_pass = True
    for name, passed in results:
        print(f"  {'PASS' if passed else 'FAIL'}  {name}")
        if not passed:
            all_pass = False

    print("\nGATE PASSED" if all_pass else "\nGATE FAILED")
    sys.exit(0 if all_pass else 1)


if __name__ == "__main__":
    main()
