# PLAN.md - GTO Poker Bot

Phase 0 plan for the HUNL GTO engine. The master plan in the Obsidian vault
(`GTO Poker Bot.md`) is the authoritative source of truth; this file is the engineering plan that
orients toward the full roadmap (HUNL engine, validation harness, visualization, then multiway, then
trainer) and executes Phase 0.

## 1. Engine dependency: `postflop-solver`
- Usable as a library, no fork required. Its API supports arbitrary node queries
  (`available_actions` / `play` / `is_chance_node` / `back_to_root` plus `strategy` /
  `expected_values` / `equity`), so the engine can drive any node, not just the root.
- Git-only, AGPL-3.0, development suspended (Oct 2023), and it makes breaking changes without
  version bumps, so it is pinned to a commit:
  ```toml
  postflop-solver = { git = "https://github.com/b-inary/postflop-solver", rev = "9d1509fe5077d019825f833eed04b16d342dfda1", features = ["zstd"] }
  ```
  Default features (`bincode`, `rayon`) kept; `zstd` added for compressed tree storage.
- Building on current stable Rust is part of the scaffold gate. If it fails, the fallback is to fork
  into our namespace and patch the dependency pins.

## 2. Workspace layout (5 members)
Under `crates/`: `config`, `engine`, `preflop`, `validate`, `store`. Dependency graph is a DAG:
`config` is the base; `engine` and `preflop` depend on `config`; `validate` and `store` depend on
`engine` + `config`. The smoke entry lives in `store/examples/smoke.rs`.

| Crate | Responsibility | Public interface |
|-------|----------------|------------------|
| config | Single source of truth for tunables; serde schema + `default.toml`. | `Config`, `SolveConfig`, `BetTreeConfig`, `AbstractionConfig`, `RakeConfig`, `StoreConfig`; `Config::load`, `Config::default` |
| engine | `Spot` model; map to solver; solve; read/navigate nodes; owns the solved game. | `Spot`, `Board`, `Street`, `NodeQuery`, `Step`, `NodeStrategy`, `SpotSolution`, `SpotSolutionSummary`; `solve_spot`, `query_node`, `read_root` |
| preflop | HU ranges as solver strings (import published charts). Phase 0 stub. | `PreflopCharts::load`, `ranges_for` |
| validate | Internal invariants; optional `rs_poker` / opensolver cross-check. | `check_invariants`, `strategy_normalized`, `cross_check_equity` |
| store | Precompute-and-cache: serialize solved trees + manifest. | `spot_key`, `store_solution`, `load_solution`, `load_manifest` |

## 3. Spot data model (highest risk)
postflop-solver solves a subgame from a chosen street. Preflop / earlier-street history is NOT passed
to the solver; it is encoded in three inputs: the two ranges, the starting pot, and the effective
stack. A `Spot` carries: nominal stack depth (label), initial street, board, pot (bb), effective
stack (bb), both range strings, and rake. `solve_spot` converts bb to integer chips via
`chip_per_bb`, builds `CardConfig` + `TreeConfig` + `ActionTree`, allocates (with a RAM-ceiling
guard), solves to target, and reads the root. `query_node` walks to an arbitrary node using semantic
`Step`s (Action / Chance) resolved to indices at query time, then reads strategy / EV / equity.
`SpotSolution` owns the solved `PostFlopGame` so it persists natively and supports node queries after
reload with zero solving.

## 4. Preflop chart sourcing - DECISION: import published charts
HU preflop is fully solved and postflop-solver is postflop-only, so HU ranges are imported from a
published set and fed as fixed inputs. Importing is faster and lower-risk than writing and validating
our own preflop CFR. Phase 0 ships a stub returning hardcoded single-raised-pot ranges; Phase 1
parses a published HU set (open / 3bet / 4bet / 5bet by stack depth) into range strings.

## 5. Storage and the matrix (precompute-and-cache)
Solve a curated set once, store to `matrix/`, look up with zero solving. Layout:
`matrix/<key>.bin[.zst]` (solved tree, native bincode + optional zstd), `matrix/<key>.summary.json`
(spot + metadata + root), `matrix/manifest.json` (key -> metadata index). `load_solution`
deserializes the tree, reads the sidecar, recomputes the root. v1 is flat-files-plus-manifest only;
a SQLite index over the files is the deferred migration. Out-of-set spots get exact-key lookup only
in Phase 0; nearest-node mapping and solve-then-cache are deferred.

## 6. Validation strategy (no commercial solver assumed)
Primary = internal invariants, run after every solve: exploitability <= target; per-node strategy
normalizes to 1 for every hand; zero-sum EV between players. The curated stored matrix IS the
benchmark set (one list), with spots chosen partly for verifiability. Optional external cross-check
(Phase 1 onward): re-solve with opensolver (UPI) and recompute equity with `rs_poker`, starting on
rainbow flops (opensolver isomorphism is incomplete on two-tone / monotone). Reference-solve
sourcing open question: use opensolver + `rs_poker` as the external reference; confirm opensolver
license before depending on it.

## 7. config as single source of truth
`crates/config/default.toml` owns: chip resolution (`chip_per_bb`); stack depths; target
exploitability (% of pot), iteration cap, progress flag; per-street per-player bet / raise / donk
size trees plus a `bet_tree_id`; abstraction params (compression, all-in and merging thresholds,
RAM ceiling); rake; and IO / store settings (`out_dir`, `matrix_dir`, zstd).

## 8. Open questions and proposed resolutions
1. Reference-solve source: opensolver (UPI) + `rs_poker` equity; internal invariants are the gate.
2. Pin upstream or track main: pin a commit (main breaks silently).
3. Build on stable: verified at the scaffold gate; fork + patch if it fails.
4. AGPL-3.0: no obligation for personal offline use; a shipped viz must comply later.
5. Fractional bet sizes: `chip_per_bb` keeps sizes integer.
6. Preflop chart source / format: import a published HU set; finalized Phase 1.
7. Real-spot RAM ceiling: `ram_ceiling_gb` + compression; shrink the tree on overflow.
8. Out-of-set spots: Phase 0 exact-key lookup; nearest-node mapping + solve-then-cache deferred.
9. Matrix index scaling: flat files + manifest at v1; SQLite index deferred.
10. Packaging (Tauri vs WASM): deferred; v1 dumps JSON for a local viewer.

## Phase 0 deliverables and gate
- Compiling workspace (`cargo check --workspace --all-targets` clean).
- Smoke (`cargo run --release -p gto-store --example smoke`) converges below target exploitability,
  proves the store round-trip (reloaded root strategy matches), and writes `out/smoke.json` plus a
  `matrix/` entry.
