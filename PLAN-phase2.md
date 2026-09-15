# PLAN-phase2.md - GTO Poker Bot Phase 2 (local visualization)

Status: PLAN (awaiting approval). Authoritative master plan: the author's private project notes. Builds on the
Phase 1 matrix (8 stored spots). No em dashes. Hard constraint: render everything ORIGINAL (CSS/SVG),
no scraped/branded/copyrighted poker assets, draw card faces ourselves.

## Goal
A local, read-only visualization: pick one of the 8 stored spots, render a poker table view of it, and
surface its solved strategy (action frequencies + EV + equity) from the Phase 1 matrix. The data behind
the table is real (read from the matrix), never mocked.

## 1. Packaging decision (RESOLVED): native Rust JSON export + static Vite/TS front-end
The viz only READS stored solves (no live solving in the UI), which collapses the Tauri-vs-WASM
tradeoff. Adversarially verified (workflow): WASM is impractical (zstd-sys will not C-compile to
wasm32; rayon needs nightly + atomics + COOP/COEP; postflop-solver has no wasm tree-loader; eager
39 MB bincode in-browser) and gives ZERO benefit for read-only. Tauri (a runtime Rust backend) is
overkill for static read-only data.

- **v1 = Architecture A:** a NATIVE Rust exporter reads the matrix and writes per-spot JSON; a static
  Vite + TypeScript front-end (vanilla TS, original CSS/SVG) reads that JSON. This is the master-plan
  v1 path ("engine dumps JSON, render with a local Vite app").
- **Tauri is the chosen EVENTUAL packaging** (over WASM) for when interactive re-query or live solving
  is added: it keeps the native engine and can wrap the same Vite front-end (swap `fetch` for `invoke`).
  Deferred. The front-end is built Tauri-ready.
- **WASM rejected** (recorded as finding F4).

**Data path:** `matrix/manifest.json` + the stored trees -> `gto-store` example `export_viz`
(native Rust; `gto_store::load_manifest` to enumerate, `gto_store::load_solution` per spot, read the
root) -> `viz/public/data/index.json` + `viz/public/data/<key>.json` -> Vite/TS `fetch`. No solving in
the UI. `query_node` is available for deeper nodes later; v1 reads the ROOT only.

## 2. Data contract (read-only, from gto-store load_solution -> root)
`viz/public/data/index.json`:
```json
{ "spots": [ { "key": "100bb_Ks7h2c_flop_srp-v1", "board": ["Ks","7h","2c"],
              "stack_depth_bb": 100, "exploitability_pct_pot": 0.4278 } ] }
```
`viz/public/data/<key>.json` (one spot; the exporter reshapes the solver's
`strategy[hand + action*n]` into clean per-hand arrays):
```json
{
  "key": "100bb_Ks7h2c_flop_srp-v1",
  "board": ["Ks","7h","2c"], "street": "flop",
  "stack_depth_bb": 100, "pot_bb": 5.0, "eff_stack_bb": 97.5,
  "exploitability_pct_pot": 0.4278, "iterations": 90,
  "hero": { "player": 0, "position": "OOP", "seat": "BB" },
  "villain": { "position": "IP", "seat": "BTN" },
  "actions": ["Check","Bet(33)","Bet(75)"],
  "hands": [ { "hand": "5c4c", "strategy": [0.21,0.50,0.29], "ev": 1.2, "equity": 0.45 } ]
}
```
The root player is OOP (BB), so the readout is OOP's flop strategy.

## 3. Table view (original CSS/SVG; real tables referenced for proportions only)
- Felt: a green ellipse (SVG `<ellipse>` or CSS), centered, aspect ~1.6:1.
- Hero: bottom-center, seat label "BB (OOP)", two FACE-UP cards = the currently selected hand, drawn as
  original SVG cards (rank glyph + suit symbol; red for h/d, dark for s/c; rounded rect, no image packs).
- Villain: top-center, seat label "BTN (IP)", two FACE-DOWN card backs (original pattern; it is a range,
  not a specific hand).
- Board: center row, the 3 flop cards (original SVG).
- Pot: label above the board ("Pot 5.0 bb"), with stack-depth / exploitability shown in a small HUD.

## 4. Strategy readout (13x13 grid primary; per-hand fallback)
- **Primary: a 13x13 hand-class grid.** Rows/cols A..2; suited above the diagonal, offsuit below, pairs
  on the diagonal. Each cell aggregates the per-combo root strategy to the 169 hand classes (front-end
  maps "AsKs" -> class "AKs", averages combos in the class). Cell fill encodes the OOP action mix
  (stacked mini-bar per action, distinct hue per action: check / bet33 / bet75 / allin). Clicking a cell
  selects that hand: the felt shows it as the hero cards and a detail panel shows the action mix %, EV,
  and equity.
- **Detail panel:** for the selected hand, action-frequency bars + EV + equity (exact numbers).
- **Fallback (abort condition):** if the 13x13 grid proves heavy or unclear with multiple bet sizes,
  fall back to a per-hand readout (searchable hand selector + the detail panel) for v1 and document it.

## 5. Spot selector
A selector listing the 8 spots from `index.json` (board, depth, exploitability). Choosing a spot fetches
its `<key>.json` and re-renders the table + grid. Empty state: if a spot's JSON is missing (matrix not
exported), show a clear "no solved data - run build_matrix + export_viz" message rather than mocking.

## 6. Open questions / risks
1. Node/Vite toolchain presence on the machine - verify in 2b; if absent, surface it.
2. 13x13 cell encoding clarity with 2-3 bet sizes (stacked mini-bars vs a blended color) - a 2b design
   choice, settled with the frontend-design / impeccable skills.
3. Exported JSON size: per spot ~ a few hundred hands x a few actions = well under 100 KB; fine to track.
4. Deeper-node navigation (turn/river, post-bet nodes via `query_node`) deferred; v1 is the root only.

## DECISIONS.md additions (Phase 2)
- D23. Packaging = native Rust JSON export + static Vite/TS front-end for the read-only v1. Tauri is the
  chosen eventual packaging (over WASM); deferred; front-end built Tauri-ready. WASM rejected.
- D24. Data path = matrix -> `export_viz` (native, via `load_manifest` + `load_solution`) ->
  `viz/public/data/*.json` -> Vite/TS fetch. Read-only; no solving in the UI; root node only for v1.
- D25. Data contract = per-spot JSON (board, street, stacks, pot, exploitability, hero/villain seats,
  actions, hands[{hand, strategy, ev, equity}]) + `index.json`.
- D26. Strategy readout = 13x13 OOP root action-frequency grid (primary) + per-hand detail panel; a
  per-hand-only readout is the documented fallback if the grid is heavy.
- F4 (finding). WASM rejected for the viz: zstd-sys will not C-compile to wasm32; rayon needs
  nightly + atomics + COOP/COEP; postflop-solver has no wasm tree-loader; eager 39 MB bincode in-browser
  is wasteful; and a read-only viz needs no engine in the browser at all.

## Phase 2b - execution steps (on approval)
1. `crates/store/examples/export_viz.rs`: native exporter (manifest -> per-spot JSON + index), reshaping
   the root strategy into per-hand arrays. Empty-state handling if a tree/summary is missing.
2. `viz/`: Vite + TS project (vanilla TS, original CSS/SVG): `cards.ts` (SVG card faces + backs),
   `table.ts` (felt + seats + board + pot), `grid.ts` (13x13 aggregation + render + selection),
   `main.ts` (spot selector + fetch + wiring), `types.ts` (the data contract). Use the frontend-design +
   impeccable skills for the UI.
3. Track `viz/` source + config + the small exported `viz/public/data/*.json`; never the large trees.
4. Verify it renders all 8 spots; spot-check that the readout matches the stored solution (e.g. the
   100bb_Ks7h2c equity validated in Phase 1).
5. Commit Phase 2.
6. Vault: master plan status -> phase-2; a `Phase 2 - findings` note (packaging decision + data path +
   rendering constraints), bidirectional links; tools-inventory row updated with the front-end stack.

## Verification / GO-NO-GO (2b gate)
- `cargo check --workspace --all-targets` clean; the `export_viz` exporter writes JSON for all 8 spots; a
  clean front-end build (`npm run build`) for the Vite/TS app.
- The viz loads and renders each of the 8 stored spots from the real matrix; board, hero cards, villain
  seat, and pot correct per `spots.toml`.
- The strategy readout matches the stored solution for a spot-checked hand.
- No external/branded/copyrighted assets; all card and table rendering is original.

## Out of scope this phase
Live solving in the UI, Tauri packaging, deeper-node navigation, multiway, trainer.
