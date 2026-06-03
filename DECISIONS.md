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
