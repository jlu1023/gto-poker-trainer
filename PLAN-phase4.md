# PLAN-phase4.md - GTO Poker Bot Phase 4 (manual trainer)

Status: PLAN (awaiting approval). Builds on Phase 3 (commit a223bcc).
No em dashes. Authoritative master plan: vault `GTO Poker Bot.md`.

## Goal

The operator enters a spot (depth + board + their exact hand) and commits to an action BEFORE
seeing any solver output. The tool then solves or cache-hits via the existing Phase 3 server,
reveals the solver's strategy for that hand, and grades the committed action by frequency and
EV loss. Commit-then-reveal; GTO is mixed, so the verdict is never a naive right/wrong.

## Scope (fixed)

- Single-raised pots only; depths 50bb and 100bb (both have authored ranges; 100bb has Phase 1
  cached solves). Unsupported depth/pot type = clear error, never guessed ranges.
- The Phase 3 solve server and cache are reused wholesale; the trainer adds two thin endpoints
  and a front-end mode. No fork of the solve path.
- The operator's hand is an INPUT: one specific two-card combo (e.g. AhKh), graded from its own
  row in the solved root, not from the 169-class aggregate.

---

## 1. Key decisions

### D32. Grading model: two numbers + banded verdict, mixed-strategy aware

For the operator's hand `h` and chosen root action `a`:

- **Frequency** = `strategy[h, a]` (how often the solver takes `a` with this exact combo).
- **EV loss** = `max_a' EV[h, a'] - EV[h, a]`, reported in chips AND as a fraction of the pot.

Verdict precedence (first match wins), bands as config tunables:

| Verdict | Rule | Default band |
|---|---|---|
| `Correct` | ev_loss <= `correct_eps_pct_pot` | 0.5% pot |
| `FineMix` | frequency >= `mix_freq_floor` AND ev_loss <= `minor_leak_max_pct_pot` | freq >= 10%, loss <= 2% pot |
| `MinorLeak` | ev_loss <= `minor_leak_max_pct_pot` | <= 2% pot |
| `ClearLeak` | otherwise | > 2% pot |

Rationale for `correct_eps_pct_pot = 0.005`: the matrix converges to 0.5% of pot
exploitability, so per-action EVs carry solver noise of that order; a finer epsilon would
grade noise. At equilibrium every action in the mixing support has near-equal EV, so any
meaningfully-mixed action lands in `Correct` or `FineMix` by construction; a low-frequency
near-zero-loss pick reads `FineMix` ("solver does this sometimes; it costs ~nothing"), never
a punished mistake.

Edge cases (all explicit, all tested):
- Chosen action within epsilon of best -> `Correct` regardless of frequency.
- Hand absent from the root (board-blocked combo) -> error "hand collides with the board".
- Hand present but reach weight ~0 (BB folds it preflop) -> error "hand not in the BB's range
  for this spot"; detected pre-commit via the setup endpoint (below), re-checked at grade time.
- Off-tree action (not in the solved action set) -> mapped to the nearest available action of
  the same kind (Bet -> closest bet size; else kind fallback), response carries a
  `mapped_from` notice the UI must surface. Defensive only: the commit buttons come from the
  legal set, so this fires only on direct API use or tree drift.

### D33. Per-action EV plumbing (the data grading needs)

The stored root (`NodeStrategy`) today has per-hand strategy but only strategy-weighted EV.
Grading needs `EV[h, a]` per action. VERIFIED against the pinned postflop-solver source:
`expected_values(player)` works at any node, including when `current_player() != player`
(interpreter.rs:672 returns the per-hand detail in exactly that case). So:

- `NodeStrategy` gains `ev_per_action: Vec<f32>` (layout `[hand + action * n]`, marked
  `#[serde(default)]` so existing summary sidecars still deserialize).
- `read_root` computes it: for each root action `a`: `back_to_root()`, `play(a)`,
  `cache_normalized_weights()`, `expected_values(root_player)` -> column `a`. Terminal or
  chance children would yield NaN guards; at the srp-v1 root every child is an IP player node,
  so the guard is dormant.
- `load_solution` already recomputes the root from the reloaded tree, so EVERY cached spot
  (Phase 1 batch included) gets per-action EV on load with no re-store and no migration.
- `SpotContract.hands[i]` gains `ev_actions: Vec<f32>` (aligned with `actions`); the explore
  detail panel shows per-action EV too (free upgrade, keeps one contract).

### D34. Trainer endpoints: two thin additions on the existing server

```
GET  /api/setup?depth=50&board=8d6s3c&hand=AhKh
     -> { "key": "50bb_8d6s3c_flop_srp-v1", "actions": ["Check","Bet(33)","Bet(75)"],
          "hand_in_range": true, "cached": true }
     -> 200 with {"error": "..."} style SSE-free JSON error for illegal input
```
- `actions` derive from `ActionTree::new(tree_config)` for the requested depth: VERIFIED the
  pinned crate exposes `ActionTree::available_actions()` (action_tree.rs:304); the action tree
  is board-independent and costs milliseconds, no game build, no solve. Labels are rendered by
  the same `render_action` the solve path uses, so they match the solved contract exactly.
- `hand_in_range` uses `Range::get_weight_by_cards` (range.rs:557) on the authored OOP range:
  preflop chart data (an input the operator authored), NOT solver output; no strategy leaks.
- `cached` tells the UI whether to expect an instant reveal or a progress strip. It is a
  manifest-key lookup, leaks no strategy.

```
POST /api/solve   body: {"board": [...], "depth": 50}
```
- Phase 3 request gains `depth` (default 50, back-compat with the explore input). Allowlist
  {50, 100}; anything else returns the ranges_for error ("no SRP ranges authored for Nbb").
  The cache key already includes depth via `spot_key`, so nothing else changes.

```
POST /api/grade   body: {"depth": 50, "board": ["8d","6s","3c"], "hand": "AhKh",
                         "action": "Bet(33)"}
     -> { "verdict": "FineMix", "frequency": 0.31, "ev_loss_chips": 0.42,
          "ev_loss_pct_pot": 0.0042, "best_action": "Bet(75)", "chosen_action": "Bet(33)",
          "mapped_from": null, "hand": "AhKh",
          "distribution": [ {"action":"Check","frequency":0.12,"ev":243.1}, ... ] }
```
- Requires the spot to already be in the matrix (the trainer always solves first; direct API
  use on an unsolved board gets "spot not solved yet; solve it first").
- Load path: summary sidecar first; if its root lacks `ev_per_action` (entries stored before
  this phase), fall back to `load_solution` (recomputes from the tree, ~0.5 s). Fresh stores
  write the extended summary, so the fallback decays away.
- Grading itself is the engine-side pure function (below); the endpoint just resolves indices
  and serializes.

### D35. Commit-then-reveal: enforced by data absence, not by hiding

Before commit, the client possesses ONLY the setup payload: action labels, a boolean, a cache
flag. No strategy, EV, equity, or grid data exists client-side, and the trainer fires no
`/api/solve` and no `data/*.json` fetch until the commit click. There is nothing to peek at:
revealing early is impossible rather than merely hidden.

Gate test: Playwright intercepts network traffic and asserts zero solve/grade/static-spot
requests between entering the spot and clicking commit, and that no `.grid` / `.detail`
content renders in trainer mode pre-commit. (The setup request is allowed; it carries labels
only.)

### D36. Verdict bands in config (single source of truth)

New `[trainer]` section in `gto-config` (with `default.toml` documentation):

```toml
[trainer]
correct_eps_pct_pot = 0.005     # within solver convergence noise -> Correct
minor_leak_max_pct_pot = 0.02   # above this -> ClearLeak
mix_freq_floor = 0.10           # solver frequency >= this + small loss -> FineMix
```

The server loads them with the rest of `phase1.toml` (serde defaults if absent) and applies
them in `/api/grade`. The front-end never hardcodes bands; it renders the verdict it is given.

### D37. Session history: in scope, in-memory only

A simple list in the trainer pane: one row per graded spot (board, depth, hand, committed
action, verdict, EV loss), plus a running total and average EV loss (% pot). Lives in page
state; reload clears it. Persistence, spaced repetition, and randomized spot generation are
explicitly deferred.

---

## 2. Trainer flow (state machine)

```
ENTRY    operator sets depth (50/100 select), board (Ks7h2c), hand (AhKh)
         -> client validates: legal cards, 5 distinct cards (3 board + 2 hand, no collision)
         -> GET /api/setup: legal actions + hand_in_range (+ cached flag)
         -> hand_in_range false -> inline error, no commit buttons
  |
COMMIT   the legal actions render as buttons; NOTHING else on screen is solver-derived
         -> operator clicks one action; this click IS the commitment (logged into history)
  |
SOLVING  POST /api/solve (SSE): cached -> complete arrives immediately;
         fresh -> the existing convergence strip streams (operator already committed)
  |
REVEAL   POST /api/grade -> grade card renders (verdict, frequency, EV loss chips + % pot,
         full distribution with per-action EV, best action highlighted, mapped_from notice
         if any) + the normal grid/table render with the operator's hand cell pre-selected
         and hero cards shown on the felt
  |
NEXT     "next spot" resets to ENTRY (history row appended)
```

Explore mode (prebuilt selector + Phase 3 flop input) is untouched; a topbar mode toggle
switches between explore and train.

---

## 3. Changes by component

### gto-engine
- `NodeStrategy.ev_per_action: Vec<f32>` (`#[serde(default)]`).
- `read_root`: compute per-action EV columns via play/cache/expected_values per action.
- New `grade` module (pure, no IO):
  - `find_hand(node, "AhKh") -> Option<usize>` (normalizes card order: "KhAh" == "AhKh").
  - `map_action(actions, requested) -> (usize, Option<String>)` nearest-by-kind mapping.
  - `grade(node, hand_idx, action_idx, pot_chips, bands) -> Grade`
    where `Grade { frequency, ev_loss_chips, ev_loss_pct_pot, best_action_idx, verdict }`.
  - Unit tests: mixed pick -> FineMix; exact best -> Correct; near-epsilon second action ->
    Correct; off-tree Bet(50) -> maps to Bet(33) or Bet(75) with notice; board-blocked hand ->
    None from find_hand; zero-weight hand -> range error; band boundary values.

### gto-config
- `TrainerConfig` (the three bands) + `[trainer]` in `default.toml`; `Config.trainer` field.

### gto-server
- `/api/setup` (GET): canonical board (reuse Phase 3 helper), depth allowlist, ActionTree
  root actions, `get_weight_by_cards` range check, manifest cached flag.
- `/api/solve`: `depth` field (default 50).
- `/api/grade` (POST): summary-first load, engine grade fn, JSON response.
- Hand parsing/normalization shared with the engine helper (one definition).

### viz front-end
- `trainer.ts`: the state machine above; entry form; commit buttons; reveal panel (verdict
  chip, two big numbers, distribution bars with per-action EV); session history list.
- Mode toggle in the topbar; trainer pane replaces the stage content in train mode.
- `types.ts`: `SetupResponse`, `GradeResponse`, `ev_actions` on `HandEntry`.
- Explore detail panel: show per-action EV under the existing bars (uses `ev_actions`).
- All error states real and inline (illegal cards, collision, out-of-range hand, unsupported
  depth, server down, solve timeout); never mocked.

### verify_trainer.py (the 4b gate, Playwright + API)
1. cargo check + cargo test (grading units) + npm build clean.
2. Commit gate: network interception proves no solve/grade/static fetch pre-commit on a fresh
   board; no grid rendered pre-commit.
3. Known-spot grading: use a cached spot (e.g. 50bb 8d6s3c, AA): commit the highest-frequency
   action -> Correct; commit a low-frequency near-zero-loss action (if the mix has one) ->
   FineMix or Correct per bands; commit the worst action by EV -> verify frequency + EV loss
   numbers reconcile against the stored summary row within tolerance, verdict matches the
   band arithmetic exactly.
4. Both depths: 100bb AcKhQd... (100bb Phase 1 board) cache-hits and grades; depth flows into
   the key (assert key string in setup response).
5. Off-tree action via direct API (Bet(50)) -> mapped + notice. Illegal hand (board collision)
   and out-of-range hand (72o) -> clean errors. Unsupported depth (20) -> clear error.
6. Phase 2 + Phase 3 verifiers still green (regression).

---

## 4. Open questions / risks

| Risk | Mitigation |
|---|---|
| 100bb fresh solve latency (bigger trees than 50bb; est. 1 to 3 min) | Within the 300 s timeout; the 6 Phase 1 100bb boards are already cached; the operator sees the real convergence strip while waiting. |
| Per-action EVs carry ~0.5% pot solver noise at the convergence target | `correct_eps_pct_pot` equals the target by default; bands are tunables (D36). Verdicts are honest to solve precision, no finer. |
| `expected_values` panics if weights not cached / not solved | The engine wraps the exact call pattern (play -> cache -> read) used by working Phase 2/3 code; loaded games are in Solved state (proven by the existing viz path). |
| Old summaries lack `ev_per_action` | Grade endpoint falls back to `load_solution` (recomputes from the tree); fresh stores carry the field. No migration needed. |
| Action labels could drift between setup (ActionTree) and solve (game) | Same TreeConfig builder + same `render_action`; gate asserts setup actions == solved contract actions on a fresh board. |

## 5. Out of scope this phase

Turn/river decision training, multi-street lines, 3-bet pots, randomized spot generation,
history persistence, spaced repetition, EV-loss leaderboards, mobile layout, Tauri packaging.

## 6. DECISIONS.md additions

D32 (grading model + bands), D33 (per-action EV plumbing), D34 (setup/grade endpoints + depth
param), D35 (commit-then-reveal by data absence), D36 (bands in config), D37 (in-memory
session history). Recorded as planned, awaiting approval; flipped on the 4b gate.
