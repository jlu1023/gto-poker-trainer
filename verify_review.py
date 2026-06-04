#!/usr/bin/env python3
"""
Phase 5b gate verification: hand-history leak analysis.
Run with: C:\\Users\\james\\AppData\\Local\\Programs\\Python\\Python312\\python.exe verify_review.py

Covers:
  1. Builds + unit tests (parser/classifier/aggregator units incl. the D45 negative test and
     the hand-computed Poisson-binomial fixture run inside cargo test).
  2. Envelope + coverage: a synthetic JSON history exercising every skip reason classifies
     exactly; coverage reconciles with the input hand count; the pending-solve state is
     distinct from skipped (ZERO fresh solves in this gate).
  3. read_node F5 battery AT the [Check] node (operator gate addition): for every graded
     ip-after-check record: ev_mix == dot(p, ev_actions) exactly; played-action EV spread
     small; no unplayed action dominates; near-pure hands play their argmax.
  4. Reuse proof: a review-graded decision's numbers equal the trainer /api/grade of the same
     spot+combo (same solve path, no parallel grader).
  5. Persistence: re-ingestion is idempotent (all duplicates, zero appended).
  6. Report: aggregates with the D44 caveat shipped in the payload; tiny samples are never
     ranked; language tripwire on the built bundle (with the recorded caveat that the grep
     validates templates, not framing - the safety comes from the per-instance-rate model).
  7. UI round: review tab ingests PS text on cached boards, renders coverage + caveat +
     report, zero console errors. Existing-mode regressions run separately.
"""

import json
import shutil
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
STORE = MATRIX_DIR / "history" / "graded.jsonl"
STORE_BAK = MATRIX_DIR / "history" / "graded.jsonl.gate-bak"
SERVER_EXE = REPO / "target" / "release" / "gto-server.exe"

# boards guaranteed cached (Phase 1/3/4 solves, srp-v1 keys)
CACHED_100 = ["Ks7h2c", "9c5d3h", "Jc9h8c"]
CACHED_50 = ["AcKhQd", "Qs8s5h", "8d6s3c"]
# never-solved candidates for the pending-solve fixture (distinct from verify_solve's list)
PENDING_CANDIDATES = ["Th8c4d", "Jd8h5s", "Qc9d4s", "Th7s3h"]


def api(path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(
        f"{SERVER_URL}{path}", data=data, method="POST" if data else "GET",
        headers={"Content-Type": "application/json"} if data else {})
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            return r.status, json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        body_text = e.read().decode("utf-8", "replace")
        try:
            return e.code, json.loads(body_text)
        except json.JSONDecodeError:
            return e.code, body_text


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


def ev_action(seat, kind, amount=0.0):
    return {"seat": seat, "kind": kind, "amount_bb": amount, "all_in": False}


def srp_preflop(open_bb=2.5):
    return [ev_action("btn", "raise", open_bb), ev_action("bb", "call", open_bb - 1.0)]


def hand(hid, **kw):
    base = {
        "hand_id": hid, "source": "gate", "table_size_dealt": 2, "hero_seat": "bb",
        "stacks_bb": {"btn": 100.0, "bb": 100.0}, "blinds_bb": [0.5, 1.0], "ante_bb": 0.0,
        "hero_cards": "AhKh", "preflop": srp_preflop(),
        "flop": {"cards": ["Ks", "7h", "2c"],
                 "actions": [ev_action("bb", "check")]},
        "turn": None, "river": None,
    }
    base.update(kw)
    return base


def build_fixture(pending_board):
    """Synthetic JSON history: every skip reason + gradeable decisions on cached boards."""
    h = []
    # gradeable, cached
    h.append(hand("g1"))  # BB root check, 100bb Ks7h2c, AhKh
    h.append(hand("g2", hero_cards="9h9d",
                  flop={"cards": ["9c", "5d", "3h"],
                        "actions": [ev_action("bb", "bet", 1.7)]}))  # Bet(33) via tolerance
    h.append(hand("g3", hero_seat="btn", hero_cards="9s9h",
                  stacks_bb={"btn": 50.0, "bb": 50.0},
                  flop={"cards": ["Ac", "Kh", "Qd"],
                        "actions": [ev_action("bb", "check"), ev_action("btn", "check")]}))
    h.append(hand("g4", hero_seat="btn", hero_cards="AdKd",
                  stacks_bb={"btn": 50.0, "bb": 50.0},
                  flop={"cards": ["Qs", "8s", "5h"],
                        "actions": [ev_action("bb", "check"), ev_action("btn", "bet", 3.75)]}))
    h.append(hand("g5", hero_seat="btn", hero_cards="AsAh",
                  flop={"cards": ["Jc", "9h", "8c"],
                        "actions": [ev_action("bb", "check"), ev_action("btn", "check")]}))
    # gradeable + turn skip in the same hand
    h.append(hand("g6", hero_cards="AcKc",
                  turn={"card": "Td", "actions": [ev_action("bb", "check"), ev_action("btn", "check")]}))
    # gradeable root + facing-bet later in the same hand
    h.append(hand("g7", hero_cards="AdKh",
                  flop={"cards": ["Ks", "7h", "2c"],
                        "actions": [ev_action("bb", "check"), ev_action("btn", "bet", 1.65),
                                    ev_action("bb", "call", 1.65)]}))
    # pending: gradeable shape on a NEVER-solved board (must NOT be solved by this gate)
    h.append(hand("p1", hero_cards="AsAc",
                  stacks_bb={"btn": 50.0, "bb": 50.0},
                  flop={"cards": [pending_board[0:2], pending_board[2:4], pending_board[4:6]],
                        "actions": [ev_action("bb", "check")]}))
    # skip reasons
    h.append(hand("s1", table_size_dealt=6))                                  # not-heads-up
    h.append(hand("s2", preflop=[ev_action("btn", "raise", 2.5), ev_action("bb", "raise", 9.0),
                                 ev_action("btn", "call", 6.5)]))             # 3-bet-pot
    h.append(hand("s3", preflop=[ev_action("btn", "call", 0.5), ev_action("bb", "check")]))  # limped
    h.append(hand("s4", preflop=srp_preflop(3.0)))                            # open-size-out-of-band
    h.append(hand("s5", stacks_bb={"btn": 75.0, "bb": 75.0}))                 # depth-out-of-band
    h.append(hand("s6", flop={"cards": ["Ks", "7h", "2c"],
                              "actions": [ev_action("bb", "bet", 2.5)]}))     # off-tree 50% pot
    h.append(hand("s7", hero_cards=None))                                     # hero-cards-unknown
    h.append(hand("s8", preflop=[ev_action("btn", "raise", 2.5), ev_action("bb", "fold")],
                  flop=None))                                                  # hero-folded-preflop
    h.append(hand("s9", ante_bb=0.5))                                          # ante-pot
    return {"hands": h}


def main():
    print("=== Phase 5b Gate Verification ===\n")
    all_pass = True

    # ---- 1. builds + units ----
    print("=== 1. Builds + unit tests ===")
    for name, cmd, cwd, shell in [
        ("cargo check", ["cargo", "check", "--workspace", "--all-targets"], REPO, False),
        ("cargo test (incl. D45 negative + Poisson-binomial fixture)",
         ["cargo", "test", "--workspace"], REPO, False),
        ("npm build", "npm.cmd run build", VIZ_DIR, True),
    ]:
        r = subprocess.run(cmd, cwd=str(cwd), capture_output=True, text=True, shell=shell)
        ok = r.returncode == 0
        print(f"{'PASS' if ok else 'FAIL'}  {name}")
        if not ok:
            print((r.stderr or r.stdout)[-2000:])
            all_pass = False

    # ---- language tripwire on the built bundle ----
    print("\n=== 2. Language tripwire (validates templates, not framing) ===")
    bundle = next((VIZ_DIR / "dist" / "assets").glob("*.js")).read_text(encoding="utf-8", errors="replace").lower()
    banned = ["you should", "next time", "instead you", "you must", "play this", "always bet", "never bet"]
    hits = [b for b in banned if b in bundle]
    if hits:
        print(f"FAIL  prescriptive phrasing in bundle: {hits}")
        all_pass = False
    else:
        print("PASS  no prescriptive phrasing in the built bundle (note: framing safety comes "
              "from the per-instance-rate model, not this grep)")

    # ---- backup any real store; the gate runs on a clean one ----
    STORE.parent.mkdir(parents=True, exist_ok=True)
    had_real_store = STORE.exists()
    if had_real_store:
        shutil.move(str(STORE), str(STORE_BAK))

    manifest = json.loads((MATRIX_DIR / "manifest.json").read_text())
    pending_board = next(
        (b for b in PENDING_CANDIDATES if f"50bb_{b}_flop_srp-v1" not in manifest["spots"]), None)
    if pending_board is None:
        print("FAIL  no uncached pending candidate left")
        sys.exit(1)
    print(f"\npending-solve fixture board: {pending_board} (must remain unsolved)")

    server = subprocess.Popen([str(SERVER_EXE)], cwd=str(REPO),
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    try:
        if not wait_health():
            print("FAIL  server not healthy")
            sys.exit(1)

        # ---- 3. ingest the synthetic history: envelope + coverage ----
        print("\n=== 3. Envelope + coverage (synthetic JSON, every skip reason) ===")
        fixture = build_fixture(pending_board)
        st, ing = api("/api/review/ingest",
                      {"format": "json", "content": json.dumps(fixture), "source": "gate"})
        if st != 200:
            print(f"FAIL  ingest: HTTP {st} {ing}")
            sys.exit(1)
        cov = ing["coverage"]
        expect_reasons = {
            "not-heads-up": 1, "3-bet-pot": 1, "limped-pot": 1, "open-size-out-of-band": 1,
            "depth-out-of-band": 1, "off-tree-hero-size": 1, "hero-cards-unknown": 1,
            "hero-folded-preflop": 1, "ante-pot": 1, "turn-decision": 1, "facing-bet-on-flop": 1,
            "gradeable-needs-solve": 1,
        }
        for reason, min_count in expect_reasons.items():
            got = cov.get(reason, 0)
            tag = "PASS" if got >= min_count else "FAIL"
            print(f"  {tag}  coverage[{reason}] = {got} (>= {min_count})")
            if tag == "FAIL":
                all_pass = False
        if ing["hands"] == len(fixture["hands"]):
            print(f"  PASS  hand count reconciles: {ing['hands']} in == {len(fixture['hands'])} parsed")
        else:
            print(f"  FAIL  hands {ing['hands']} != fixture {len(fixture['hands'])}")
            all_pass = False
        gradeable = ing["gradeable"]
        cached_g = [g for g in gradeable if g["status"] == "cached"]
        fresh_g = [g for g in gradeable if g["status"] == "fresh"]
        if len(cached_g) == 7 and len(fresh_g) == 1:
            print(f"  PASS  gradeable split: {len(cached_g)} cached + {len(fresh_g)} fresh (p1)")
        else:
            print(f"  FAIL  gradeable split: {len(cached_g)} cached / {len(fresh_g)} fresh (want 7/1)")
            all_pass = False
        if "coverage_note" in ing and "roadmap" in ing["coverage_note"]:
            print("  PASS  expectation-setting coverage note present")
        else:
            print("  FAIL  coverage note missing")
            all_pass = False

        # ---- 4. grade WITHOUT solving the pending board ----
        print("\n=== 4. Grading (cached only; pending stays pending; zero fresh solves) ===")
        st, gr = api("/api/review/grade",
                     {"decisions": [g["decision"] for g in gradeable], "source_file": "gate"})
        if st != 200:
            print(f"FAIL  grade: HTTP {st} {gr}")
            sys.exit(1)
        ok = (gr["graded"] == 7 and gr["appended"] == 7 and gr["duplicates"] == 0
              and len(gr["pending"]) == 1 and len(gr["errors"]) == 0)
        print(f"  {'PASS' if ok else 'FAIL'}  graded {gr['graded']}/7, appended {gr['appended']}, "
              f"pending {len(gr['pending'])}/1, errors {len(gr['errors'])}")
        if not ok:
            print(f"        detail: {json.dumps(gr)[:400]}")
            all_pass = False

        # idempotency: same batch again -> all duplicates
        st, gr2 = api("/api/review/grade",
                      {"decisions": [g["decision"] for g in gradeable], "source_file": "gate"})
        if gr2["appended"] == 0 and gr2["duplicates"] == 7:
            print(f"  PASS  re-ingestion idempotent: 0 appended, 7 duplicates")
        else:
            print(f"  FAIL  idempotency: appended {gr2['appended']}, duplicates {gr2['duplicates']}")
            all_pass = False

        # ---- 5. read_node F5 battery AT the [Check] node ----
        print("\n=== 5. F5 battery at the ip-after-check node (gate addition) ===")
        records = [json.loads(l) for l in STORE.read_text().splitlines() if l.strip()]
        ip_records = [r for r in records if r["node_shape"] == "ip-after-check"]
        if len(ip_records) >= 3:
            print(f"  PASS  {len(ip_records)} ip-after-check records graded (g3, g4, g5)")
        else:
            print(f"  FAIL  only {len(ip_records)} ip-after-check records")
            all_pass = False
        worst_dot, worst_spread, worst_dom, purest = 0.0, 0.0, 0.0, None
        for r in ip_records:
            dot = sum(p * e for p, e in zip(r["p"], r["ev_actions"]))
            worst_dot = max(worst_dot, abs(dot - r["ev_mix"]))
            played = [e for p, e in zip(r["p"], r["ev_actions"]) if p >= 0.10]
            if len(played) > 1:
                worst_spread = max(worst_spread, max(played) - min(played))
            best_played = max((e for p, e in zip(r["p"], r["ev_actions"]) if p >= 0.01),
                              default=max(r["ev_actions"]))
            worst_dom = max(worst_dom, max(r["ev_actions"]) - best_played)
            mp = max(r["p"])
            if purest is None or mp > max(purest["p"]):
                purest = r
        t1 = "PASS" if worst_dot < 0.05 else "FAIL"
        t2 = "PASS" if worst_spread <= 2.0 else "FAIL"
        t3 = "PASS" if worst_dom <= 1.0 else "FAIL"
        print(f"  {t1}  ev_mix == dot(p, ev_actions): max |err| {worst_dot:.4f} chips")
        print(f"  {t2}  played-action EV spread at [Check] node: worst {worst_spread:.3f} chips "
              f"(F5 would show 30+)")
        print(f"  {t3}  no dominant unplayed action: worst {worst_dom:.3f} chips")
        if "FAIL" in (t1, t2, t3):
            all_pass = False
        if purest and max(purest["p"]) >= 0.90:
            am = purest["p"].index(max(purest["p"]))
            loss = max(purest["ev_actions"]) - purest["ev_actions"][am]
            t = "PASS" if loss <= 0.2 else "FAIL"
            print(f"  {t}  near-pure IP hand {purest['combo']} ({max(purest['p'])*100:.0f}% "
                  f"{purest['actions'][am]}): argmax loss {loss:.3f} chips")
            if t == "FAIL":
                all_pass = False

        # ---- 6. reuse proof: review record == trainer grade ----
        print("\n=== 6. Reuse proof (review path == trainer /api/grade) ===")
        g1 = next(r for r in records if r["hand_id"] == "g1")
        st, tg = api("/api/grade", {"board": ["Ks", "7h", "2c"], "depth": 100,
                                    "hand": "AhKh", "action": "Check"})
        ci = g1["actions"].index("Check")
        freq_ok = abs(tg["frequency"] - g1["p"][ci]) < 1e-5
        ev_ok = abs(tg["ev_chosen"] - g1["ev_actions"][ci]) < 1e-3
        best_ok = abs(tg["ev_best"] - max(g1["ev_actions"])) < 1e-3
        ok = st == 200 and freq_ok and ev_ok and best_ok
        print(f"  {'PASS' if ok else 'FAIL'}  AhKh@Ks7h2c Check: review p={g1['p'][ci]:.4f} vs "
              f"trainer freq={tg['frequency']:.4f}; ev/best agree: {ev_ok}/{best_ok}")
        if not ok:
            all_pass = False

        # ---- 7. report ----
        print("\n=== 7. Aggregate report ===")
        st, rep = api("/api/review/report")
        report = rep.get("report", {})
        ok = (st == 200 and report.get("total_records") == 7
              and report.get("distinct_hands") == 7
              and "range-conditioned" in report.get("comparator_caveat", ""))
        print(f"  {'PASS' if ok else 'FAIL'}  report: {report.get('total_records')} records, "
              f"{report.get('distinct_hands')} hands, D44 caveat shipped in payload")
        if not ok:
            all_pass = False
        ranked = [n for n in report.get("nodes", []) if n.get("ranked")]
        if not ranked:
            print(f"  PASS  no node ranked as a leak at these tiny samples (min-n guard holds)")
        else:
            print(f"  FAIL  {len(ranked)} nodes ranked off <10 instances")
            all_pass = False

        # pending board must still be unsolved (zero fresh solves in this gate)
        manifest2 = json.loads((MATRIX_DIR / "manifest.json").read_text())
        if f"50bb_{pending_board}_flop_srp-v1" not in manifest2["spots"]:
            print(f"  PASS  {pending_board} still unsolved (gate ran zero fresh solves)")
        else:
            print(f"  FAIL  {pending_board} was solved during the gate")
            all_pass = False

        # ---- 8. UI round (PS text on cached boards) ----
        print("\n=== 8. Review UI (Playwright, PS text, cached boards) ===")
        ps_text = (
            "PokerStars Hand #555000001:  Hold'em No Limit ($0.25/$0.50 USD) - 2026/06/04 01:00:00 ET\n"
            "Table 'Gate' 2-max Seat #1 is the button\n"
            "Seat 1: Villain ($50 in chips)\n"
            "Seat 2: Hero ($50 in chips)\n"
            "Villain: posts small blind $0.25\n"
            "Hero: posts big blind $0.50\n"
            "*** HOLE CARDS ***\n"
            "Dealt to Hero [Ah Kh]\n"
            "Villain: raises $0.75 to $1.25\n"
            "Hero: calls $0.75\n"
            "*** FLOP *** [Ks 7h 2c]\n"
            "Hero: checks\n"
            "Villain: checks\n"
            "*** SUMMARY ***\n"
            "Total pot $2.50 | Rake $0.06\n"
        )
        from playwright.sync_api import sync_playwright
        vite = subprocess.Popen(
            f"npm.cmd run dev -- --port {VITE_PORT} --strictPort",
            cwd=str(VIZ_DIR), shell=True,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        try:
            with sync_playwright() as pw:
                browser = pw.chromium.launch()
                page = browser.new_page()
                console_errors = []
                page.on("console", lambda m: console_errors.append(m.text) if m.type == "error" else None)
                page.on("pageerror", lambda e: console_errors.append(str(e)))
                for attempt in range(12):
                    try:
                        page.goto(APP_URL, timeout=5000, wait_until="domcontentloaded")
                        break
                    except Exception:
                        if attempt == 11:
                            raise
                        time.sleep(0.5)
                page.wait_for_selector("#modeReview", timeout=15000)
                page.click("#modeReview")
                page.fill("#rvText", ps_text)
                page.click("#rvAnalyze")
                page.wait_for_function(
                    "() => !document.querySelector('#rvReport').hidden", timeout=30000)
                caveat = page.eval_on_selector(".rv-caveat", "el => el.textContent") or ""
                cov_rows = page.eval_on_selector_all(".rv-cov-row", "rs => rs.length")
                ok = "range-conditioned" in caveat and cov_rows > 0 and not console_errors
                print(f"  {'PASS' if ok else 'FAIL'}  review UI: coverage rows {cov_rows}, "
                      f"D44 caveat rendered, console errors {len(console_errors)}")
                if console_errors:
                    print(f"        first: {console_errors[0]}")
                if not ok:
                    all_pass = False
                # 1022-hand UI total is now in the store (the PS hand was graded via the UI)
                browser.close()
        finally:
            vite.terminate()
            try:
                vite.wait(timeout=5)
            except subprocess.TimeoutExpired:
                vite.kill()
    finally:
        server.terminate()
        try:
            out, _ = server.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            server.kill()
            out, _ = server.communicate()
        # restore the operator's real store
        if STORE.exists():
            STORE.unlink()
        if had_real_store and STORE_BAK.exists():
            shutil.move(str(STORE_BAK), str(STORE))

    solving_lines = [l for l in (out or "").splitlines() if "solving" in l]
    if not solving_lines:
        print(f"\nPASS  server log: zero fresh solves across the entire gate")
    else:
        print(f"\nFAIL  unexpected solves: {solving_lines}")
        all_pass = False

    print("\n" + ("GATE PASSED" if all_pass else "GATE FAILED"))
    sys.exit(0 if all_pass else 1)


if __name__ == "__main__":
    main()
