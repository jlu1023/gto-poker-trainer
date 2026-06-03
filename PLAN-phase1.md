# PLAN-phase1.md - GTO Poker Bot Phase 1

Status: EXECUTED 2026-06-03. Gate green (see Results). The authoritative master plan is the vault note
`GTO Poker Bot.md`; durable findings are in the vault `Phase 0 - findings.md`.

## Goal
Replace the hardcoded preflop stub with a real authored HU range set, stand up the first curated
benchmark batch in the matrix, and wire a live rs_poker equity cross-check. The opensolver cross-check
is dropped (no license + isomorphism disabled). No em dashes.

## 1. Preflop loader (authored ranges)
HU preflop import was BLOCKED (no clean free importable set; the top candidate was refuted by
adversarial verification - its range data does not exist). Decision: author ranges in-repo.
- `crates/preflop/data/hu_ranges.toml`: authored HU SRP ranges for 100bb and 50bb (`oop` = BB postflop,
  `ip` = BTN postflop), from established HU GTO theory. Our data, license-clean.
- `crates/preflop/src/lib.rs`: `PreflopCharts::load_embedded()` (data embedded via `include_str!`,
  CWD-independent) and `load(path)` (disk override); `ranges_for(ctx)` returns `(oop, ip)` or errors.
- Tests: every range parses as a valid `postflop_solver::Range`; membership (AA present, 72o absent);
  missing depth errors.

## 2. Curated benchmark batch (8 SRP flop spots) = the benchmark set
`benchmark/spots.toml`, the one list. Context: single-raised pot, BB OOP vs BTN IP.

| board | texture | depth |
|-------|---------|-------|
| Ks7h2c | dry rainbow high | 100 |
| 9c5d3h | dry rainbow low | 100 |
| AcKhQd | broadway rainbow | 50 |
| Jc9h8c | two-tone (clubs) | 100 |
| Kh9h4h | monotone hearts | 100 |
| 7h7d2c | paired (sevens), rainbow | 100 |
| 6c5d4h | low connected, rainbow | 100 |
| Qs8s5h | two-tone (spades) | 50 |

Spot keys `{depth}bb_{board}_flop_srp-v1`. Bet tree `srp-v1` (`phase1.toml`): flop `33%,75%`,
turn/river `75%`, no raises, compression on. Chip scale 20: pot 100; eff 1950 (100bb) / 950 (50bb).

## 3. Matrix runner
`crates/store/examples/build_matrix.rs`: loads `phase1.toml` + embedded charts + `benchmark/spots.toml`;
per spot solves, asserts invariants, stores (tree + summary + manifest), logs. RAM-ceiling shrink
fallback (single flop size) per the abort condition. Run: `cargo run --release -p gto-store --example build_matrix`.

## 4. rs_poker equity cross-check (live)
`validate::cross_check_equity`: deterministic EXHAUSTIVE turn+river enumeration of one hero hand vs the
opponent range on the flop, compared to the solver's `root.equity[hero_index]`, tolerance 2%. rs_poker
is used only for card representation + 7-card ranking; the enumeration and tally are ours. (The research
agent confabulated an `OutsCalculator`; the real non-arena primitives are `RangeParser` + `Hand::rank`.)

## Results (gate green)
- `cargo check --workspace --all-targets` clean. `cargo test -p gto-preflop`: 3/3 pass.
- All 8 curated spots solved below the 0.5% target (0.38% to 0.49% of pot), invariants passed, stored,
  none required shrinking. Manifest holds the 8 `srp-v1` entries (plus the Phase 0 `smoke-v0` spot).
- Equity cross-check on `100bb_Ks7h2c` hero `2h2d`: solver 0.9535 vs rs_poker 0.9535, |diff| 0.0000
  (592 villain combos). Exact agreement.

## Out of scope
20bb ranges, richer production bet trees, nearest-node mapping, solve-then-cache, SQLite index, a
licensed external reference solver, visualization, multiway, trainer.
