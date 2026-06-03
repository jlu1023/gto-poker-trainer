#!/usr/bin/env python3
"""
Phase 2b gate verification for GTO Lab viz.
Run with: C:\\Users\\james\\AppData\\Local\\Programs\\Python\\Python312\\python.exe verify_viz.py
"""

import json
import subprocess
import sys
import time
from pathlib import Path

PORT = 5247
REPO = Path(__file__).parent
VIZ_DIR = REPO / "viz"
DATA_DIR = VIZ_DIR / "public" / "data"
URL = f"http://localhost:{PORT}/"


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


def step_playwright():
    print("\n=== 3. Playwright rendering checks ===")
    from playwright.sync_api import sync_playwright

    print(f"  Starting Vite dev server on port {PORT} ...")
    srv = subprocess.Popen(
        f"npm.cmd run dev -- --port {PORT} --strictPort",
        cwd=str(VIZ_DIR), shell=True,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True
    )

    try:
        time.sleep(3)  # Vite is fast; 3 s is enough

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

            # wait for selector options to be populated (options inside <select> are not "visible")
            try:
                page.wait_for_function(
                    "() => document.querySelectorAll('#spot option').length > 0",
                    timeout=15000
                )
            except Exception:
                print("  FAIL  #spot selector never populated")
                return False

            spot_keys = page.eval_on_selector_all(
                "#spot option", "els => els.map(o => o.value)"
            )
            print(f"  {len(spot_keys)} spots in selector: {spot_keys}")

            # ---- per-spot rendering checks ----
            for key in spot_keys:
                console_errors.clear()
                page_errors.clear()

                page.select_option("#spot", key)

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
                    f"  {tag}  {key}  "
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
            ks_key = "100bb_Ks7h2c_flop_srp-v1"
            print(f"\n  -- equity + aggregation spot-check: {ks_key} --")

            with open(DATA_DIR / f"{ks_key}.json") as f:
                raw = json.load(f)

            actions = raw["actions"]

            page.select_option("#spot", ks_key)
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

            browser.close()

        return ok

    finally:
        srv.terminate()
        try:
            srv.wait(timeout=5)
        except subprocess.TimeoutExpired:
            srv.kill()


def main():
    print("=== Phase 2b Gate Verification ===\n")
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
