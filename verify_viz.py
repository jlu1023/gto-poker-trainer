#!/usr/bin/env python3
"""
Phase 2b gate verification for GTO Lab viz.
Run with: C:\\Users\\james\\AppData\\Local\\Programs\\Python\\Python312\\python.exe verify_viz.py

Since the SPOT preset dropdown was removed, the solve cluster (depth select + board input +
Solve) is the single spot entry, so this gate drives it. Preconditions: a built
target/release/gto-server.exe and a populated matrix/ (the current-tree fixtures listed in
viz/public/data/index.json must be cached; the tree id comes from verifier_boards.TREE_ID).
Each fixture load MUST terminate "cached" with zero progress events: the gate also guards
that presets never silently re-solve. Rendering is still reconciled against the static
exports in viz/public/data/*.json. The smoke-v0 spot is out of gate scope: the cluster only
forms current-tree keys, so a smoke-v0 entry is unreachable from the UI (it stays cached).
"""

import json
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

import verifier_boards

PORT = 5247
REPO = Path(__file__).parent
VIZ_DIR = REPO / "viz"
DATA_DIR = VIZ_DIR / "public" / "data"
URL = f"http://localhost:{PORT}/"
SERVER_URL = "http://127.0.0.1:7474"
SERVER_EXE = REPO / "target" / "release" / "gto-server.exe"
SERVER_LOG = REPO / "out" / "verify_viz_server.log"


def step_cargo_check():
    print("=== 1. cargo check --workspace --all-targets ===")
    r = subprocess.run(
        ["cargo", "check", "--workspace", "--all-targets"],
        cwd=str(REPO), capture_output=True, text=True
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
        "npm.cmd run build",
        cwd=str(VIZ_DIR), shell=True,
        capture_output=True, text=True
    )
    if r.returncode != 0:
        print("FAIL  npm run build")
        print((r.stderr or r.stdout)[-3000:])
        return False
    print("PASS  npm run build clean")
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


def load_fixture(page, board, depth, pot="srp", timeout_ms=15_000):
    """Load a cached spot through the solve cluster (the single spot entry).
    Returns (state, progress_count). The terminal wait also requires the board in the
    strip label so a stale terminal state from the PREVIOUS spot cannot satisfy it.
    Pot select goes FIRST: switching to 4bp locks depth to 100 client-side."""
    page.select_option("#tPot", pot)
    page.select_option("#tDepth", str(depth))
    page.fill("#flop", "")
    page.fill("#flop", board)
    page.click("#solveBtn")
    page.wait_for_function(
        f"""() => {{
            const s = document.querySelector('#solveStatus');
            const label = document.querySelector('#ssLabel').textContent;
            return ['done','cached','error'].includes(s.dataset.state) && label.includes('{board}');
        }}""",
        timeout=timeout_ms,
    )
    state = page.eval_on_selector("#solveStatus", "el => el.dataset.state")
    progress = int(page.eval_on_selector("#solveStatus", "el => el.dataset.progressCount"))
    return state, progress


def step_playwright():
    print("\n=== 3. Playwright rendering checks (cluster-driven, cache hits only) ===")
    from playwright.sync_api import sync_playwright

    # the fixture set: every configured pot type's tree entries of the static export index
    # that the registry reserves as expected-cached. Non-reserved boards (gate-run state in
    # a future re-export) and other tree ids (e.g. smoke-v0, unreachable through the
    # cluster) are skipped WITH a note, never silently.
    def pot_of_key(key):
        for pot, tid in verifier_boards.TREE_IDS.items():
            if key.endswith(f"_{tid}"):
                return pot
        return None

    index = json.loads((DATA_DIR / "index.json").read_text())
    reserved = set(verifier_boards.EXPECTED_CACHED)
    fixtures = [s for s in index["spots"]
                if pot_of_key(s["key"]) and "".join(s["board"]) in reserved]
    skipped = [s["key"] for s in index["spots"] if s not in fixtures]
    if skipped:
        print(f"  note: {len(skipped)} index entr{'y' if len(skipped) == 1 else 'ies'} outside "
              f"the registry's expected-cached tree set, skipped: {', '.join(skipped)}")
    if not fixtures:
        print("  FAIL  no registry-reserved fixtures in data/index.json")
        return False
    by_pot = {}
    for s in fixtures:
        by_pot[pot_of_key(s["key"])] = by_pot.get(pot_of_key(s["key"]), 0) + 1

    SERVER_LOG.parent.mkdir(exist_ok=True)
    server_log = open(SERVER_LOG, "w", encoding="utf-8")
    server = subprocess.Popen(
        [str(SERVER_EXE)], cwd=str(REPO),
        stdout=server_log, stderr=subprocess.STDOUT, text=True,
    )
    print(f"  Starting Vite dev server on port {PORT} ...")
    srv = subprocess.Popen(
        f"npm.cmd run dev -- --port {PORT} --strictPort",
        cwd=str(VIZ_DIR), shell=True,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True
    )

    try:
        time.sleep(3)  # Vite is fast; 3 s is enough
        if not wait_health():
            print("  FAIL  solve server never became healthy")
            return False

        ok = True
        with sync_playwright() as pw:
            browser = pw.chromium.launch()
            page = browser.new_page()

            page_errors = []
            console_errors = []
            page.on("console", lambda m: console_errors.append(m.text) if m.type == "error" else None)
            page.on("pageerror", lambda e: page_errors.append(str(e)))

            # navigate with retry until server is ready
            for attempt in range(12):
                try:
                    page.goto(URL, timeout=5000, wait_until="domcontentloaded")
                    break
                except Exception:
                    if attempt == 11:
                        print(f"  FAIL  could not reach {URL} after 12 attempts")
                        return False
                    time.sleep(0.5)

            page.wait_for_selector("#solveBtn", timeout=15000)
            print(f"  {len(fixtures)} fixtures from data/index.json "
                  f"({', '.join(f'{p}:{n}' for p, n in sorted(by_pot.items()))})")

            # ---- per-spot rendering checks ----
            for entry in fixtures:
                key = entry["key"]
                board = "".join(entry["board"])
                depth = entry["stack_depth_bb"]
                pot = pot_of_key(key)
                console_errors.clear()
                page_errors.clear()

                try:
                    state, progress = load_fixture(page, board, depth, pot)
                except Exception:
                    print(f"  FAIL  {key}: no terminal solve state within budget")
                    ok = False
                    continue
                if state != "cached" or progress != 0:
                    print(f"  FAIL  {key}: expected instant cache hit, got state={state} "
                          f"progress={progress} (fixture re-solved or missing from matrix/)")
                    ok = False
                    continue

                # wait for the full 13x13 grid (169 cells)
                try:
                    page.wait_for_function(
                        "() => document.querySelectorAll('.cell').length === 169",
                        timeout=10000
                    )
                except Exception:
                    n = page.eval_on_selector_all(".cell", "cs => cs.length")
                    print(f"  FAIL  {key}: grid has {n} cells (expected 169)")
                    ok = False
                    continue

                board_n = page.eval_on_selector_all(
                    ".board-area .card", "cs => cs.length"
                )
                hero_n = page.eval_on_selector_all(
                    ".seat--hero .card", "cs => cs.length"
                )
                bar_n = page.eval_on_selector_all(".bars", "cs => cs.length")
                detail = (page.text_content("#detail") or "").strip()
                all_errors = console_errors + page_errors

                spot_ok = (board_n == 3) and (hero_n == 2) and (bar_n > 0) and bool(detail) and not all_errors
                tag = "PASS" if spot_ok else "FAIL"
                print(
                    f"  {tag}  {key}  cached/0-progress  "
                    f"board={board_n}/3  hero={hero_n}/2  bars={bar_n}  "
                    f"detail={'ok' if detail else 'empty'}  "
                    f"errors={len(all_errors)}"
                )
                if all_errors:
                    for e in all_errors[:3]:
                        print(f"        error: {e}")
                if not spot_ok:
                    ok = False

            # ---- equity spot-check and aggregation reconciliation: 100bb_Ks7h2c ----
            ks_key = verifier_boards.spot_key(100, "Ks7h2c")
            print(f"\n  -- equity + aggregation spot-check: {ks_key} --")

            with open(DATA_DIR / f"{ks_key}.json") as f:
                raw = json.load(f)

            actions = raw["actions"]

            state, progress = load_fixture(page, "Ks7h2c", 100)
            if state != "cached":
                print(f"  FAIL  reconciliation load not cached (state={state})")
                ok = False
            page.wait_for_function(
                "() => document.querySelectorAll('.cell').length === 169",
                timeout=10000
            )

            # board-blocker check: AsKs must be absent (Ks is on the board)
            blocked = [h for h in raw["hands"] if h["hand"] == "AsKs"]
            aks_combos = [h for h in raw["hands"] if h["hand"] in ("AsKs", "AhKh", "AdKd", "AcKc")]
            n_aks = len(aks_combos)

            if blocked:
                print("  FAIL  AsKs (blocked by Ks board card) is present in JSON - blocker filtering broken")
                ok = False
            else:
                print(f"  PASS  AsKs absent (board-blocked); {n_aks} AKs combos in JSON")

            # expected aggregate for AKs
            exp_strat = [
                sum(h["strategy"][a] for h in aks_combos) / n_aks
                for a in range(len(actions))
            ]
            exp_eq = sum(h["equity"] for h in aks_combos) / n_aks

            # click the AKs cell via JS (locator parent traversal is safest cross-version)
            clicked = page.evaluate("""
                () => {
                    const lbl = [...document.querySelectorAll('.cell-label')]
                                 .find(el => el.textContent.trim() === 'AKs');
                    if (!lbl) return false;
                    lbl.closest('.cell').click();
                    return true;
                }
            """)
            if not clicked:
                print("  FAIL  AKs cell not found in grid")
                ok = False
            else:
                time.sleep(0.25)

                # read the two .stat-v values: [0]=EV, [1]=equity
                stat_texts = page.eval_on_selector_all(".stat-v", "vs => vs.map(v => v.textContent)")
                eq_text = stat_texts[1] if len(stat_texts) >= 2 else ""
                try:
                    eq_disp = float(eq_text.replace("%", "").replace("chips", "").strip())
                    diff = abs(eq_disp - exp_eq * 100)
                    tag = "PASS" if diff <= 0.2 else "FAIL"
                    print(
                        f"  {tag}  AKs equity displayed={eq_disp:.1f}%  "
                        f"expected={exp_eq * 100:.1f}%  diff={diff:.3f}%"
                    )
                    if tag == "FAIL":
                        ok = False
                except ValueError:
                    print(f"  FAIL  could not parse equity text: '{eq_text}'")
                    ok = False

                # strategy bars
                bar_texts = page.eval_on_selector_all(".dbar-val", "vs => vs.map(v => v.textContent)")
                for i, (act, exp_f) in enumerate(zip(actions, exp_strat)):
                    exp_pct = round(exp_f * 100)
                    if i < len(bar_texts):
                        raw_bar = bar_texts[i].replace("%", "").strip()
                        try:
                            disp_pct = float(raw_bar)
                            diff = abs(disp_pct - exp_pct)
                            tag = "PASS" if diff <= 1 else "FAIL"
                            print(f"  {tag}  {act}: bar={disp_pct:.0f}%  expected={exp_pct}%")
                            if tag == "FAIL":
                                ok = False
                        except ValueError:
                            print(f"  FAIL  could not parse bar for {act}: '{raw_bar}'")
                            ok = False
                    else:
                        print(f"  FAIL  no bar element for action {act} (index {i})")
                        ok = False

            # ---- 3bp weight-correctness reconciliation (operator acceptance item):
            # fractional 3-bet weights must survive export (HandEntry.weight = authored
            # mass) and the grid must aggregate by MASS (detail pane shows it when it
            # differs from the combo count). A failure here is a STOP + attribution,
            # never a silent re-fix (the 1e805da grid machinery is the thing under test).
            tbp_key = verifier_boards.spot_key(100, "9c5d3h", pot="3bp")
            print(f"\n  -- 3bp fractional-weight spot-check: {tbp_key} --")
            with open(DATA_DIR / f"{tbp_key}.json") as f:
                raw3 = json.load(f)
            w_by_hand = {h["hand"]: h.get("weight", 1.0) for h in raw3["hands"]}
            aa = [w for h, w in w_by_hand.items() if h[0] == "A" and h[2] == "A"]
            checks = [
                ("AA combos export weight 0.75",
                 len(aa) == 6 and all(abs(w - 0.75) < 1e-6 for w in aa)),
                ("an A5o combo exports weight 0.25",
                 abs(w_by_hand.get("Ah5c", 0.0) - 0.25) < 1e-6),
                ("TT absent (zero 3-bet capacity: SRP calls at 1.0)",
                 not any(h[0] == "T" and h[2] == "T" for h in w_by_hand)),
            ]
            for label_txt, passed in checks:
                print(f"  {'PASS' if passed else 'FAIL'}  {label_txt}")
                if not passed:
                    ok = False

            state, _ = load_fixture(page, "9c5d3h", 100, "3bp")
            if state != "cached":
                print(f"  FAIL  3bp reconciliation load not cached (state={state})")
                ok = False
            page.wait_for_function(
                "() => document.querySelectorAll('.cell').length === 169", timeout=10000)
            clicked = page.evaluate("""
                () => {
                    const lbl = [...document.querySelectorAll('.cell-label')]
                                 .find(el => el.textContent.trim() === 'AA');
                    if (!lbl) return false;
                    lbl.closest('.cell').click();
                    return true;
                }
            """)
            if not clicked:
                print("  FAIL  AA cell not found in the 3bp grid")
                ok = False
            else:
                time.sleep(0.25)
                detail_txt = (page.text_content("#detail") or "").strip()
                # 6 AA combos at authored mass 0.75 = 4.50; the detail pane only prints
                # mass when it differs from the combo count, so its presence IS the
                # weight-aware aggregation working end to end
                if "mass 4.50" in detail_txt:
                    print("  PASS  AA detail shows mass 4.50 (6 combos x 0.75 authored)")
                else:
                    print(f"  FAIL  AA detail lacks 'mass 4.50': '{detail_txt[:120]}'")
                    ok = False

            browser.close()

        return ok

    finally:
        for proc in (server, srv):
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
        server_log.close()


def main():
    print("=== Phase 2b Gate Verification ===\n")
    verifier_boards.prepare(REPO)
    results = [
        ("cargo check",   step_cargo_check()),
        ("npm build",     step_npm_build()),
        ("playwright",    step_playwright()),
    ]

    print("\n=== Summary ===")
    all_pass = True
    for name, passed in results:
        print(f"  {'PASS' if passed else 'FAIL'}  {name}")
        if not passed:
            all_pass = False

    if all_pass:
        print("\nGATE PASSED")
        sys.exit(0)
    else:
        print("\nGATE FAILED")
        sys.exit(1)


if __name__ == "__main__":
    main()
