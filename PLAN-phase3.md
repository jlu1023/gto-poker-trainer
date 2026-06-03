# PLAN-phase3.md - GTO Poker Bot Phase 3 (solve-on-demand)

Status: PLAN (awaiting approval). Builds on Phase 2 (commit ee73a11).
No em dashes. Authoritative master plan: vault `GTO Poker Bot.md`.

## Goal

The operator types a flop in the viz and submits it. A live 50bb SRP solve runs on a local
Rust server; convergence streams in real time; the 13x13 strategy grid renders on completion.
Re-submitting the same board returns from the cache instantly.

## Scope (fixed)

- 50bb only. Single-raised pot only. Ranges from `crates/preflop` (`PreflopCharts::load_embedded`).
- Flop-only solve (the root, same as Phase 2). Board validation: 3 cards, no duplicates, legal rank +
  suit. No turn/river, no other depths.
- If `PreflopCharts::ranges_for` returns an error (unsupported depth): clear "unsupported spot" error.
- The operator's hand is NOT an input; it is only a reading aid after the grid renders.

---

## 1. Key decisions

### 1a. Bet tree for 50bb: reuse `srp-v1`

Phase 1 already stores `50bb_AcKhQd_flop_srp-v1` and `50bb_Qs8s5h_flop_srp-v1`. The sizing
(33%+75% flop, 75% turn/river) is pot-fraction-based and scales correctly to any depth. Using
the same `bet_tree_id = "srp-v1"` keeps spot keys consistent and re-uses cached Phase 1 boards
directly (`50bb_{board}_flop_srp-v1`). No `srp-50-v1` defined.

### 1b. Progress mechanism: SSE (Server-Sent Events)

The engine emits exploitability every 10 iterations. The server exposes this as an SSE stream
(`Content-Type: text/event-stream`). The browser `EventSource` API consumes it with no polling
overhead or WebSocket complexity. Events:
- `event: progress` - fired every 10 iterations; payload: `{iteration, exploitability_pct_pot}`
- `event: complete` - fired once on convergence / max-iter; payload: full SpotContract JSON
- `event: error` - fired on validation / RAM ceiling / unsupported-spot failure; payload: `{message}`

CORS: `Access-Control-Allow-Origin: *` on all responses. Localhost-only, no security concern.

### 1c. Server framework: axum 0.8 + tokio 1

Standard Rust async web framework. Native `axum::response::sse::Sse` type. `spawn_blocking` for
the CFR solve (which is CPU-bound and blocking). `tower-http` for CORS middleware. Single binary
(`crates/server`). New workspace member.

### 1d. Concurrency: single-solve-at-a-time

`Arc<tokio::sync::Mutex<SolveStatus>>` shared across handlers. States: `Idle`, `Running`. On a
submit while `Running`: emit an `error` event immediately ("a solve is already in progress, please
wait"). On a cache hit: emit `complete` immediately without touching the mutex.

---

## 2. Architecture

```
Operator
  |
  | (browser)
  v
viz (Vite/TS, port 5173/5247)
  |
  | POST /api/solve   {"board":["Ks","7h","2c"]}
  | GET  /api/health
  v
solve server (localhost, port 7474)
  |-- validate board + check preflop support
  |-- check manifest (gto-store::load_manifest)
  |   |-- HIT:  emit complete, return
  |   `-- MISS: spawn_blocking(solve_spot_streaming)
  |               -> progress events every 10 iters
  |               -> on convergence: validate::check_invariants
  |                                  gto-store::store_solution
  |                                  emit complete
  `-- emit error on any failure
```

The existing prebuilt-spot selector in the viz continues to work via the static JSON files. The
server and the static files are independent data sources; both produce `SpotContract` JSON.

---

## 3. Crate structure

### New crate: `crates/server`

```
crates/server/
  Cargo.toml           -- axum, tokio, tower-http, serde_json; internal: gto-engine, gto-preflop,
                          gto-store, gto-validate, gto-config
  src/
    main.rs            -- startup: load config, bind 0.0.0.0:7474, print ready message
    routes.rs          -- axum router + CORS layer
    handlers.rs        -- POST /api/solve (SSE), GET /api/health
    contract.rs        -- SpotContract serialization (same shape as export_viz; reuses the type)
    state.rs           -- Arc<Mutex<SolveStatus>>, AppState
```

Binary target (not an example). Not a library; nothing depends on it.

### Engine modification: `gto-engine`

Add a `SolveProgress` struct and `solve_spot_streaming`:

```rust
pub struct SolveProgress {
    pub iteration: u32,
    pub exploitability_pct_pot: f32,
}

/// Like solve_spot but calls on_progress after each exploitability check (every 10 iterations).
pub fn solve_spot_streaming<F>(spot: &Spot, cfg: &SolveConfig, on_progress: F) -> Result<SpotSolution>
where F: Fn(SolveProgress) + Send
```

`solve_spot` is left unchanged; internally it calls `solve_spot_streaming` with a no-op callback.
This is the only engine API change in Phase 3.

---

## 4. Solve flow (server side)

```
POST /api/solve  body: {"board": ["Ks","7h","2c"]}
```

1. Parse and validate board (3 cards, legal rank+suit, no duplicates).
2. Build the spot key: `50bb_{board}_flop_srp-v1`.
3. Check `gto-store::load_manifest`: if the key is present, `load_solution`, build
   `SpotContract`, emit `complete`. **Done; no solve.**
4. Acquire `SolveStatus` mutex. If `Running`: emit `error` ("solve already in progress"). Release.
5. Set `Running`. Release mutex.
6. `spawn_blocking`:
   a. `PreflopCharts::load_embedded()` -> `ranges_for(50bb SRP)`.
   b. Build `Spot` (board, pot_bb=5.0, eff_stack_bb=47.5, ranges, stack_depth_bb=50).
   c. Load `phase1.toml` config (or a server-supplied equivalent with srp-v1 tree).
   d. `solve_spot_streaming(spot, cfg, |p| tx.send(p))`.
   e. On completion: `validate::check_invariants`, `store_solution`, build `SpotContract`.
   f. Send `Done(contract)` or `Err(msg)` through a result channel.
7. In the SSE handler: `rx.recv()` on progress channel -> forward `progress` events;
   on `Done` -> emit `complete` event; on `Err` -> emit `error` event.
8. Set mutex back to `Idle` after solve completes or errors.

### RAM/time guard

`solve_spot_streaming` inherits the RAM ceiling from `cfg.solve.abstraction.ram_ceiling_gb` (same
guard already in `solve_spot`). `max_iterations` caps solve time. If RAM ceiling is breached, the
error propagates as an `error` SSE event: "solve would need X GiB (> Y GiB ceiling); the bet tree
was not cached." A `tokio::time::timeout` wraps the `spawn_blocking` future with a configurable
`solve_timeout_secs` (default 300 s); on timeout, emit `error` ("solve timed out after N seconds").

---

## 5. API

```
GET  /api/health
     200  {"ok": true}

POST /api/solve
     body: {"board": ["Ks","7h","2c"]}
     200  text/event-stream

SSE events:
  event: progress
  data: {"iteration":10,"exploitability_pct_pot":0.452}

  event: complete
  data: { ... SpotContract ... }

  event: error
  data: {"message":"illegal board: duplicate card 'Ks'"}
```

`SpotContract` is the same JSON shape the viz already consumes from Phase 2's static files
(`board`, `street`, `stack_depth_bb`, `pot_bb`, `eff_stack_bb`, `exploitability_pct_pot`,
`iterations`, `hero`, `villain`, `actions`, `hands[{hand,strategy,ev,equity}]`).

Board input is always an array of 3 card strings (normalized uppercase rank + lowercase suit:
`["Ks","7h","2c"]`). The front-end normalizes case before POSTing.

---

## 6. Front-end changes (`viz/`)

### New UI section: flop input

Below or alongside the existing `#spot` selector:

```
[ K  s ] [ 7  h ] [ 2  c ]   [Submit]   (or text field: "Ks7h2c")
```

Text-field format `"Ks7h2c"` (6 chars, space-optional, case-insensitive) is simpler to type and
validate than 6 individual select elements. A lightweight parser normalizes it.

Validation (client-side, before submit):
- Exactly 3 cards parseable from the input.
- Each card: rank in `{A K Q J T 9 8 7 6 5 4 3 2}`, suit in `{s h d c}`.
- No duplicate cards among the 3.
- Show inline error text, disable Submit if invalid.

### Progress bar

While the `EventSource` is open and receiving `progress` events:
- Show a progress bar labeled with the current exploitability (e.g. "0.23% of pot").
- Target line at 0.5% of pot (the `srp-v1` convergence target).
- The bar fills from current exploitability toward the target; fill = `target / exploitability`
  capped at 100%. This gives a natural "converging toward done" visual.

### Grid render on completion

On `event: complete`: parse the `SpotContract` JSON and pass it through the existing
`renderGrid` + `renderTable` + `renderDetail` pipeline (the same functions Phase 2 built). The
operator sees the same grid they would see for any prebuilt spot. The new spot also appears in the
`#spot` selector if the page refreshes (since it is now in the matrix).

### Cache-hit path

If the server returns `complete` with no `progress` events (cache hit), the UI skips the progress
bar and renders the grid immediately. The latency should feel instant.

### Error states

- Illegal board (client-side): inline text below the input, Submit disabled.
- Unsupported spot / server error: toast or inline message above the grid area.
- Solve already running: "A solve is in progress. Please wait."
- Solve timeout: "The solve timed out. Try a simpler board or increase the timeout."

---

## 7. Config

The server reads `phase1.toml` (same config the batch runner uses) so solve parameters are
consistent. The `matrix_dir` path must resolve correctly from wherever the server binary is run;
default is `matrix` (relative to CWD, which is the repo root). The server binary must be run from
the repo root: `cargo run --release -p gto-server`.

A `solve_timeout_secs` field is added to `IoConfig` (default 300). No other config changes.

---

## 8. Execution steps (Phase 3b)

1. Add `gto-server` crate: `Cargo.toml`, `src/main.rs` skeleton, add to workspace.
2. Add workspace deps: `axum = "0.8"`, `tokio = {version="1", features=["full"]}`,
   `tower-http = {version="0.6", features=["cors"]}`, `futures = "0.3"`.
3. Modify `gto-engine`: add `SolveProgress`, `solve_spot_streaming`; refactor `solve_spot`
   to call it internally. Add `solve_timeout_secs` to `IoConfig`.
4. Implement `crates/server/src/` (routes, handlers, state, contract).
5. Wire CORS + `GET /api/health`.
6. Wire `POST /api/solve` SSE handler (validate -> cache check -> spawn_blocking -> stream).
7. Implement `SpotContract` builder in `contract.rs` (from `SpotSolution`, same logic as
   `export_viz.rs::build_contract`).
8. Viz: new flop input component (`src/solve.ts`), `EventSource` client, progress bar, grid
   render on completion. Hook into existing `renderGrid` / `renderTable` / `renderDetail`.
9. End-to-end test (manual + Playwright): submit new board, watch convergence, verify grid.
   Re-submit same board, confirm instant cache hit. Submit illegal board, confirm error.
10. Run `verify_viz.py` to confirm Phase 2 gate still passes.
11. Commit Phase 3 (server source, front-end changes; matrix trees gitignored).
12. Vault: `Phase 3 - findings.md`, update `GTO Poker Bot.md`, update `Clod.md`.

---

## 9. DECISIONS.md additions (Phase 3)

- D27. Server framework = axum 0.8 + tokio 1; `tower-http` CORS; new crate `gto-server`.
- D28. Progress mechanism = SSE. `EventSource` API; `progress` / `complete` / `error` events.
  Polling rejected (unnecessary round-trips for a single blocking solve; SSE fits the data model).
- D29. Bet tree for 50bb = `srp-v1` (same as Phase 1 50bb spots). No `srp-50-v1` needed;
  sizing is pot-fraction-based and scales correctly.
- D30. Concurrency = single-solve-at-a-time (`Arc<Mutex<SolveStatus>>`). A second submit
  while a solve is running returns an `error` event immediately. Cache hits are lock-free.

---

## 10. Open questions / risks

| Question | Answer / mitigation |
|----------|---------------------|
| Solve latency on wet boards at 50bb | 50bb has lower SPR (~9.5) -> fewer bet nodes than 100bb -> typically converges FASTER. Wet boards (connected/flush-draw) have large ranges and may be slower; max_iterations cap + timeout guard prevent hangs. |
| CORS from Vite dev server | `Access-Control-Allow-Origin: *` on the Rust server. No credentials involved; localhost-only. Works for EventSource. |
| SSE and server shutdown | On browser disconnect, the `EventSource` closes the connection; the server detects EOF and drops the channel. The blocking solve thread runs to completion (it does not poll for cancellation); its result is discarded. This is acceptable: solving is fast enough that orphaned solves are self-limiting. If needed, a cancellation token can be added later. |
| Re-export after Phase 3 cache growth | `export_viz` re-run (or auto-run on server start) will pick up newly cached boards. Deferred; operator can run it manually. |
| Server and viz bundle together | Not this phase; Tauri packaging remains deferred. Operator runs two processes. |

---

## 11. Gate (Phase 3b)

- `cargo check --workspace --all-targets` clean; `npm run build` clean; server starts.
- Submit a NEW legal 50bb flop: real solve runs, progress events stream (exploitability decreasing),
  grid renders on `complete`. Invariants pass (logged by server).
- Re-submit the SAME flop: `complete` event arrives with no `progress` events; near-instant.
- Illegal board (duplicate card): inline error shown, Submit disabled, no server request.
- Unsupported spot: server returns `error` event, UI shows message.
- Phase 2 `verify_viz.py` still passes (prebuilt spots unaffected).
- Spot-check correctness: submit a 50bb board, compare the grid equity for one hand against
  a direct `cargo run --example smoke` solve of the same spot at 50bb.

---

## 12. Out of scope this phase

Turn/river navigation, deeper-node navigation, 100bb on-demand, 3-bet pots, multi-solve
queuing, Tauri packaging, distributed deployment, rate limiting, auth.
