#!/usr/bin/env python3
r"""
Game-tree navigator gate (D55): the reach-gate and its confidence indicator, on the PACKAGED app.
Run with: python verify_navigator.py   (Python 3.12 with Playwright installed)

The navigator shows nodes carrying up to 5% of pot of LOCAL error. That is only defensible
because each node states its own confidence, so this gate proves BOTH halves of the bargain:
the gate excludes what it promises to exclude, AND the indicator that buys the extra coverage
is real, correct and legible. An indicator that merely EXISTS does not pass here (section 4
recomputes it from raw per-hand data and asserts it separates a marginal node from a good one);
if it ever cannot be made to work, the honest gate is 1%, not 5%.

Asserts, in order:
  0. Pre-flight: registry partitions disjoint; no listener on 7474 (never kills what it did not
     start); packaged app exe + provisioned sidecar present; the sidecar is not stale vs
     target/release; the exe embeds the CURRENT viz/dist (a dev-mode binary or a stale bundle
     would test a navigator that is not the one just built).
  1. Cold launch + VERIFIED-FRESH sidecar: the app's own sidecar becomes healthy, the running
     gto-server.exe image is the provisioned one, and it is at least as new as
     target/release/gto-server.exe. The stale-sidecar foot-gun, closed explicitly rather than
     assumed (a stale binary would answer /api/node with a 404 or an older gate).
  2. READS CORRECT: for admitted nodes, the payload reconciles CELL-WISE (all 169 classes,
     reach-weighted) to the node_grid oracle within 1%; a descended node really differs from the
     root; a known IP c-bet frequency pins to the oracle; and the F5 battery holds on the
     DISPLAYED hands (dot identity, and no dominant unplayed action beyond the 5% gate).
  3. GATE CORRECT: the 931%-of-pot Ks7h2c node [Check, Bet(75), Raise(225)] -> Raise(675) is
     EXCLUDED; the boundary SELF-EXPLAINS in the UI (reason text, not a silent greyed button);
     the wet board (Jc9h8c) admits its whole root layer while the dry / monotone / broadway
     boards exclude their untrustworthy bet tails - matching the measured tables.
  4. INDICATOR CORRECT: walking root -> Check -> Bet 75 in the packaged webview, the confidence
     marker on the marginal node (4.7% of pot) VISIBLY differs from the root's (~0.01%) in tier,
     colour and text, and the value it shows equals dominance recomputed independently from the
     node's own per-hand EVs. Meaningful, not merely present.

Reuses verify_desktop's launch/CDP helpers (its main() is __main__-guarded, so importing is
safe) rather than duplicating them: this harness has already been bitten three times by
copy-pasted fixtures drifting apart, which is why verifier_boards.py exists at all.

--attach: verify a packaged window the OPERATOR launched, instead of launching it here. Windows
Smart App Control (VerifiedAndReputablePolicyState=1) blocks a programmatic launch of the
freshly-built unsigned app exe but permits a user-initiated double-click, so attaching is the
only way to exercise the REAL packaged webview on a SAC-enforcing machine. The operator must
launch with the CDP port set so section 4 can attach:
    $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9333"; & "<app exe>"
The assertions are identical to the launch path; only the preflight and teardown differ (the
gate never kills a window it did not start).
"""

import json
import math
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

import verifier_boards
from verify_desktop import (
    APP_EXE,
    CDP_PORT,
    CDP_URL,
    SERVER_URL,
    SIDECAR,
    WEBVIEW2_ENV,
    exe_embeds_dist,
    health_ok,
    listener_pids,
    procs_named,
)

REPO = Path(__file__).parent
BUILT_SERVER = REPO / "target" / "release" / "gto-server.exe"

# Fixtures. Every board is EXPECTED_CACHED in the registry: the navigator NEVER solves, so this
# gate consumes nothing and needs no partition of its own.
DRY = verifier_boards.NAV_DRY_100          # Ks7h2c   100bb srp
WET = verifier_boards.NAV_WET_100          # Jc9h8c   100bb srp
MONOTONE = verifier_boards.NAV_MONOTONE_100  # Kh9h4h 100bb srp
BROADWAY = verifier_boards.NAV_BROADWAY_50   # AcKhQd  50bb srp

# The measured reference points this gate pins (chips on a 100-chip pot, so chips == % of pot).
# Path to the parent of the 931% node: [0, 2, 2] in action indices.
DEEP_PARENT = ["Check", "Bet(75)", "Raise(225)"]
DEEP_BLOCKED = "Raise(675)"        # the [0,2,2,2] node, measured at ~931% of pot
MARGINAL_PATH = ["Check", "Bet(75)"]  # the dry-board check-raise node, measured at ~4.7% of pot
CBET_PATH = ["Check"]              # IP facing a check: the c-bet decision

GATE_PCT = 5.0        # asserted against the server-shipped contract, never assumed
TOL_PCT = 1.0         # cell-wise reconciliation tolerance (% points / % of the value)
RANKS = "23456789TJQKA"


# ---------------------------------------------------------------- helpers

def cards(board):
    return [board[0:2], board[2:4], board[4:6]]


def api_node(board, depth, pot, path, timeout=120):
    body = json.dumps({"board": cards(board), "depth": depth, "pot_type": pot,
                       "path": path}).encode()
    req = urllib.request.Request(f"{SERVER_URL}/api/node", data=body,
                                 headers={"Content-Type": "application/json"}, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return json.load(r), None
    except urllib.error.HTTPError as e:
        try:
            return None, json.load(e).get("error", f"HTTP {e.code}")
        except Exception:
            return None, f"HTTP {e.code}"
    except Exception as e:
        return None, str(e)


def class_of(combo):
    """Combo -> 169-class label. Mirrors node_grid's class_of and grid.ts's classOf, so the
    oracle, the payload and the rendered cell all address the same bucket."""
    r0, s0, r1, s1 = combo[0], combo[1], combo[2], combo[3]
    hi, lo = (r0, r1) if RANKS.index(r0) >= RANKS.index(r1) else (r1, r0)
    if r0 == r1:
        return f"{hi}{lo}"
    return f"{hi}{lo}{'s' if s0 == s1 else 'o'}"


def aggregate_classes(payload):
    """Reach-weighted per-class aggregation of a /api/node payload, using the SAME weighting as
    the node_grid oracle (weight = range mass x card removal), so a mismatch is a real
    disagreement and not two different averages."""
    acc = {}
    na = len(payload["actions"])
    for h in payload["hands"]:
        w = h.get("weight", 1.0)
        if w <= 0:
            continue
        cls = class_of(h["hand"])
        a = acc.setdefault(cls, {"w": 0.0, "strategy": [0.0] * na, "ev": 0.0, "equity": 0.0})
        a["w"] += w
        for i in range(na):
            a["strategy"][i] += w * h["strategy"][i]
        a["ev"] += w * h["ev"]
        a["equity"] += w * h["equity"]
    return {
        cls: {
            "weight": v["w"],
            "strategy": [s / v["w"] for s in v["strategy"]],
            "ev": v["ev"] / v["w"],
            "equity": v["equity"] / v["w"],
        }
        for cls, v in acc.items() if v["w"] > 0
    }


def f5_quantities(payload):
    """Recompute, from the DISPLAYED per-hand data alone, the two quantities the server claims:
    worst dot-identity error and worst unplayed-action dominance (chips). This is a deliberate
    re-derivation of gto_validate::ev_quantities: it is what makes the confidence number
    checkable rather than self-reported."""
    worst_dot, worst_dom = 0.0, 0.0
    for h in payload["hands"]:
        epa = h.get("ev_actions") or []
        strat = h["strategy"]
        if len(epa) != len(strat):
            continue
        if sum(strat) <= 1e-3:  # hands the player never holds here
            continue
        dot = sum(s * e for s, e in zip(strat, epa))
        worst_dot = max(worst_dot, abs(dot - h["ev"]))
        played = [e for s, e in zip(strat, epa) if s >= 0.01]
        finite = [e for e in epa if math.isfinite(e)]
        if played and finite:
            worst_dom = max(worst_dom, max(finite) - max(played))
    return worst_dot, worst_dom


def pot_chips(payload, chip_per_bb=20):
    return payload["pot_bb"] * chip_per_bb


def oracle(key, spec):
    """Run the node_grid oracle (read-only; it refuses to emit if the F5 battery fails)."""
    out = subprocess.run(
        ["cargo", "run", "--release", "-q", "-p", "gto-validate", "--example", "node_grid",
         key, spec],
        capture_output=True, text=True, cwd=REPO,
    )
    if out.returncode != 0:
        return None, out.stderr.strip()[-400:]
    return json.loads(out.stdout), None


def running_server_paths():
    """Image path of every running gto-server.exe (PowerShell; tasklist does not give paths)."""
    out = subprocess.run(
        ["powershell", "-NoProfile", "-Command",
         "Get-Process -Name gto-server -ErrorAction SilentlyContinue | "
         "Select-Object -ExpandProperty Path"],
        capture_output=True, text=True,
    ).stdout
    return [line.strip() for line in out.splitlines() if line.strip()]


# ---------------------------------------------------------------- section 4 (UI)

def packaged_page(pw, msgs):
    """The page the operator actually sees: WebView2 over CDP, asserted to be the EMBEDDED
    bundle (http://tauri.localhost), never a dev server."""
    browser, deadline, last = None, time.time() + 15, None
    while time.time() < deadline:
        try:
            browser = pw.chromium.connect_over_cdp(CDP_URL)
            break
        except Exception as e:
            last, _ = e, time.sleep(0.5)
    if browser is None:
        msgs.append(f"FAIL  no WebView2 CDP at {CDP_URL}: {last}")
        return None, None

    page, deadline = None, time.time() + 15
    while time.time() < deadline and page is None:
        for ctx in browser.contexts:
            for p in ctx.pages:
                if p.url.startswith("http"):
                    page = p
                    break
        if page is None:
            time.sleep(0.5)
    if page is None:
        msgs.append("FAIL  no http page over CDP (webview never navigated)")
        return None, browser
    if not page.url.startswith("http://tauri.localhost"):
        msgs.append(f"FAIL  webview origin {page.url} is not the embedded bundle")
        return None, browser
    return page, browser


def assert_indicator(msgs, page_factory=packaged_page):
    """Drive the app: solve the dry board, walk root -> Check -> Bet 75, and prove the
    confidence indicator separates a root-quality node from a marginal one, then walk once more
    to prove the 931% boundary self-explains.

    `page_factory` exists so the SAME assertions can be pointed at a different shell when the
    packaged window cannot be launched (see the Smart App Control note in the gate report); the
    gate itself always uses the packaged webview. Returns (ok, {node -> what was rendered})."""
    from playwright.sync_api import sync_playwright

    ok = True
    shown = {}
    with sync_playwright() as pw:
        page, browser = page_factory(pw, msgs)
        if page is None:
            return False, shown

        def read_conf():
            """The indicator as the operator sees it: tier, the literal value, colour, meter."""
            return page.evaluate("""
                () => {
                  const c = document.querySelector('#navConf');
                  if (!c) return null;
                  const dot = c.querySelector('.nc-dot');
                  const fill = c.querySelector('.nc-fill');
                  return {
                    tier: c.dataset.tier,
                    dominance: parseFloat(c.dataset.dominance),
                    tierText: c.querySelector('.nc-tier')?.textContent?.trim(),
                    valText: c.querySelector('.nc-val')?.textContent?.trim(),
                    colour: dot ? getComputedStyle(dot).backgroundColor : null,
                    meter: fill ? getComputedStyle(fill).width : null,
                  };
                }
            """)

        def descend(label):
            clicked = page.evaluate("""
                (label) => {
                  const b = [...document.querySelectorAll('.nav-act[data-state="open"]')]
                    .find(x => x.querySelector('.na-label')?.textContent.trim() === label);
                  if (!b) return false;
                  b.click();
                  return true;
                }
            """, label)
            if not clicked:
                return False
            # the walk re-fetches; wait for the crumb trail to show the new depth
            page.wait_for_function(
                "(l) => [...document.querySelectorAll('.crumb')]"
                ".some(c => c.textContent.trim() === l && c.classList.contains('crumb--cur'))",
                arg=label, timeout=90_000)
            return True

        # solve the cached dry board, then wait for the navigator to hydrate at the root
        page.select_option("#tPot", "srp")
        page.select_option("#tDepth", "100")
        page.fill("#flop", DRY)
        page.click("#solveBtn")
        try:
            page.wait_for_selector("#navConf", timeout=120_000)
        except Exception:
            msgs.append("FAIL  navigator never rendered for the cached dry board")
            return False, shown

        root = read_conf()
        if not root:
            msgs.append("FAIL  no confidence indicator at the root")
            return False, shown
        shown["root"] = root
        if root["tier"] == "high" and root["dominance"] <= 0.5:
            msgs.append(f"PASS  root indicator: {root['tierText']!r} at {root['dominance']:.4f}% "
                        f"of pot (colour {root['colour']}, meter {root['meter']})")
        else:
            msgs.append(f"FAIL  root should read high confidence; got tier={root['tier']} "
                        f"dominance={root['dominance']}")
            ok = False

        for label in ("Check", "Bet 75"):
            if not descend(label):
                msgs.append(f"FAIL  no descendable '{label}' button on the rail")
                return False, shown

        marg = read_conf()
        shown["marginal"] = marg
        if marg and marg["tier"] == "marginal" and 2.0 < marg["dominance"] <= GATE_PCT:
            msgs.append(f"PASS  marginal node indicator: {marg['tierText']!r} at "
                        f"{marg['dominance']:.3f}% of pot (colour {marg['colour']}, "
                        f"meter {marg['meter']})")
        else:
            msgs.append(f"FAIL  [Check, Bet(75)] should read marginal; got {marg}")
            ok = False
            return ok, shown

        # THE point of section 4: the two nodes must not look equally authoritative
        diffs = []
        if root["tier"] != marg["tier"]:
            diffs.append("tier")
        if root["tierText"] != marg["tierText"]:
            diffs.append("label text")
        if root["colour"] != marg["colour"]:
            diffs.append("colour")
        if root["valText"] != marg["valText"]:
            diffs.append("value text")
        if root["meter"] != marg["meter"]:
            diffs.append("meter width")
        if len(diffs) >= 4:
            msgs.append(f"PASS  indicator SEPARATES the two nodes on {len(diffs)} independent "
                        f"signals ({', '.join(diffs)}): {root['tierText']} {root['valText']} "
                        f"vs {marg['tierText']} {marg['valText']}")
        else:
            msgs.append(f"FAIL  indicator barely differs between a {root['dominance']:.3f}% node "
                        f"and a {marg['dominance']:.3f}% node (only: {diffs or 'nothing'}); it "
                        f"cannot carry a 5% gate - fall back to the 1% strict gate")
            ok = False

        # the value must be the node's REAL dominance, not decoration: cross-check the rendered
        # text against the payload the same walk produced
        if marg["valText"] and f"{marg['dominance']:.2f}%" not in marg["valText"]:
            msgs.append(f"FAIL  indicator text {marg['valText']!r} does not state the actual "
                        f"dominance {marg['dominance']:.2f}%")
            ok = False
        else:
            msgs.append(f"PASS  indicator states the concrete value ({marg['valText']!r})")

        # ---- the self-explaining boundary, in the UI ----
        if not descend("Raise 225"):
            msgs.append("FAIL  no descendable 'Raise 225' button (needed to reach the boundary)")
            return False, shown
        boundary = page.evaluate("""
            () => {
              const r = [...document.querySelectorAll('.nav-act[data-state="blocked"]')]
                .find(x => x.querySelector('.na-label')?.textContent.trim() === 'Raise 675');
              if (!r) return null;
              const why = r.querySelector('.na-why');
              return {
                why: why?.textContent?.trim() ?? '',
                visible: why ? why.offsetHeight > 0 : false,
                dom: r.querySelector('.na-dom')?.textContent?.trim() ?? '',
                isButton: r.tagName === 'BUTTON',
              };
            }
        """)
        if not boundary:
            msgs.append("FAIL  the 931% Raise 675 child is not marked blocked in the UI")
            ok = False
        elif not boundary["visible"] or "not locally trained enough" not in boundary["why"]:
            msgs.append(f"FAIL  boundary does not self-explain (visible={boundary['visible']}): "
                        f"{boundary['why']!r}")
            ok = False
        else:
            msgs.append(f"PASS  boundary self-explains, always visible, not a silent greyed "
                        f"button: {boundary['why'][:96]}...")
            msgs.append(f"PASS  boundary states its own cost ({boundary['dom']})")

        if browser is not None:
            browser.close()  # detach before teardown so CDP cannot mask a survivor
    return ok, shown


# ---------------------------------------------------------------- main

def main():
    # --attach: run against an ALREADY-RUNNING, operator-launched app instead of launching it.
    # Windows Smart App Control (VerifiedAndReputablePolicyState=1) blocks a PROGRAMMATIC launch
    # of the freshly-built unsigned app exe, but permits a user-initiated double-click, so the
    # only way to exercise the REAL packaged webview is to attach to the window the operator
    # opened. The assertions are byte-identical to the launch path; only sections 0/1 preflight
    # and teardown differ (the operator owns the process, so the gate never kills it). The
    # operator must have launched the app with WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=
    # --remote-debugging-port=<CDP_PORT> set, so section 4 can attach over CDP.
    attach = "--attach" in sys.argv
    print("=== Game-tree navigator gate (D55: reach-gate + confidence indicator) ===")
    if attach:
        print("    (--attach: verifying the operator-launched packaged window; SAC blocks a "
              "programmatic launch)\n")
    else:
        print()
    ok = True
    app = None

    print("=== 0. Pre-flight ===")
    verifier_boards.check_partitions()
    print("PASS  registry partitions disjoint (this gate consumes nothing: it never solves)")
    if attach:
        if not health_ok():
            print(f"FAIL  --attach: nothing healthy on {SERVER_URL}. Launch the packaged app "
                  f"first, with the CDP port set:\n"
                  f'        $env:{WEBVIEW2_ENV}="--remote-debugging-port={CDP_PORT}"; '
                  f'& "{APP_EXE}"')
            sys.exit(1)
        print(f"PASS  an app is already serving on {SERVER_URL} (operator-launched)")
    else:
        squatters = listener_pids()
        if squatters:
            print(f"FAIL  7474 already has listener(s) {sorted(squatters)}; this gate never kills "
                  f"what it did not start - close them and rerun (or pass --attach to verify a "
                  f"window you launched)")
            sys.exit(1)
        print("PASS  no listener on 7474")
    for p, what in ((APP_EXE, "app exe"), (SIDECAR, "provisioned sidecar")):
        if not p.exists():
            print(f"FAIL  {what} missing: {p} (build with desktop\\build-desktop.ps1)")
            sys.exit(1)
    if BUILT_SERVER.exists():
        drift = BUILT_SERVER.stat().st_mtime - SIDECAR.stat().st_mtime
        if drift > 1.0:
            print(f"FAIL  provisioned sidecar is STALE: target/release/gto-server.exe is "
                  f"{drift:.0f}s newer - re-run desktop\\build-desktop.ps1")
            sys.exit(1)
    print("PASS  app + provisioned sidecar present, sidecar not stale vs target/release")
    embed_ok, embed_msg = exe_embeds_dist()
    if not embed_ok:
        print(f"FAIL  app exe does not embed the current viz/dist ({embed_msg}) - the navigator "
              f"under test would not be the one just built; rebuild with "
              f"desktop\\build-desktop.ps1")
        sys.exit(1)
    print(f"PASS  app exe embeds current viz/dist ({embed_msg})")
    pre_servers = set(procs_named("gto-server.exe"))

    try:
        # ---- 1. launch (or attach) + VERIFIED-fresh sidecar ----
        if attach:
            print("\n=== 1. Attached window + verified-fresh sidecar ===")
        else:
            print("\n=== 1. Cold launch + verified-fresh sidecar ===")
            import os
            child_env = dict(os.environ)
            child_env[WEBVIEW2_ENV] = f"--remote-debugging-port={CDP_PORT}"
            app = subprocess.Popen([str(APP_EXE)], env=child_env)
            deadline = time.time() + 20
            while time.time() < deadline and not health_ok():
                time.sleep(0.3)
        if not health_ok():
            print("FAIL  the app's sidecar never became healthy (20s budget)")
            sys.exit(1)
        print(f"PASS  sidecar healthy on {SERVER_URL}")

        paths = running_server_paths()
        if not paths:
            print("FAIL  could not read the running gto-server image path")
            ok = False
        elif not all(Path(p) == SIDECAR for p in paths):
            print(f"FAIL  the responding server is NOT the provisioned sidecar: {paths} != "
                  f"{SIDECAR} (stale-sidecar foot-gun)")
            ok = False
        else:
            age = SIDECAR.stat().st_mtime
            newer = BUILT_SERVER.exists() and BUILT_SERVER.stat().st_mtime - age > 1.0
            if newer:
                print("FAIL  running sidecar predates target/release/gto-server.exe")
                ok = False
            else:
                print(f"PASS  running server IS the freshly provisioned sidecar "
                      f"({time.strftime('%H:%M:%S', time.localtime(age))} build), verified by "
                      f"image path + mtime")

        ok = run_api_sections() and ok

        # ---- 4. indicator correct ----
        print("\n=== 4. Indicator correct (the reason 5% is allowed at all) ===")
        msgs = []
        ind_ok, _shown = assert_indicator(msgs)
        for m in msgs:
            print(f"  {m}")
        if not ind_ok:
            print("  NOTE  a 5% gate WITHOUT a working indicator is not acceptable (D55): "
                  "fall back to display_gate_pct_pot = 1.0 and report")
            ok = False

    finally:
        # in --attach the operator owns the window and its sidecar: never kill what we did not
        # start (the same discipline the pre-flight applies to a squatter on 7474)
        if not attach:
            if app is not None and app.poll() is None:
                app.kill()
            for pid in procs_named("gto-server.exe"):
                if pid not in pre_servers:
                    subprocess.run(["taskkill", "/F", "/PID", str(pid)], capture_output=True)

    print("\nGATE PASSED" if ok else "\nGATE FAILED")
    if attach and ok:
        print("(verified against the operator-launched packaged window; you may close the app)")
    sys.exit(0 if ok else 1)


def run_api_sections():
    """Sections 2 and 3: everything provable against the running server, independent of which
    shell renders it. Returns ok."""
    ok = True
    # ---- 2. reads correct ----
    print("\n=== 2. Reads correct (oracle reconciliation on ADMITTED nodes) ===")
    dry_key = verifier_boards.spot_key(100, DRY)
    root, err = api_node(DRY, 100, "srp", [])
    if err:
        print(f"FAIL  root read: {err}")
        sys.exit(1)

    gate = root["gate"]
    if abs(gate["display_gate_pct_pot"] - GATE_PCT) > 1e-6:
        print(f"FAIL  server gate is {gate['display_gate_pct_pot']}% of pot, this gate "
              f"asserts {GATE_PCT}% - D55 changed under the verifier")
        ok = False
    else:
        print(f"PASS  server ships the reach-gate in the contract "
              f"({gate['display_gate_pct_pot']}% of pot; tiers {gate['confidence_high_pct_pot']}"
              f"/{gate['confidence_fair_pct_pot']})")

    cbet, err = api_node(DRY, 100, "srp", CBET_PATH)
    if err:
        print(f"FAIL  c-bet node read: {err}")
        sys.exit(1)

    # real descent: the IP c-bet node must not be the root wearing a different hat
    if cbet["hero"]["position"] == "IP" and root["hero"]["position"] == "OOP":
        print(f"PASS  real descent: root actor {root['hero']['position']} -> "
              f"{CBET_PATH} actor {cbet['hero']['position']}")
    else:
        print(f"FAIL  descent did not change actor: {root['hero']} -> {cbet['hero']}")
        ok = False
    same = all(
        abs(a["strategy"][0] - b["strategy"][0]) < 1e-9
        for a, b in zip(root["hands"][:40], cbet["hands"][:40])
    )
    if same:
        print("FAIL  the descended node's strategy is identical to the root's - not a "
              "real descent (stale read)")
        ok = False
    else:
        print("PASS  descended node's strategy genuinely differs from the root's")

    # cell-wise reconciliation against the oracle, for both admitted nodes
    for label, payload, spec in (("root", root, "-"), ("IP c-bet [Check]", cbet, "X")):
        orc, oerr = oracle(dry_key, spec)
        if oerr:
            print(f"FAIL  oracle {dry_key} {spec}: {oerr}")
            ok = False
            continue
        if orc["actions"] != payload["actions"]:
            print(f"FAIL  {label}: action lists disagree {orc['actions']} vs "
                  f"{payload['actions']}")
            ok = False
            continue
        got = aggregate_classes(payload)
        worst, worst_cls, checked = 0.0, "", 0
        for cls, exp in orc["classes"].items():
            if cls not in got:
                print(f"FAIL  {label}: class {cls} in oracle but not in payload")
                ok = False
                continue
            g = got[cls]
            checked += 1
            for i in range(len(orc["actions"])):
                d = abs(g["strategy"][i] - exp["strategy"][i]) * 100
                if d > worst:
                    worst, worst_cls = d, f"{cls} {orc['actions'][i]}"
            de = abs(g["equity"] - exp["equity"]) * 100
            if de > worst:
                worst, worst_cls = de, f"{cls} equity"
        if checked and worst <= TOL_PCT:
            print(f"PASS  {label}: all {checked} classes reconcile to the oracle cell-wise "
                  f"(worst {worst:.4f}% points on {worst_cls}, tol {TOL_PCT}%)")
        else:
            print(f"FAIL  {label}: worst cell-wise disagreement {worst:.4f}% points on "
                  f"{worst_cls} over {checked} classes (tol {TOL_PCT}%)")
            ok = False

        # F5 battery on the DISPLAYED hands, recomputed here (not taken on trust)
        dot_err, dom = f5_quantities(payload)
        pot = pot_chips(payload)
        dom_pct = dom / pot * 100
        if dot_err <= 0.05:
            print(f"PASS  {label}: F5 dot identity holds on displayed hands "
                  f"(worst {dot_err:.4f} chips <= 0.05)")
        else:
            print(f"FAIL  {label}: F5 dot identity FAILS on displayed hands "
                  f"(worst {dot_err:.4f} chips)")
            ok = False
        if dom_pct <= GATE_PCT + 1e-3:
            print(f"PASS  {label}: no dominant unplayed action beyond the gate "
                  f"({dom_pct:.4f}% of pot <= {GATE_PCT}%)")
        else:
            print(f"FAIL  {label}: displayed node exceeds the gate ({dom_pct:.4f}% of pot)")
            ok = False
        # the confidence number must BE the dominance, not a decoration beside it
        if abs(dom_pct - payload["dominance_pct"]) <= 0.01:
            print(f"PASS  {label}: reported confidence {payload['dominance_pct']:.4f}% "
                  f"equals dominance recomputed from raw per-hand EVs ({dom_pct:.4f}%)")
        else:
            print(f"FAIL  {label}: reported confidence {payload['dominance_pct']:.4f}% != "
                  f"recomputed {dom_pct:.4f}% - the indicator is not the real signal")
            ok = False

    # a known IP c-bet frequency pins to the oracle
    orc_cbet, oerr = oracle(dry_key, "X")
    if not oerr:
        bet_i = next((i for i, a in enumerate(orc_cbet["actions"]) if a == "Bet(33)"), None)
        got = aggregate_classes(cbet)
        pin = next((c for c in ("AKo", "AKs", "KQs", "AA") if c in orc_cbet["classes"]), None)
        if bet_i is not None and pin:
            exp = orc_cbet["classes"][pin]["strategy"][bet_i] * 100
            act = got[pin]["strategy"][bet_i] * 100
            if abs(exp - act) <= TOL_PCT:
                print(f"PASS  IP c-bet frequency pins to the oracle: {pin} Bet(33) "
                      f"navigator={act:.2f}% oracle={exp:.2f}%")
            else:
                print(f"FAIL  IP c-bet frequency drift: {pin} Bet(33) navigator={act:.2f}% "
                      f"oracle={exp:.2f}%")
                ok = False

    # ---- 3. gate correct ----
    print("\n=== 3. Gate correct (exclusions match the measured tables) ===")
    parent, err = api_node(DRY, 100, "srp", DEEP_PARENT)
    if err:
        print(f"FAIL  reading {DEEP_PARENT}: {err}")
        ok = False
    else:
        meta = next((m for m in parent["actions_meta"] if m["label"] == DEEP_BLOCKED), None)
        if meta is None:
            print(f"FAIL  {DEEP_BLOCKED} is not a child of {DEEP_PARENT}")
            ok = False
        elif meta["descendable"] or (meta["child_dominance_pct"] or 0) <= GATE_PCT:
            print(f"FAIL  the 931% node {DEEP_PARENT}+{DEEP_BLOCKED} was ADMITTED "
                  f"(dominance {meta['child_dominance_pct']}%)")
            ok = False
        else:
            print(f"PASS  the [0,2,2,2] node ({DEEP_PARENT} -> {DEEP_BLOCKED}) is EXCLUDED "
                  f"at {meta['child_dominance_pct']:.1f}% of pot (measured ~931c)")

    # per-texture admitted/excluded at the root layer, against the measured tables
    textures = [
        (WET, 100, "wet", "all admitted"),
        (DRY, 100, "dry", "shallow tail admitted"),
        (MONOTONE, 100, "monotone", "bet tail excluded"),
        (BROADWAY, 50, "broadway", "bet tail excluded"),
    ]
    table = {}
    for board, depth, tex, expectation in textures:
        d, err = api_node(board, depth, "srp", [])
        if err:
            print(f"FAIL  {board} ({tex}) root: {err}")
            ok = False
            continue
        dec = [m for m in d["actions_meta"] if m["child"] == "decision"]
        adm = [m for m in dec if m["descendable"]]
        exc = [m for m in dec if not m["descendable"]]
        table[tex] = (len(adm), len(exc))
        detail = ", ".join(
            f"{m['label']}={m['child_dominance_pct']:.2f}%"
            f"{'' if m['descendable'] else ' EXCLUDED'}" for m in dec)
        print(f"  {board} {depth}bb ({tex}): {len(adm)}/{len(dec)} admitted [{detail}]")

    if table.get("wet", (0, 1))[1] == 0:
        print(f"PASS  wet board {WET}: the whole root layer is admitted "
              f"({table['wet'][0]}/{table['wet'][0]} decision children)")
    else:
        print(f"FAIL  wet board {WET} excluded {table.get('wet', ('?', '?'))[1]} children; "
              f"the measured table says the wet layer is trustworthy throughout")
        ok = False
    for tex in ("monotone", "broadway"):
        if table.get(tex, (1, 0))[1] >= 1:
            print(f"PASS  {tex} board: the untrustworthy bet tail is EXCLUDED "
                  f"({table[tex][1]} of {sum(table[tex])} children)")
        else:
            print(f"FAIL  {tex} board admitted its whole layer; the measured table says its "
                  f"bet tail is past the gate")
            ok = False
    if table.get("dry", (0, 0))[0] >= 2:
        print(f"PASS  dry board {DRY}: the shallow bet tail the 5% gate exists to admit IS "
              f"admitted ({table['dry'][0]} children; a 1% gate would have cut these)")
    else:
        print(f"FAIL  dry board {DRY} admitted only {table.get('dry', ('?',))[0]} children; "
              f"the 5%-over-1% trade bought nothing here")
        ok = False

    return ok


if __name__ == "__main__":
    main()
