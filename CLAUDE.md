# CLAUDE.md - gto-poker-bot

Build conventions and constraints for this repo. Read alongside PLAN.md and DECISIONS.md.

## Source of truth
The master plan at `C:\Users\james\OneDrive\Second Brain\Projects\GTO Poker Bot\GTO Poker Bot.md`
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
Five crates under `crates/`:
- `config`  (`gto-config`)  - single source of truth for every tunable; serde schema + `default.toml`.
- `engine`  (`gto-engine`)  - `Spot` model, spot-to-solver mapping, `solve_spot`, `query_node`.
- `preflop` (`gto-preflop`) - HU preflop range provider (imports published charts; Phase 0 stub).
- `validate`(`gto-validate`)- internal invariants + optional `rs_poker` / opensolver cross-check.
- `store`   (`gto-store`)   - precompute-and-cache: serialize solved trees + manifest; `examples/smoke.rs`.

Dependency graph (DAG): `config` is the base; `engine` and `preflop` depend on `config`; `validate`
and `store` depend on `engine` + `config`. The smoke entry lives in `store/examples/smoke.rs`.

## Spot model (the highest-risk piece)
postflop-solver solves a subgame from a chosen street. Preflop / earlier-street history is NOT passed
to the solver; it is encoded in three inputs: the two ranges, the starting pot, and the effective
stack. Money is carried in big blinds and converted to integer chips via `chip_per_bb`. Node reads
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

## Writing conventions
- No em dashes in any prose. Use hyphens, colons, or parentheses.
- Configuration is read from `gto-config`, never hardcoded in the engine.
