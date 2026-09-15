#!/usr/bin/env python3
"""
Phase 4b gate verification: the manual GTO trainer.
Run with: python verify_trainer.py   (Python 3.12 with Playwright installed)

Covers:
  1. cargo check + grading unit tests + npm build clean.
  2. COMMIT GATE (the centerpiece): in train mode, zero /api/solve, /api/grade and
     data/*.json requests before the commit click, and no grid cells in the DOM; the solve
     and grade fire only after commit.
  3. Grading correctness, data-driven: exemplar (hand, action) pairs for every verdict band
     found in the REAL solved data are graded via /api/grade and reconciled against a
     python recomputation (frequency, EV loss, band arithmetic) from the exported contract.
  4. Both depths through the UI: 50bb cached spot and 100bb Phase 1 spot (depth in the key).
  5. A FRESH board in the trainer: commit first, real solve streams progress, then reveal.
  6. Off-tree action maps with a notice; collision / out-of-range / unsupported-depth /
     unsolved-spot all error cleanly.
  7. Zero console/page errors. (Phase 2 + 3 verifiers run separately for regression.)
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
DATA_DIR = VIZ_DIR / "public" / "data"
MATRIX_DIR = REPO / "matrix"
SERVER_EXE = REPO / "target" / "release" / "gto-server.exe"
SERVER_LOG = REPO / "out" / "verify_trainer_server.log"

# verdict bands: mirrors gto-config TrainerConfig::default() (pot = 100 chips at the root)
EPS_CHIPS = 0.5
MINOR_CHIPS = 2.0
FREQ_FLOOR = 0.10

# board reservations live in verifier_boards.py (partitioned registry, disjointness
# asserted + consumables/absents swept at setup, so run order never matters)
import verifier_boards

CACHED_50 = verifier_boards.TRAINER_CACHED_50    # solved in Phase 3, stays cached
CACHED_100 = verifier_boards.TRAINER_CACHED_100  # solved in Phase 1, stays cached
FRESH_CANDIDATES = verifier_boards.TRAINER_FRESH
ABSENT = verifier_boards.TRAINER_ABSENT          # the unsolved-spot 409 fixture

# Above the server-side cap (phase1.toml io.solve_timeout_secs = 900s) so the fresh-solve
# round always sees a terminal SSE state; see verify_solve.py for the srp-v2 measured basis.
SOLVE_WAIT_MS = 960_000


def run_step(name, cmd, cwd, shell=False):
    r = subprocess.run(cmd, cwd=str(cwd), capture_output=True, text=True, shell=shell)
    if r.returncode != 0:
        print(f"FAIL  {name}")
        print((r.stderr or r.stdout)[-2500:])
        return False
    print(f"PASS  {name}")
    return True


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


def api(path, body=None, method=None):
    """Call the server; returns (status, parsed-or-text)."""
    url = f"{SERVER_URL}{path}"
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(
        url, data=data, method=method or ("POST" if data else "GET"),
        headers={"Content-Type": "application/json"} if data else {},
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            text = r.read().decode("utf-8", "replace")
            status = r.status
    except urllib.error.HTTPError as e:
        text = e.read().decode("utf-8", "replace")
        status = e.code
    try:
        return status, json.loads(text)
    except json.JSONDecodeError:
        return status, text


def expected_grade(contract, hand, action_label):
    """Recompute frequency / EV loss / verdict from the exported contract (python-side)."""
    row = next(h for h in contract["hands"] if h["hand"] == hand)
    a = contract["actions"].index(action_label)
    evs = row["ev_actions"]
    best = max(evs)
    loss = max(0.0, best - evs[a])
    freq = row["strategy"][a]
    if loss <= EPS_CHIPS:
        verdict = "correct"
    elif freq >= FREQ_FLOOR and loss <= MINOR_CHIPS:
        verdict = "fine_mix"
    elif loss <= MINOR_CHIPS:
        verdict = "minor_leak"
    else:
        verdict = "clear_leak"
    return {"frequency": freq, "loss": loss, "verdict": verdict,
            "best": contract["actions"][evs.index(best)]}


def find_band_exemplars(contracts):
    """Scan real solved data for one (board, hand, action) exemplar per verdict band."""
    found = {}
    for board, contract in contracts.items():
        for h in contract["hands"]:
            if len(h.get("ev_actions") or []) != len(contract["actions"]):
                continue
            for i, action in enumerate(contract["actions"]):
                exp = expected_grade(contract, h["hand"], action)
                found.setdefault(exp["verdict"], (board, h["hand"], action, exp))
    return found


def step_units():
    print("=== 1. Builds + unit tests ===")
    ok = run_step("cargo check --workspace --all-targets",
                  ["cargo", "check", "--workspace", "--all-targets"], REPO)
    ok = run_step("cargo test -p gto-engine (grading units)",
                  ["cargo", "test", "-p", "gto-engine"], REPO) and ok
    ok = run_step("npm run build", "npm.cmd run build", VIZ_DIR, shell=True) and ok
    return ok


def step_api_grading():
    print("\n=== 2. Grading correctness (data-driven, via API) ===")
    contracts = {}
    for board, depth in ((CACHED_50, 50), (CACHED_100, 100)):
        p = DATA_DIR / f"{verifier_boards.spot_key(depth, board)}.json"
        contracts[(board, depth)] = json.loads(p.read_text())

    exemplars = find_band_exemplars(
        {b: c for (b, _), c in zip(contracts.keys(), contracts.values())}
    )
    # rebuild with depth info for the API call
    by_board_depth = {b: d for (b, d) in contracts.keys()}

    ok = True
    print(f"  verdict bands found in real data: {sorted(exemplars.keys())}")
    if "correct" not in exemplars:
        print("  FAIL  no 'correct' exemplar found (impossible: argmax action always qualifies)")
        return False
    if len(exemplars) < 2:
        print("  FAIL  fewer than 2 verdict bands present in the data")
        ok = False

    for verdict, (board, hand, action, exp) in sorted(exemplars.items()):
        depth = by_board_depth[board]
        cards = [board[i:i + 2] for i in (0, 2, 4)]
        status, g = api("/api/grade",
                        {"board": cards, "depth": depth, "hand": hand, "action": action})
        if status != 200:
            print(f"  FAIL  grade {board} {hand} {action}: HTTP {status} {g}")
            ok = False
            continue
        freq_ok = abs(g["frequency"] - exp["frequency"]) < 1e-4
        loss_ok = abs(g["ev_loss_chips"] - exp["loss"]) < 0.01
        verdict_ok = g["verdict"] == verdict
        best_ok = g["best_action"] == exp["best"]
        tag = "PASS" if (freq_ok and loss_ok and verdict_ok and best_ok) else "FAIL"
        print(f"  {tag}  {verdict:<10} {depth}bb {board} {hand} {action}: "
              f"freq {g['frequency']:.3f} loss {g['ev_loss_chips']:.2f} chips "
              f"({g['ev_loss_pct_pot']*100:.2f}% pot) best {g['best_action']}")
        if tag == "FAIL":
            print(f"        expected freq {exp['frequency']:.3f} loss {exp['loss']:.2f} "
                  f"verdict {verdict} best {exp['best']}")
            ok = False

    # off-tree action maps to nearest with a notice
    cards = [CACHED_50[i:i + 2] for i in (0, 2, 4)]
    status, g = api("/api/grade",
                    {"board": cards, "depth": 50, "hand": "AsAc", "action": "Bet(50)"})
    if status == 200 and g.get("mapped_from") == "Bet(50)" and g.get("chosen_action") == "Bet(33)":
        print(f"  PASS  off-tree Bet(50) mapped to {g['chosen_action']} with notice")
    else:
        print(f"  FAIL  off-tree mapping: HTTP {status} {g}")
        ok = False

    # error cases
    absent_cards = [ABSENT[i:i + 2] for i in (0, 2, 4)]
    cases = [
        ("collision", f"/api/setup?depth=50&board={CACHED_50}&hand={CACHED_50[:2]}2h",
         None, 400, "collides"),
        ("bad depth", f"/api/setup?depth=20&board={CACHED_50}&hand=AsAc",
         None, 400, "ranges not available"),
        ("unsolved spot", "/api/grade",
         {"board": absent_cards, "depth": 50, "hand": "AsAc", "action": "Check"},
         409, "not solved"),
        ("out-of-range hand", "/api/grade",
         {"board": cards, "depth": 50, "hand": "7s2d", "action": "Check"},
         400, "not in the BB"),
    ]
    # the absent fixture is registry-guarded (no consumable pool may contain it) and the
    # setup sweep removes any stray solve of it; this assert is defense in depth
    manifest = json.loads((MATRIX_DIR / "manifest.json").read_text())
    assert verifier_boards.spot_key(50, ABSENT) not in manifest["spots"], "test board unexpectedly cached"

    for name, path, body, want_status, want_text in cases:
        status, resp = api(path, body)
        text = json.dumps(resp)
        if status == want_status and want_text in text:
            print(f"  PASS  {name}: HTTP {status}, '{want_text}'")
        else:
            print(f"  FAIL  {name}: HTTP {status} (want {want_status}), body {text[:160]}")
            ok = False

    # out-of-range hand at setup is a 200 with hand_in_range=false (UI shows the message)
    status, resp = api("/api/setup?depth=50&board=8d6s3c&hand=7s2d")
    if status == 200 and resp.get("hand_in_range") is False:
        print("  PASS  setup flags 7s2d as out of range (hand_in_range=false)")
    else:
        print(f"  FAIL  setup out-of-range: HTTP {status} {resp}")
        ok = False

    return ok


def step_ui():
    print("\n=== 3. Trainer UI: commit gate + reveal + both depths + fresh solve ===")
    from playwright.sync_api import sync_playwright

    contract_50 = json.loads((DATA_DIR / f"{verifier_boards.spot_key(50, CACHED_50)}.json").read_text())
    contract_100 = json.loads((DATA_DIR / f"{verifier_boards.spot_key(100, CACHED_100)}.json").read_text())
    cached_3bp = verifier_boards.TRAINER_CACHED_3BP
    contract_3bp = json.loads(
        (DATA_DIR / f"{verifier_boards.spot_key(100, cached_3bp, pot='3bp')}.json").read_text())

    # pick a guaranteed-present hand on the 100bb board (mid-range row from the real data)
    hand_100 = contract_100["hands"][len(contract_100["hands"]) // 2]["hand"]

    manifest = json.loads((MATRIX_DIR / "manifest.json").read_text())
    fresh = next((b for b in FRESH_CANDIDATES
                  if verifier_boards.spot_key(50, b) not in manifest["spots"]), None)
    if fresh is None:
        print("  FAIL  no un-cached fresh candidate left; extend FRESH_CANDIDATES")
        return False
    print(f"  fresh trainer board: {fresh}")

    ok = True
    requests_log = []

    vite = subprocess.Popen(
        f"npm.cmd run dev -- --port {VITE_PORT} --strictPort",
        cwd=str(VIZ_DIR), shell=True,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
    )
    try:
        with sync_playwright() as pw:
            browser = pw.chromium.launch()
            page = browser.new_page()
            console_errors, page_errors = [], []
            page.on("console", lambda m: console_errors.append(m.text) if m.type == "error" else None)
            page.on("pageerror", lambda e: page_errors.append(str(e)))
            page.on("request", lambda r: requests_log.append(r.url))

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

            def n_solver_fetches():
                return sum(1 for u in requests_log
                           if "/api/solve" in u or "/api/grade" in u or "/data/" in u and u.endswith(".json"))

            def run_round(depth, board, hand, action_label, expect_cached, pot="srp"):
                """One full trainer round; returns the displayed verdict."""
                page.click("#modeTrain")
                page.wait_for_function(
                    "() => document.querySelectorAll('.cell').length === 0", timeout=5000)
                page.select_option("#tPot", pot)
                page.select_option("#tDepth", str(depth))
                page.fill("#tBoard", board)
                page.fill("#tHand", hand)

                mark = n_solver_fetches()
                page.click("#tSetup")
                page.wait_for_function(
                    "() => document.querySelector('#trainerHost').dataset.phase === 'commit'",
                    timeout=10_000)

                # ---- COMMIT GATE ----
                gate_ok = True
                if n_solver_fetches() != mark:
                    print(f"  FAIL  commit gate: solver/static data fetched before commit ({board})")
                    gate_ok = False
                cells = page.eval_on_selector_all(".cell", "cs => cs.length")
                if cells != 0:
                    print(f"  FAIL  commit gate: {cells} grid cells rendered before commit ({board})")
                    gate_ok = False

                buttons = page.eval_on_selector_all(
                    ".t-action-btn", "bs => bs.map(b => b.textContent)")
                page.click(f'.t-action-btn:has-text("{action_label}")')
                t0 = time.time()
                page.wait_for_function(
                    "() => document.querySelector('#trainerHost').dataset.phase === 'reveal'",
                    timeout=SOLVE_WAIT_MS)
                dt = time.time() - t0
                progress = int(page.eval_on_selector(
                    "#trainerHost", "el => el.dataset.progressCount || '0'"))
                verdict = page.eval_on_selector(
                    ".g-verdict", "el => el.dataset.verdict")
                grid_cells = page.eval_on_selector_all(".cell", "cs => cs.length")
                selected = page.eval_on_selector(
                    ".cell--selected .cell-label", "el => el.textContent")
                return {
                    "gate_ok": gate_ok, "buttons": buttons, "dt": dt,
                    "progress": progress, "verdict": verdict,
                    "grid_cells": grid_cells, "selected": selected,
                }

            # ---- round 1: cached 50bb spot, commit the argmax action (expect 'correct') ----
            exp = expected_grade(contract_50, "AsAc", contract_50["actions"][1])  # Bet(33)
            r = run_round(50, CACHED_50, "acas", "Bet(33)", expect_cached=True)
            if r["gate_ok"]:
                print(f"  PASS  commit gate held on cached 50bb {CACHED_50} (no pre-commit fetch, blank grid)")
            else:
                ok = False
            if r["buttons"] == contract_50["actions"]:
                print(f"  PASS  commit buttons == solved action set {r['buttons']}")
            else:
                print(f"  FAIL  buttons {r['buttons']} != contract actions {contract_50['actions']}")
                ok = False
            if r["verdict"] == exp["verdict"] and r["grid_cells"] == 169 and r["selected"] == "AA":
                print(f"  PASS  reveal: verdict '{r['verdict']}' (expected '{exp['verdict']}'), "
                      f"169 cells, AA pre-selected, {r['dt']:.1f}s")
            else:
                print(f"  FAIL  reveal: verdict {r['verdict']} (want {exp['verdict']}), "
                      f"cells {r['grid_cells']}, selected {r['selected']}")
                ok = False
            if r["progress"] == 0:
                print(f"  PASS  cached spot: 0 progress events, revealed in {r['dt']:.1f}s")
            else:
                print(f"  FAIL  cached spot streamed {r['progress']} progress events")
                ok = False

            # ---- round 2: 100bb Phase 1 spot through the trainer ----
            best_100 = expected_grade(contract_100, hand_100, contract_100["actions"][0])
            # commit the BEST action for this hand (whatever it is) -> must grade 'correct'
            best_action_100 = best_100["best"]
            exp2 = expected_grade(contract_100, hand_100, best_action_100)
            r2 = run_round(100, CACHED_100, hand_100, best_action_100, expect_cached=True)
            if r2["gate_ok"] and r2["verdict"] == exp2["verdict"] and r2["grid_cells"] == 169:
                print(f"  PASS  100bb {CACHED_100} {hand_100} {best_action_100}: "
                      f"verdict '{r2['verdict']}', cached reveal {r2['dt']:.1f}s")
            else:
                print(f"  FAIL  100bb round: gate={r2['gate_ok']} verdict={r2['verdict']} "
                      f"(want {exp2['verdict']}) cells={r2['grid_cells']}")
                ok = False
            # depth-flowed proof, post-dropdown: the graded history row echoes the grade
            # response's depth, and the revealed spot's meta chips carry the key's depth
            th_spots = page.eval_on_selector_all(".th-spot", "els => els.map(e => e.textContent)")
            meta_text = page.text_content("#meta") or ""
            if th_spots and th_spots[-1] == f"100bb {CACHED_100}" and "100bb" in meta_text:
                print(f"  PASS  depth flowed into the key: history row '{th_spots[-1]}', meta 100bb")
            else:
                print(f"  FAIL  depth missing: history rows {th_spots}, meta '{meta_text.strip()}'")
                ok = False

            # ---- round 2b: cached 3-BET pot through the commit gate (pot select drives
            # the key, the geometry, and the action set: the buttons must be the 3bp
            # tree's root actions, not srp's) ----
            best_3bp = expected_grade(contract_3bp, "AhAd", contract_3bp["actions"][0])["best"]
            exp3bp = expected_grade(contract_3bp, "AhAd", best_3bp)
            r2b = run_round(100, cached_3bp, "ahad", best_3bp, expect_cached=True, pot="3bp")
            if r2b["buttons"] == contract_3bp["actions"]:
                print(f"  PASS  3bp commit buttons == 3bp action set {r2b['buttons']}")
            else:
                print(f"  FAIL  3bp buttons {r2b['buttons']} != {contract_3bp['actions']}")
                ok = False
            if r2b["gate_ok"] and r2b["verdict"] == exp3bp["verdict"] \
                    and r2b["grid_cells"] == 169 and r2b["progress"] == 0:
                print(f"  PASS  3bp {cached_3bp} AhAd {best_3bp}: verdict '{r2b['verdict']}', "
                      f"cached reveal {r2b['dt']:.1f}s")
            else:
                print(f"  FAIL  3bp round: gate={r2b['gate_ok']} verdict={r2b['verdict']} "
                      f"(want {exp3bp['verdict']}) cells={r2b['grid_cells']} "
                      f"progress={r2b['progress']}")
                ok = False

            # ---- round 3: FRESH board, commit before any solve exists anywhere ----
            r3 = run_round(50, fresh, "acas", "Check", expect_cached=False)
            if r3["gate_ok"] and r3["progress"] >= 1 and r3["grid_cells"] == 169 and r3["verdict"]:
                print(f"  PASS  fresh {fresh}: committed pre-solve, {r3['progress']} progress "
                      f"samples streamed, solved+graded '{r3['verdict']}' in {r3['dt']:.1f}s")
            else:
                print(f"  FAIL  fresh round: gate={r3['gate_ok']} progress={r3['progress']} "
                      f"cells={r3['grid_cells']} verdict={r3['verdict']}")
                ok = False

            # ---- session history ----
            rows = page.eval_on_selector_all(".th-row", "rs => rs.length")
            head = page.eval_on_selector(".th-head", "el => el.textContent") or ""
            if rows == 4 and "4 spots" in head and "total EV loss" in head:
                print(f"  PASS  session history: 4 rows, running total shown")
            else:
                print(f"  FAIL  history rows={rows} head='{head.strip()}'")
                ok = False

            # ---- client-side collision: no request fired ----
            page.fill("#tBoard", CACHED_50)
            page.fill("#tHand", f"{CACHED_50[:2]}2h")
            mark = len([u for u in requests_log if "/api/setup" in u])
            page.click("#tSetup")
            time.sleep(0.4)
            err = page.eval_on_selector("#tErr", "el => el.textContent")
            setups_after = len([u for u in requests_log if "/api/setup" in u])
            if "collides" in err and setups_after == mark:
                print(f"  PASS  client collision check: '{err}', zero requests")
            else:
                print(f"  FAIL  collision: err='{err}' new_requests={setups_after - mark}")
                ok = False

            errs = console_errors + page_errors
            if errs:
                print(f"  FAIL  {len(errs)} console/page errors; first: {errs[0]}")
                ok = False
            else:
                print("  PASS  zero console/page errors across the whole session")

            browser.close()
        return ok
    finally:
        vite.terminate()
        try:
            vite.wait(timeout=5)
        except subprocess.TimeoutExpired:
            vite.kill()


def main():
    print("=== Phase 4b Gate Verification ===\n")
    verifier_boards.prepare(REPO)
    results = [("builds + units", step_units())]

    SERVER_LOG.parent.mkdir(exist_ok=True)
    log = open(SERVER_LOG, "w", encoding="utf-8")
    server = subprocess.Popen([str(SERVER_EXE)], cwd=str(REPO),
                              stdout=log, stderr=subprocess.STDOUT, text=True)
    try:
        if not wait_health():
            print("FAIL  solve server never became healthy")
            results.append(("server up", False))
        else:
            results.append(("grading API", step_api_grading()))
            results.append(("trainer UI", step_ui()))
    finally:
        server.terminate()
        try:
            server.wait(timeout=5)
        except subprocess.TimeoutExpired:
            server.kill()
        log.close()

    print("\n=== Summary ===")
    all_pass = all(p for _, p in results)
    for name, passed in results:
        print(f"  {'PASS' if passed else 'FAIL'}  {name}")
    print("\nGATE PASSED" if all_pass else "\nGATE FAILED")
    sys.exit(0 if all_pass else 1)


if __name__ == "__main__":
    main()
