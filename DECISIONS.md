# DECISIONS.md - GTO Poker Bot

Phase 0 decisions and findings. Numbered for reference from the vault and future phases.

## Decisions

1. **Engine dependency = `postflop-solver` (b-inary), git-pinned `rev = 9d1509fe...`, default features + `zstd`.**
   Git-only, `version = 0.1.0`, no crates.io release, AGPL-3.0-or-later, development suspended (Oct
   2023). README warns breaking changes ship without version bumps, so a commit pin is required for
   reproducibility. The library API is complete (node queries supported), so no fork is needed.

2. **No fork in Phase 0.** Fork only if the build cannot be made to work on stable or we need source
   patches. See finding F1: the build was made to work by pinning a transitive dependency, not by
   forking.

3. **Workspace = 5 members under `crates/`:** `config`, `engine`, `preflop`, `validate`, `store`.
   `config` is the schema crate and single source of truth. Package names are prefixed `gto-` to
   avoid clashes with same-named crates.io crates (`config`, `store`).

4. **Spot data model carries money in big blinds and ranges as strings.** Preflop / earlier-street
   history is encoded as (ranges, pot, effective stack) and never passed to the solver. Conversion
   to integer chips happens at mapping time via `chip_per_bb`.

5. **`SpotSolution` owns the solved `PostFlopGame`** so it persists natively and supports node
   queries after reload with zero solving.

6. **Node queries use semantic `Step`s (Action / Chance) resolved to action indices at query time**
   (matched by Debug rendering, so no `PartialEq` dependency on the solver's `Action`). Stable
   across action-tree edits.

7. **Architecture is precompute-and-cache.** Solve curated spots once, store to `matrix/` (flat
   files + `manifest.json` + JSON summary sidecar), look up at decision time with zero solving.

8. **v1 matrix scope = a curated operator-defined set, not a sweep. The curated stored set IS the
   validation benchmark set (one list).** Spots chosen as much for verifiability as for frequency.

9. **No SQLite at v1** (flat files + `manifest.json`). SQLite-index-over-files is the deferred
   migration once the manifest grows large.

10. **Out-of-set spots: exact-key lookup only in Phase 0.** Nearest-node mapping and solve-then-cache
    are deferred.

11. **Preflop ranges imported from published HU charts (fixed inputs), not self-solved.** HU preflop
    is fully solved and postflop-solver is postflop-only. Phase 0 ships a hardcoded SRP stub.

12. **Validation = internal invariants first** (exploitability <= target, per-node strategy sums to
    1, zero-sum EV). External cross-check optional (opensolver UPI + `rs_poker`). No commercial
    solver assumed.

13. **`rs_poker = "4"` (resolves to 4.1.0) is the independent equity / eval cross-check dependency**
    in the `validate` crate.

14. **Chip resolution `chip_per_bb = 20` (a 10/20 game)** so all configured bet sizes are integers.

15. **Smoke spot: 100bb HU single-raised pot, BB (OOP) vs BTN (IP), flop Td9d6h, pot 5bb / eff
    97.5bb (pot 100 / stack 1950 chips), deliberately tiny bet tree, target 0.5% of pot.** Also
    exercises a store round-trip. Sized to clear the RAM / time abort condition.

16. **Knowledge store = the Obsidian vault, written through the file directory (not via MCP).** This
    repo keeps PLAN.md / DECISIONS.md / CLAUDE.md; the vault keeps durable findings. Vault note links
    are bidirectional. No em dashes in operator-facing writing.

17. **Packaging (Tauri vs WASM) deferred** per the master plan; v1 emits JSON for a local viewer.

## Findings

- **F1 - `postflop-solver` does not build against released `bincode 2.0.x`.** The crate pins
  `bincode = "2.0.0-rc.3"`. Cargo's caret on that pre-release resolves up to the released `2.0.1`,
  whose `Decode` trait gained a generic `Context` parameter (`Decode<Context>`), so every
  `impl Decode for ...` in postflop-solver fails to compile (E0107). Resolution: pin
  `bincode = "=2.0.0-rc.3"` as a workspace dependency referenced from `engine`. This keeps the
  suspended crate building on current stable Rust (rustc 1.95) without forking. This is the
  authorized "patch the dep pins" path under open question 3, not a fork (decision 2 holds).

- **F2 - Toolchain confirmed.** rustup 1.29.0, rustc / cargo 1.95.0 stable, host
  `x86_64-pc-windows-msvc`. `zstd` builds via the MSVC toolchain (cc / zstd-sys compiled cleanly).

## Open questions (proposed resolutions)

1. Reference-solve source for validation: opensolver (UPI) + `rs_poker` equity; internal invariants
   are the gate; benchmark set = the curated matrix. Confirm opensolver's license before depending.
2. Real-spot RAM ceiling: `ram_ceiling_gb` + compression; shrink the tree on overflow.
3. Exact preflop chart source / format: import a published HU set; finalized Phase 1.
4. Matrix index scaling: flat files + manifest at v1; SQLite index deferred.
5. Packaging Tauri vs WASM: deferred; v1 dumps JSON for a local viewer.

## Phase 1 decisions (2026-06-03)

- **D18. Preflop ranges AUTHORED in-repo** (`crates/preflop/data/hu_ranges.toml`, 100bb + 50bb SRP)
  from established HU GTO theory, NOT imported. No clean free importable HU set was confirmed; the top
  candidate (AHTOOOXA/poker-charts) was refuted by adversarial verification (its range data does not
  exist). Our authored data is license-clean. The real loader (`PreflopCharts`) replaces the Phase 0 stub.
- **D19. opensolver DROPPED as a cross-check.** Verified: no LICENSE file (defaults to all rights
  reserved) and isomorphism hardcoded-disabled (`iso_flag = 0` in src/isomorphism.rs), so it is legally
  and practically unusable. External cross-check = rs_poker equity only. A properly-licensed reference
  (CFRX / Pio API) is the future option.
- **D20. rs_poker equity cross-check uses non-arena API** via a deterministic exhaustive turn+river
  enumeration (rs_poker `RangeParser` + `Hand::rank`; the enumeration is ours). Tolerance 2%.
- **D21. Curated benchmark = 8 SRP flop spots** (`benchmark/spots.toml`); the matrix IS the benchmark.
  Spot keys `{depth}bb_{board}_flop_srp-v1`.
- **D22. Bet tree `srp-v1`** (`phase1.toml`: flop 33%+75%, turn/river 75%, compression on, zstd-9). The
  batch runner shrinks a spot's flop tree on a `ram_ceiling_gb` breach and logs the tradeoff (not
  triggered in practice; all 8 spots fit).

## Phase 1 findings

- **F3 - The rs_poker research agent confabulated `OutsCalculator`.** It does not exist in rs_poker
  4.1.0. Verified against the crate source: the non-arena equity primitives are `holdem::MonteCarloGame`
  (random) and `holdem::RangeParser` + `core::Hand::rank` (Rankable). We use a deterministic exhaustive
  enumeration built on `RangeParser` + `Hand::rank` for a stable, RNG-free gate. Lesson reinforced:
  verify agent-reported API names against source before relying on them.

## Phase 1 gate (passed)

- `cargo check --workspace --all-targets` clean; `cargo test -p gto-preflop` 3/3.
- All 8 curated spots solved below the 0.5% target (0.38% to 0.49% of pot), invariants passed, stored;
  no shrink needed.
- Equity cross-check on `100bb_Ks7h2c` (hero `2h2d`): solver 0.9535 vs rs_poker 0.9535, |diff| 0.0000.

## Phase 2 decisions

- **D23. Packaging = native Rust JSON export + static Vite/TS front-end** for the read-only v1 viz. The
  viz only reads stored solves, which collapses the Tauri-vs-WASM tradeoff. Tauri is the chosen EVENTUAL
  packaging (over WASM) for when live re-query/solving is added; deferred; the front-end is built
  Tauri-ready. WASM rejected.
- **D24. Data path** = matrix -> `gto-store` example `export_viz` (native; `load_manifest` +
  `load_solution`, root node) -> `viz/public/data/*.json` -> Vite/TS fetch. Read-only, no solving in the
  UI, root only for v1 (`query_node` for deeper nodes is deferred).
- **D25. Data contract** = per-spot JSON (board, street, stacks, pot, exploitability, hero/villain seats,
  actions, hands[{hand, strategy, ev, equity}]) + an `index.json`. The exporter reshapes the solver's
  `strategy[hand + action*n]` into per-hand arrays.
- **D26. Strategy readout** = a 13x13 OOP root action-frequency grid (primary) + a per-hand detail panel
  (action mix / EV / equity); a per-hand-only readout is the documented fallback if the grid is heavy.

## Phase 2 findings

- **F4. WASM rejected for the viz** (adversarially verified). zstd-sys will not C-compile to wasm32;
  rayon needs nightly + atomics + COOP/COEP; postflop-solver has no wasm tree-loader; eager 39 MB
  bincode in-browser is wasteful; and a read-only viz needs no engine in the browser at all. The native
  JSON-export + static front-end path avoids all of this.

## Phase 3 decisions

- **D27. Server framework = axum 0.8 + tokio 1** (new crate `gto-server`). `tower-http` CORS
  middleware (permissive; localhost-only, no credentials). Binary target run from the repo root:
  `cargo run --release -p gto-server`, binds `127.0.0.1:7474`. Fails fast if `phase1.toml` is
  missing (a defaulted config would silently solve under the wrong tree id and split the cache).
- **D28. Progress = SSE over a fetch stream.** Events: `progress` (iteration + exploitability
  every 10 solver iterations), `complete` ({cached, shrunk, spot}), `error` ({message}); exactly
  one terminal event per request. REFINED at build time: the browser `EventSource` API cannot
  POST and its auto-reconnect would re-fire solves, so the client parses `text/event-stream` off
  a `fetch` body (~40-line parser in `viz/src/solve.ts`). Polling rejected as planned.
- **D29. Bet tree for 50bb = `srp-v1`** (same as Phase 1 50bb spots). No `srp-50-v1` defined;
  sizing is pot-fraction-based and scales to any stack depth correctly. Verified: a submitted
  `AcKhQd` cache-hits the Phase 1 batch entry with no re-solve.
- **D30. Concurrency = single-solve-at-a-time.** REFINED: `Arc<tokio::sync::Semaphore>` with one
  permit instead of `Mutex<SolveStatus>`; the owned permit moves into the blocking solve thread,
  so the slot releases exactly when the CPU work ends (RAII, panic-safe, and a timed-out orphan
  keeps holding the slot until it finishes and caches). Second submit while running gets an
  immediate `error` event. Cache hits never touch the slot.
- **D31. Boards are canonicalized server-side** (rank desc, suit desc s>h>d>c, matching the
  Phase 1 benchmark keys) before key building, so ANY typing order/case of the same flop maps to
  one spot key (cache correctness). The engine's solve geometry is order-independent; only the
  key needed pinning. The shrunk-tree variant key (`...-shrunk`) is also checked on lookup so a
  RAM-shrunk board still cache-hits.

## Phase 4 decisions

- **D32. Grading model = two numbers + banded verdict, mixed-strategy aware.** Frequency of the
  chosen action + EV loss vs the best action (chips and % pot). Verdict precedence: Correct
  (loss <= eps) > FineMix (freq >= floor AND loss <= minor max) > MinorLeak > ClearLeak. A
  low-frequency near-zero-loss pick is a fine mix, never a punished mistake; within-epsilon is
  Correct regardless of frequency. Off-tree actions map to the nearest available with a notice.
- **D33. Per-action EV plumbing**: `NodeStrategy.ev_per_action` (serde-default), computed in
  `read_root` by play -> cache_normalized_weights -> expected_values(root_player) per action
  (verified: interpreter.rs:672 supports reading the non-current player). `load_solution`
  recomputes the root, so every cached spot gets per-action EV with no migration.
- **D34. Trainer endpoints on the existing server**: GET `/api/setup` (legal actions from
  `ActionTree::available_actions()` (board-independent, no solve), hand-in-range via
  `Range::get_weight_by_cards`, cached flag); `/api/solve` gains `depth` (50|100 allowlist,
  default 50); POST `/api/grade` (summary-first load, engine-side pure grading fn).
- **D35. Commit-then-reveal enforced by data absence**: pre-commit the client holds only action
  labels + booleans; no solve/grade/static fetch fires until the commit click. Gate test
  intercepts network traffic to prove it.
- **D36. Verdict bands in config** (`[trainer]`): correct_eps_pct_pot = 0.005 (= the solver
  convergence target; verdicts are honest to solve precision), minor_leak_max_pct_pot = 0.02,
  mix_freq_floor = 0.10. Front-end renders verdicts, never computes them.
- **D37. Session history = in-memory list** (board, depth, hand, action, verdict, EV loss;
  running + average EV loss). Persistence / spaced repetition / randomized spots deferred.

## Phase 5 decisions (approved 2026-06-04 with operator additions D44/D45)

- **D38. Input = minimal JSON schema as the engine contract + a PokerStars-text adapter.**
  Core (envelope/aggregation) is built and tested against synthetic JSON fixtures; the PS
  adapter quarantines format landmines (posts/dead blinds, all-in markers, uncalled-bet
  returns, locale, sit-outs, dealt-count vs table-max). Hero hole cards required to grade.
- **D39. Envelope = decision-level, precise, no action translation ever.** HU (2 dealt),
  standard blinds, SRP only; open in [2.3, 2.7]bb and eff stack within +-7% of {50,100}
  (tight: a graded instance must approximate the solved geometry to sub-noise); FLOP only;
  check-only action prefixes (two node shapes: BB root, IP after BB check); hero's own bet
  maps only within +-8% pot of a tree size, else skipped (`off-tree-hero-size`); shrunk-tree
  instances excluded from size-split math (`solve-shrunk-size-unavailable`). Full skip/state
  taxonomy in PLAN-phase5.md incl. `pending-solve` as a state distinct from skipped.
- **D40. Aggregation = per-instance-mean GTO comparator + Poisson-binomial significance +
  mix-EV primary cost.** g(a) = mean_i p_i(a) over the operator's actual combos; z-test with
  mu = sum p_i, sigma^2 = sum p_i(1-p_i) (plain binomial SE is the wrong null for
  heterogeneous mixes); ranked leak iff N >= 10 and |z| > 2. Primary EV cost = sum_i
  (node.ev[h] - node.ev_per_action[h + a*n]) computed in the aggregator (node.ev IS the mix
  dot product), NOT from grade() (clamped vs-best, kept as the secondary "max exploitable").
- **D41. Ingestion = two-pass.** Pass 1 instant parse/classify -> coverage; Pass 2 sequential
  fresh solves through the existing /api/solve (semaphore-resilient retry, per-run cap
  default 25, capped -> pending-solve), then solve-all -> re-read manifest -> grade-all.
- **D42. Persistence = accumulating JSONL** (`matrix/history/graded.jsonl`, gitignored),
  dedupe (hand_id, street, decision_index), schema_version per record, idempotent re-ingest.
- **D43. Report framing = frequency-only fixed templates, gate-enforced.** "For the hands you
  held" wording; no per-instance verdicts in the leak report; ranking by EV cost only, never
  by prescription; a gate tripwire greps the bundle for prescriptive phrasing, WITH the
  recorded caveat that the grep validates templates, not framing - the safety comes from the
  per-instance-rate model itself.
- **D44 (operator). The GTO comparator is RANGE-CONDITIONED - a stated scope boundary.**
  g(a) is conditioned on the hands actually held, NOT the node's unconditional GTO frequency.
  The report measures action-given-range leaks only and CANNOT detect range-construction
  leaks; a skewed arriving range moves g(a) with the selection bias (the operator can look
  less leaky than they are). One-line caveat in the review UI near the leak table; templates
  never let g(a) read as "the GTO frequency for this node".
- **D45 (operator). Per-instance EV sanity assertion before any clipping** (F5 regression
  guard through read_node): where p_i(a_i) >= the mix floor, assert node.ev[h] >=
  node.ev_per_action[h + a*n] - eps. You cannot beat the equilibrium mix with an action the
  solver also mixes; a violation = an F5-style reference error re-entered via read_node ->
  ingestion fails loudly. Display-time clipping stays but is never the only guard.
  CALIBRATION (build-time deviation, flagged): eps = 2% pot, not the literal solve target
  (0.5%). The audit measured LEGITIMATE played-action gaps up to 1.385% pot on converged
  solves (a mixed action can sit ~1% above the weighted mix), so a 0.5% epsilon would
  false-fire on audited-good data; F5-class violations are 30%+ of pot, so 2% separates the
  classes cleanly. Tunable: `[review] ev_sanity_eps_pct_pot`. Companion: the dot-product
  identity + no-dominant-unplayed-action checks are promoted into check_invariants (audit
  follow-up priority 1), so fresh solves inherit the guard at the gate.

## Range + tree revision decisions (2026-06-05, operator-approved)

- **D46. BB defend ranges re-authored with fractional weights (3-bet-mix top).** The SRP
  range audit traced the advisory flags to the authored BB ranges (~34% of hands, top-heavy,
  no low-board coverage). New construction (commit 1e805da): 59.4%/53.5% weighted continue at
  100/50bb; premiums enter the SRP call range at reduced weight (AA/KK 0.25, QQ/AK 0.5,
  JJ/AQ 0.75 - real SRP pots are capped); coarse {1, .75, .5, .25} grid; IP unchanged. The
  coupled range-weight plumbing closed audit follow-up #3 (HandEntry.weight = authored mass,
  weight-correct grid, review guard w > 0). ACCEPTANCE HYPOTHESIS FALSIFIED: the advisory
  gaps WIDENED, because a capped BB cannot be defended without a check-raise and srp-v1 had
  no raise lines. Kept by operator decision with the falsified hypothesis on record.
- **D47. Bet tree `srp-v2` = srp-v1 + ONE flop raise size ("3x", both players).** The
  binding fix for D46's outcome: the check-raise line the capped BB needs. "3x" = raise-TO
  three times the facing bet (Bet(33) -> Raise(99), Bet(75) -> Raise(225)); the grammar is
  PINNED by an engine unit test BEFORE the benchmark was rebuilt on it (the F5 discipline
  applied to a config value). Turn/river raises deferred to the 3-bet phase. Tree ids and
  spot keys are now single-sourced in the gate harness (`verifier_boards.TREE_ID` +
  `spot_key()`), killing the ~12-literal drift class the id change would otherwise create.
  Acceptance is MECHANISTIC (operator amendment): the per-hand defence trace must show the
  classes that folded under srp-v1 taking the Raise line; the advisory re-run is
  corroboration only, never the pass condition.
  OUTCOME (2026-06-05): PASS, mechanism confirmed per hand (`defence_trace`, srp-v1 baseline
  traced from the archive with the same tool). The srp-v1 defect was a CAPPED checking range:
  strong classes avoided the check line entirely (air = 67.0% of the BB's mass at
  [Check, Bet(33)] on 100bb 9c5d3h) or check-called passively, and the air-heavy node folded
  56.8%. Under srp-v2 the strong classes return to the check line (air share 32.7%; set/top
  pair/draw shares roughly double) and raise actively (sets 100%, overpairs 79.6%, draws
  40.2%, top pair 33.3%; 358 hands raise >= 5%); aggregate fold 56.8% -> 27.8% (Bet 33) and
  69.0% -> 46.2% (Bet 75). Corroboration: advisory gaps 9c5d3h +32.0 -> +3.0, 7h7d2c
  +27.1 -> +12.4, c-bet 100% -> 56.3% (the raise threat repriced indiscriminate c-betting).
  Honesty notes: (1) the plan's tree-size estimate (+5-8% nodes) was wrong - measured ~2.4x
  (180k -> 427k decision nodes per 100bb spot); harmless direction, zero shrinks, all
  invariants green. (2) One NEW borderline advisory flag: 50bb AcKhQd BB vs Bet(33) at
  +20.1, composition-audited legitimate (100% fold of ~12%-equity air on the one
  BB-crushed board - MDF's realization blind spot); threshold deliberately NOT retuned in
  the acceptance commit, left flagged for an operator calibration call.

## 3-bet / 4-bet phase decisions (2026-06-06, Phase A)

- **D48. Pot types as first-class config (`PotType` srp/3bp/4bp) with single-sourced
  geometry.** The preflop line a flop spot came from is now selected by `PotType`
  (gto-config, the DAG base; wire strings "srp"/"3bp"/"4bp" pinned by test) and resolved in
  exactly two funnels: `SolveConfig::tree_for(pot)` (the bet tree; ids 3bp-v1/4bp-v1 live in
  phase1.toml only) and `PreflopCharts::spot_inputs(pot, depth)` (ranges + pot + stacks,
  stored NEXT to the ranges in hu_ranges.toml). The uniform HU law pot = 2*raise_to,
  eff = depth - raise_to is asserted at LOAD time and the authored values (2.5 / 10 / 9 / 22)
  are pinned by tests - this killed the `pot_bb: 5.0` / `depth - 2.5` literal class at all
  four Rust sites (build_matrix, server solve, suit_isomorphism + the envelope's arithmetic
  generalizes in Phase C). Canonical lines (operator-settled): 3-bet to 10bb@100 / 9bb@50
  (pot 20/18, SPR 4.5/2.3); 4-bet to 22bb at 100bb ONLY (pot 44, SPR ~1.77; 50bb 4-bet pots
  are jam-or-fold and stay out of scope with a precise envelope skip). Trees: 3bp-v1 EXTENDS
  srp-v2 unchanged; 4bp-v1 DIFFERS (single 33% flop size + the 3x raise - at SPR 1.77 a 75%
  line is all-in anyway). MEASURED at STOP A1 before the benchmark build (the srp-v2
  estimate-miss lesson): 3bp@100 ~34-47s solve+store, 309,014 nodes, 168.8MB; 4bp@100 2.4s,
  71,648 nodes, 2.2MB; zero shrinks; existing 900s/960s budgets hold with order-of-magnitude
  margin. THE RANGES (the product): the committed SRP fractional weights RESERVED the 3-bet
  mix, so the consistent BB 3-bet range is POLAR and small - 4.9%@100 / 4.0%@50 (premium
  complements + budgeted blocker/suited bluffs), NOT the ~13% of merged HU constructions
  (hands SRP-calling at 1.0 have zero 3-bet capacity); total defend 64.3%/57.5%. Surfaced to
  the operator at STOP A1 with the arithmetic and approved. Consistency LAWS unit-tested per
  combo before any solve: srp_call + 3bet <= 1, btn call3 + 4bet <= open, 4bet-call <= 3bet,
  totals in band. Acceptance traces (the srp-v2 mechanistic discipline, applied via the new
  defence_trace path-spec arg): the capped BTN caller raises the polar c-bet (17.8% raise
  mass, top pair 45%, zero strong-class folds), the BB check range check-raises 23.9%, and
  the 4bp 62.7% fold is composition-audited (QQ/JJ at 19.6% equity on K-high vs the 4-bet
  range - range-driven, not mechanism-driven). 8 3bp + 2 4bp spots solved, all invariants
  green, validate_matrix ALL 21 SPOTS PASS.
- **D49. The equity cross-check is now villain-weight-CORRECT (was weight-blind, found by
  F5 discipline at the first weighted-villain check).** The first 3bp cross-check read
  |diff| 0.0180 (vs the srp-v2 exact 0.0000) - investigated BEFORE building on it: the
  rs_poker RangeParser neither knows the weighted ":w" syntax (it also silently DROPPED
  combos: 212 parsed vs 281 real) nor was the tally weighted (each combo counted 1/n while
  the solver's equity is villain-reach-weighted). Fixed: the villain range is parsed by the
  AUTHORITATIVE parser (postflop-solver via the engine re-export - range-string semantics
  belong to the system under test; rs_poker remains the independent 7-card RANKER), the
  tally weights each combo, and the psf-id -> rs_poker card mapping is built by parsing all
  52 card strings through both parsers with a permutation check (no encoding arithmetic
  assumed). Verified at the data layer: the weighted composition law and the degenerate
  all-weights-1.0 equivalence are unit-tested; the 3bp re-check reconciled EXACTLY
  (0.5478 == 0.5478). This was the audit's range-weight tripwire class surfacing at a new
  code surface, caught by the guard-every-new-surface rule.
- **D50. Pot types live end to end (Phase B): server build_spot, viz pot selector,
  curated-only export, registry TREE_IDS.** The server resolves every request through
  `build_spot(board, depth, pot)` (spot_inputs + the pot's tree in a per-request effective
  config; SRP literals gone server-side; pot_type serde-defaults to srp for back-compat on
  all three endpoints). D45 in review grading now scales by the loaded solution's own pot
  per key. The viz pot selector constrains depth per pot (4bp locks to 100bb). export_viz
  exports ONLY the curated set (keys derivable from benchmark/spots.toml + smoke) so ad-hoc
  on-demand solves never leak into committed fixtures. The registry grew TREE_IDS +
  DEPTHS_BY_POT + a pot-aware spot_key whose default keeps every pre-pot call site
  byte-identical, and the sweep clears consumable boards across all pot keys. All four
  gates extended and GATE PASSED: 19 fixtures across 3 pots; the 3bp fractional-weight
  reconciliation (AA 0.75 / A5o 0.25 / TT absent / grid mass 4.50 - the 1e805da machinery
  generalized, no STOP); a 3bp fresh on-demand solve (31.4s, stored pot_bb 18.0,
  OOP-aggressor action set); a cached 3bp trainer round through the commit gate (buttons ==
  the 3bp tree's actions). One trainer rerun after the known LNK1104 linker race poisoned
  its test step (engine tests 10/10 standalone).
- **D51. Review envelope extended to 3-bet / 4-bet pots (Phase C).** The classifier's
  raise-count arms are SEAT-CHECKED ([Btn]=srp, [Btn,Bb]=3bp, [Btn,Bb,Btn]=4bp, more =
  five-bet-pot skip; non-alternating sequences are unparseable, never guessed), with
  per-depth size bands (3-bet [9,11.5]@100 / [8,10.5]@50; 4-bet [19,26]@100; 4bp@50 skips
  fourbet-depth-out-of-scope by decision). The blanket "3-bet-pot" skip reason is RETIRED.
  Four new node shapes: 3bp-oop-root / 3bp-ip-after-check / 4bp-oop-root /
  4bp-ip-after-check - one factual correction to the approved plan recorded here: the plan
  named a "4bp-ip-root" on the assumption the 4-bet aggressor acts first, but HU postflop
  order is fixed (BB/OOP first in EVERY pot family; the gate-measured 4bp root action set
  [Check, Bet(290)] proves it), so all six shapes resolve to two tree positions (root /
  after-one-check) selected by `hero_is_oop()`, while the pot-distinct shape STRINGS keep
  aggregation pools separate. Two pots, two jobs, never crossed: hero-size mapping uses the
  hand's ACTUAL pot (2 x the last raise-to); grading and D45 scale by the SOLVED spot's
  pot. Flop size labels are chip-denominated per (pot, depth) via `tree_sizes_for`, and
  byte-pinned against the real solved trees by the gate's end-to-end grading (g9's
  Bet(132) c-bet), not by rounding assumptions. spot_key_for resolves the tree id PER
  DECISION via tree_for. GradedRecord schema_version 1 -> 2: adds pot_bb (solved spot's
  pot) so the D45 epsilon scales per record (a chip gap that fires at the srp 100-chip pot
  is noise at the 4bp 880-chip pot - unit-tested both directions); v1 rows deserialize
  with the srp default and aggregate unchanged (back-compat test). verify_review extended:
  five gradeable 3bp/4bp synthetic hands against the cached benchmark, coverage >= 1 for
  every new skip reason plus a retired-reason assertion, the F5 battery runs PER NODE
  SHAPE with pot-scaled thresholds, and the trainer-reuse proof repeats on a 3bp record.

## Phase 4 findings

- **F5 - Per-action EV must be read with `expected_values_detail` AT the player's own node,
  never by navigating into children.** The plan's original mechanism (play into each child,
  read `expected_values` there) returns values whose money reference point shifts by the bet
  amount of each branch: on 50bb 8d6s3c, AsAc showed Check 202.3 / Bet(33) 235.5 / Bet(75)
  277.2 - "EV gaps" of +33 and +42 chips exactly tracking the bet sizes, impossible at
  equilibrium for a 5/73/22-mixed hand. `expected_values_detail(player)` at the decision node
  applies one shared offset (`starting_pot/2 + node.amount + bias`, interpreter.rs:795) to
  every action row, giving directly comparable EVs: the same hand reads 202.30 / 202.47 /
  202.19 (0.28-chip spread, equilibrium-consistent). Caught by sanity-checking exported data
  against mixed-strategy theory before wiring the grader.

## Phase 4 gate (passed 2026-06-04)

- `cargo check` clean; 9 grading unit tests green; `npm run build` clean.
- All FOUR verdict bands found in real solved data and reconciled exactly against a python
  recomputation (frequency to 1e-4, EV loss to 0.01 chips, band arithmetic, best action):
  clear_leak = Ah2h Bet(33) on 50bb 8d6s3c (3.79% pot loss), fine_mix = Ad3d Bet(75) (37%
  frequency, 0.63% pot), minor_leak = Ah4h Bet(75) (9.9% freq, 0.52% pot), correct = 2d2c
  Check (0.05% pot).
- Commit gate held on all three UI rounds: zero /api/solve, /api/grade, and data/*.json
  requests pre-commit, zero grid cells in the DOM pre-commit; solve+grade fire only after the
  commit click. Fresh board 9s6h3d: committed BEFORE any solve existed anywhere, 9 progress
  samples streamed, solved + graded in 20.3 s.
- Both depths through the UI: 50bb cached (3.7 s reveal, 0 progress events) and 100bb
  Ks7h2c (key `100bb_Ks7h2c_flop_srp-v1` in the selector proves depth flows into the cache
  key). AA cell pre-selected on reveal; session history shows 3 rows + running EV loss.
- Off-tree Bet(50) maps to Bet(33) with a `mapped_from` notice. Collision (client-side, zero
  requests), out-of-range hand (setup flag + grade error), unsupported depth, and
  unsolved-spot (409) all error cleanly. Zero console/page errors.
- Phase 2 (`verify_viz.py`) and Phase 3 (`verify_solve.py`) gates re-run green.

## Phase 3 gate (passed 2026-06-03)

- `cargo check --workspace --all-targets` clean; `npm run build` clean; server + canonicalization
  unit tests green.
- Fresh 50bb flop (8d6s3c): real solve in 17.1 s, 9 streamed convergence samples, grid rendered,
  stored exploitability 0.410% pot (below the 0.5% target), invariants passed server-side.
- UI AA cell reconciles with `matrix/<key>.summary.json` on disk: equity diff 0.025%, all three
  action bars exact, 6 combos (weight-correct aggregation).
- Same flop re-submitted: served from cache in 0.97 s with 0 progress events; server log shows
  exactly one solve across both submits. Phase 1 board `AcKhQd` also cache-hits (key interop).
- Illegal board (`KsKs2c`): inline 'duplicate card' error, submit disabled, no request fired.
  Direct API: 2-card board and duplicate card return SSE `error` events (server-side validation
  is independent of the client); malformed JSON returns HTTP 400.
- Zero console/page errors across the whole Playwright session; Phase 2 `verify_viz.py` still
  passes (no regression).

## Phase 2 gate (passed 2026-06-03)

- `cargo check --workspace --all-targets` clean; `npm run build` clean.
- All 9 stored spots render from the real matrix: board=3, hero=2, 169 grid cells, bars, detail, zero
  console errors.
- AKs equity on `100bb_Ks7h2c` displayed 89.6%, expected 89.6%, diff 0.012% (within f32 tolerance).
- AKs aggregation Check=9%, Bet(33)=87%, Bet(75)=4% - exact match vs raw JSON means over 3 non-blocked
  combos (AsKs absent, blocked by Ks board card; weight-correct via actual combo count).
- All rendering original CSS/SVG (no branded/external card assets).
