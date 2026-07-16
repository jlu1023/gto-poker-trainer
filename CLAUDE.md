# CLAUDE.md - gto-poker-bot

Build conventions and constraints for this repo. Read alongside PLAN.md and DECISIONS.md.

## Source of truth
The master plan at `C:\Users\james\Second Brain\Projects\GTO Poker Bot\GTO Poker Bot.md`
is authoritative for goal, solved-state landscape, method, stack, and architecture. Durable findings
and decisions are mirrored in the Obsidian vault folder next to it (for example
`Phase 0 - findings.md`). This repo holds PLAN.md, DECISIONS.md, and CLAUDE.md; the vault holds the
broader knowledge store. When linking vault notes, add the backlink on both notes (bidirectional).

## What this is
A correct, offline heads-up no-limit hold'em (HUNL) GTO engine in Rust, built on the
`postflop-solver` crate (Discounted CFR). Roadmap: full HUNL engine, validation harness,
visualization, then multiway, then a trainer. This is Phase 0 (scaffold + smoke).

## Architecture: precompute-and-cache
Solve a curated set of spots once, store each solved tree to the `matrix/` directory, and look up at
decision time with ZERO solving. The engine produces a `SpotSolution`; the `store` crate persists it
(native bincode + optional zstd) plus a `manifest.json` index and a JSON summary sidecar.
`load_solution` reads the tree and recomputes the root with no solving.

The curated stored set IS the validation benchmark set: one spot list, not two. Spots are chosen as
much for "can I verify it is right" as for "will I hit it often."

## Key constraints
- Local-only engine. Solving is bursty and RAM-heavy; it runs on the local machine, not a hosted VM.
- No real-time assistance during live play. This is a study / trainer tool only.
- Validation does not assume a commercial solver. Primary checks are internal invariants
  (exploitability below target, per-node strategy sums to 1, zero-sum EV). External cross-check
  (opensolver UPI, `rs_poker` equity) is optional and Phase 1 onward.
- No SQLite at v1 (flat files + manifest.json). SQLite-index-over-files is the deferred migration.
- Out-of-set spots in Phase 0: exact-key lookup only. Nearest-node mapping and solve-then-cache are
  deferred.

## Workspace
Seven crates under `crates/`:
- `config`  (`gto-config`)  - single source of truth for every tunable; serde schema + `default.toml`.
- `engine`  (`gto-engine`)  - `Spot` model, spot-to-solver mapping, `solve_spot` /
  `solve_spot_streaming`, `query_node`.
- `preflop` (`gto-preflop`) - HU preflop range + geometry provider (authored
  `data/hu_ranges.toml`): `spot_inputs(pot_type, depth)` returns ranges + pot + effective
  stack per preflop line (srp / 3bp / 4bp; the pot = 2*raise_to law is load-asserted and the
  authored values test-pinned; per-hand range-consistency laws are unit-tested).
- `validate`(`gto-validate`)- internal invariants (six-check `check_invariants` gate: exploitability
  target, root + full-tree strategy normalization, per-action EV consistency, root zero-sum,
  best-response recheck) + `rs_poker` equity cross-check (villain-weight-correct, D49) + the
  per-pot advisory layer (MDF thresholds calibrated measure-then-pin per pot family, D52;
  `theory_advisory` iterates every configured pot tree); examples `suit_isomorphism`
  (on-demand, two fresh solves) and `defence_trace` (per-hand mechanistic acceptance tool,
  optional path-spec arg).
- `store`   (`gto-store`)   - precompute-and-cache: serialize solved trees + manifest; the shared
  viz contract (`gto_store::viz`); examples `smoke` / `build_matrix` / `export_viz` /
  `validate_matrix` (re-run the full invariant gate over every stored spot).
- `server`  (`gto-server`)  - Phase 3 localhost solve-on-demand binary: POST a flop, SSE streams
  convergence, the matrix is the cache. Phase 4/5 endpoints: trainer setup/grade, review
  ingest/grade/report.
- `history` (`gto-history`) - Phase 5 hand-history leak analysis: JSON input contract +
  PokerStars-text adapter, envelope classifier (skip-with-reason), node aggregation
  (range-conditioned comparator, Poisson-binomial significance, mix-EV cost), JSONL store.

Dependency graph (DAG): `config` is the base; `engine` and `preflop` depend on `config`; `validate`
and `store` depend on `engine` + `config`; `history` depends on `config` + `store`; `server`
depends on all six.

## Spot model (the highest-risk piece)
postflop-solver solves a subgame from a chosen street. Preflop / earlier-street history is NOT passed
to the solver; it is encoded in three inputs: the two ranges, the starting pot, and the effective
stack - all sourced per pot type from `gto_preflop::spot_inputs` (never hardcoded at call sites).
Money is carried in big blinds and converted to integer chips via `chip_per_bb`. Node reads
use semantic `Step`s (Action / Chance) resolved to action indices at query time, so queries survive
action-tree edits.

## Engine dependency
`postflop-solver` is git-only, AGPL-3.0, development suspended (Oct 2023), and makes breaking changes
without version bumps. It is pinned to a specific commit:
`postflop-solver = { git = "https://github.com/b-inary/postflop-solver", rev = "9d1509fe..." }`.
AGPL-3.0 imposes no obligation for personal offline use; a hosted or shipped viz later must comply.

## Build / run
- Toolchain: Rust stable (pinned via `rust-toolchain.toml`). Windows, PowerShell.
- `cargo check --workspace --all-targets`
- Smoke (from repo root): `cargo run --release -p gto-store --example smoke`
  Converges below target, proves the store round-trip, writes `out/smoke.json` and a `matrix/` entry.
- Curated matrix (from repo root): `cargo run --release -p gto-store --example build_matrix [filter]`
  Solves the curated benchmark (`benchmark/spots.toml`; rows carry `pot_type` srp/3bp/4bp) with
  each pot type's tree from `phase1.toml` via `tree_for` (srp-v2: flop 33%/75% + one "3x"
  check-raise both players; 3bp-v1 extends it; 4bp-v1 single 33% + "3x"; single turn/river size,
  no turn/river raises anywhere), asserts invariants, stores each spot, and runs the rs_poker
  equity cross-check once per pot type (villain-weight-correct since D49). The optional filter
  substring selects spots (pre-flights, single-pot rebuilds) without re-solving the rest. Preflop
  ranges + geometry are authored in-repo (`crates/preflop/data/hu_ranges.toml`, fractional
  weights); the curated stored set IS the validation benchmark. The gate-side tree ids /
  spot-key format are single-sourced in `verifier_boards.py` (`TREE_IDS` per pot type,
  `DEPTHS_BY_POT`, `spot_key(depth, board, variant, pot)`).
- Viz export (Phase 2, from repo root): `cargo run --release -p gto-store --example export_viz`
  writes `viz/public/data/*.json`; then `npm run dev` (or `build`) inside `viz/`.
- Desktop app (the primary way to RUN the lab): `desktop\src-tauri\target\release\gto-lab-desktop.exe`
  One window, opens maximised, with `gto-server` spawned (raw std::process::Command, not a
  Tauri externalBin sidecar) as an app-owned process (lifecycle = app lifecycle:
  reuse-if-healthy on 7474, fail-loudly on a squatted port or a missing matrix at the
  resolved root - GTO_LAB_ROOT env else the baked default - and the server dies with the
  app). Build with `desktop\build-desktop.ps1` (the crate is workspace-EXCLUDED): it builds
  viz/dist, builds gto-server, PROVISIONS it beside the app exe, then builds the app with
  `--features custom-protocol`. That feature is LOAD-BEARING: a plain `cargo build --release`
  in `desktop/src-tauri` produces a DEV-MODE binary whose webview points at
  `http://localhost:5173` and shows ERR_CONNECTION_REFUSED on a double-click - use the
  script. build-desktop.ps1 re-provisions on every build (closing the stale-binary
  foot-gun); verify_desktop's pre-flight also catches provision drift and a dev-mode binary
  (it asserts the exe embeds the current viz/dist, and over CDP that the webview loaded the
  viz from tauri.localhost and a cached board paints the grid). AGPL note: D53.
- Solve server (dev path, from repo root): `cargo run --release -p gto-server`
  Listens on `127.0.0.1:7474` (pots with authored inputs: srp/3bp at 50+100bb, 4bp at 100bb;
  trees from `phase1.toml` via `tree_for`). Endpoints: `/api/health`, `/api/solve` (SSE;
  depth + pot_type in the body, both defaulted for back-compat), `/api/setup` (trainer
  pre-commit: legal actions + hand-in-range, no strategy data), `/api/grade` (verdict
  for a committed action; bands from `[trainer]` config), `/api/node` (navigator; below).
- Navigator (explore mode, D55): walk an already-cached solve down the action tree. `/api/node`
  NEVER solves (409 on an uncached spot). TWO gates, deliberately distinct: the HARD F5 gate
  (dot identity + per-action EV present) 500s at any displayed node, while the SOFT display gate
  (reach-gate) only decides descendability. A decision child is descendable when its OWN worst
  unplayed-action dominance is <= `[navigator] display_gate_pct_pot` (5% of pot). Solve quality
  is GLOBAL, not local: a low-reach subtree is barely trained even at root exploitability
  <= 0.05% of pot, so dominance there is large and does NOT shrink with tighter global
  convergence - never use it as a hard F5 gate off-root (`ev_dot_consistency` is the
  offset-invariant one that is valid anywhere). The same dominance ships per node as the
  CONFIDENCE signal, and that indicator is LOAD-BEARING: 5% (over a purist 1%) is only honest
  because a marginal node cannot look as authoritative as a root-quality one. If the indicator
  ever stops working, set `display_gate_pct_pot = 1.0` - never ship 5% unmarked. Gate + tiers
  ship in the contract (`GateContract`) so the UI never duplicates the thresholds.
- Trainer (Phase 4): the viz "train" mode. Commit-then-reveal is enforced by data absence: the
  client fetches nothing solver-derived until the action is committed. Grading is engine-side
  (`gto_engine::grade`, pure + unit-tested); per-action EV comes from `expected_values_detail`
  at the root (see DECISIONS F5: child-node navigation gives reference-shifted EVs, never use it).
- Review (Phase 5): the viz "review" mode ingests hand histories (PokerStars text or the JSON
  contract in `crates/history/src/schema.rs`) and reports AGGREGATE frequency leaks by node.
  Frequency, not sequence: no surface may emit a next-move recommendation. The GTO comparator is
  range-conditioned (D44, caveat ships in the payload); per-instance EV sanity asserts before
  any record enters `matrix/history/graded.jsonl` (D45, the F5 tripwire - epsilon scales by
  each record's own pot since schema v2). The envelope covers HU srp/3bp (50+100bb) and
  4bp (100bb) check-to flop decisions (seat-checked raise sequences, per-depth size bands);
  out-of-envelope decisions skip with precise reasons; never approximate or action-translate.
- Gates: `verify_viz.py` (Phase 2), `verify_solve.py` (Phase 3), `verify_trainer.py` (Phase 4),
  `verify_review.py` (Phase 5), `verify_desktop.py` (packaging: sidecar lifecycle,
  matrix resolution, maximised window, no-survivor teardown), `verify_navigator.py` (D55:
  reads reconcile cell-wise to the `node_grid` oracle, the reach-gate excludes what the measured
  tables say it must, and the confidence indicator provably separates a marginal node from a
  root-quality one - it reuses verify_desktop's launch/CDP helpers and the `node_grid` example
  as its read-only oracle), all run with the Python 3.12
  interpreter that has Playwright
  (`C:\Users\james\AppData\Local\Programs\Python\Python312\python.exe`). Every board a gate
  reserves lives in `verifier_boards.py` (partitioned registry: expected-cached,
  solve-consumable, trainer-consumable, absent-required). Each gate asserts the partitions
  pairwise disjoint at setup and sweeps consumable + absent boards out of the matrix, so
  fresh-solve pools never drain, absence fixtures self-heal, and the gates pass in any order.
  Add or move boards in the registry only, never inline in a verifier (hand-curated pools
  collided three times before the registry).

## Writing conventions
- No em dashes in any prose. Use hyphens, colons, or parentheses.
- Configuration is read from `gto-config`, never hardcoded in the engine.

## Backup (automatic)
At the end of every Claude Code session, and after completing any phase, run: git add -A; git commit -m "<short summary> (yyyy-MM-dd-HHmm)"; git push. Skip cleanly if nothing changed. Never commit secrets or the matrix/ solve cache (see .gitignore). Never force-push or rewrite history. Remote: private GitHub repo jlu1023/gto-poker-bot (since 2026-06-07). Notes live in the vault (Second Brain\Projects\GTO Poker Bot), backed by the second-brain repo.
