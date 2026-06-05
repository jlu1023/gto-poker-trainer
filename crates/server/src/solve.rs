//! POST /api/solve: the flop submission IS the solve request.
//!
//! Flow: canonicalize + validate the board -> ranges (unsupported-spot check) -> cache check
//! against the matrix manifest (primary key, then the `-shrunk` variant) -> on miss, run
//! `solve_spot_streaming` on a blocking thread, gate on invariants, `store_solution`, return.
//! Everything streams back over one SSE response: `progress` / `complete` / `error` events.
//!
//! Phase 3 scope: 50bb single-raised pots only. The SRP geometry matches `build_matrix`
//! (BTN raises 2.5bb, BB calls: pot 5bb, effective stack = depth - 2.5) so on-demand keys are
//! interchangeable with the Phase 1 benchmark entries.

use axum::extract::{Json, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::Stream;
use gto_config::Config;
use gto_engine::{solve_spot_streaming, Board, RakeSpec, SolveProgress, Spot, Street};
use gto_preflop::{PreflopContext, PreflopNode};
use gto_store::viz::{build_contract, SpotContract};
use gto_store::{load_manifest, load_solution, spot_key, store_solution};
use gto_validate::check_invariants;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::state::AppState;

/// SRP geometry (matches build_matrix): BTN raises 2.5bb, BB calls.
pub(crate) const SRP_POT_BB: f64 = 5.0;
pub(crate) const SRP_RAISE_BB: f64 = 2.5;
/// Phase 3 default; Phase 4 makes depth selectable (any depth with authored SRP ranges).
pub(crate) const DEFAULT_DEPTH_BB: u32 = 50;

pub(crate) fn default_depth() -> u32 {
    DEFAULT_DEPTH_BB
}

#[derive(Debug, Deserialize)]
pub struct SolveRequest {
    /// Three card strings, any order/case, e.g. ["Ks","7h","2c"].
    pub board: Vec<String>,
    /// Stack depth in bb. Supported depths = the authored SRP ranges (50, 100); anything else
    /// returns the ranges error. Defaults to 50 (Phase 3 back-compat).
    #[serde(default = "default_depth")]
    pub depth: u32,
}

/// Canonicalize + validate a board, fetch the authored SRP ranges for `depth`, and build the
/// `Spot` + cache key. Shared by the solve, setup, and grade handlers so every path produces
/// identical keys and geometry.
pub(crate) fn build_srp_spot(
    state: &AppState,
    board: &[String],
    depth: u32,
) -> Result<(Spot, String), String> {
    let flop = canonical_board(board)?;
    let (oop_range, ip_range) = state
        .charts
        .ranges_for(&PreflopContext {
            stack_depth_bb: depth,
            node: PreflopNode::SingleRaisedPot,
        })
        .map_err(|e| format!("ranges not available for this spot: {e}"))?;
    let spot = Spot {
        stack_depth_bb: depth,
        initial_street: Street::Flop,
        board: Board { flop, turn: None, river: None },
        pot_bb: SRP_POT_BB,
        effective_stack_bb: depth as f64 - SRP_RAISE_BB,
        oop_range,
        ip_range,
        rake: RakeSpec::default(),
    };
    let key = spot_key(&spot, &state.cfg.solve.bet_tree);
    Ok((spot, key))
}

/// Everything that can travel down the SSE stream.
enum Update {
    Progress(SolveProgress),
    Complete { cached: bool, shrunk: bool, contract: Box<SpotContract> },
    Failed { message: String },
}

impl Update {
    fn into_event(self) -> Event {
        match self {
            Update::Progress(p) => sse_event("progress", &p),
            Update::Complete { cached, shrunk, contract } => sse_event(
                "complete",
                &serde_json::json!({ "cached": cached, "shrunk": shrunk, "spot": contract }),
            ),
            Update::Failed { message } => {
                sse_event("error", &serde_json::json!({ "message": message }))
            }
        }
    }
}

fn sse_event(name: &str, payload: &impl Serialize) -> Event {
    Event::default()
        .event(name)
        .data(serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string()))
}

/// The SSE handler. Returns the stream immediately; all work happens in spawned tasks and is
/// reported as events. Exactly one terminal event (`complete` or `error`) per request.
pub async fn solve_handler(
    State(state): State<AppState>,
    Json(req): Json<SolveRequest>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (tx, rx) = mpsc::channel::<Update>(256);
    tokio::spawn(orchestrate(state, req, tx));

    let stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|u| (Ok::<Event, Infallible>(u.into_event()), rx))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn orchestrate(state: AppState, req: SolveRequest, tx: mpsc::Sender<Update>) {
    let terminal = match run(&state, req, &tx).await {
        Ok(complete) => complete,
        Err(message) => Update::Failed { message },
    };
    // the receiver may already be gone (client disconnected); nothing to do about it
    let _ = tx.send(terminal).await;
    // tx drops here; the channel closes and the SSE stream ends
}

async fn run(
    state: &AppState,
    req: SolveRequest,
    tx: &mpsc::Sender<Update>,
) -> Result<Update, String> {
    // canonical board + ranges (unsupported pot type / depth -> clear error, never guessed)
    let (spot, key) = build_srp_spot(state, &req.board, req.depth)?;
    // a board that previously hit the RAM ceiling is cached under the shrunk tree id
    let shrunk_key = format!("{key}-shrunk");

    // ---- cache check (lock-free; the matrix is the cache) ----
    let dir = Path::new(&state.cfg.io.matrix_dir);
    let manifest = load_manifest(dir).map_err(|e| format!("reading matrix manifest: {e}"))?;
    let hit = if manifest.spots.contains_key(&key) {
        Some((key.clone(), false))
    } else if manifest.spots.contains_key(&shrunk_key) {
        Some((shrunk_key.clone(), true))
    } else {
        None
    };
    if let Some((hit_key, shrunk)) = hit {
        let cfg = state.cfg.clone();
        let contract = tokio::task::spawn_blocking(move || {
            let mut solution = load_solution(&hit_key, &cfg)?;
            Ok::<_, anyhow::Error>(build_contract(&hit_key, &mut solution))
        })
        .await
        .map_err(|e| format!("cache load task failed: {e}"))?
        .map_err(|e| format!("loading cached solve: {e}"))?;
        return Ok(Update::Complete { cached: true, shrunk, contract: Box::new(contract) });
    }

    // ---- single-solve slot (D30) ----
    let permit = match state.solve_slot.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => return Err("a solve is already in progress; wait for it to finish".to_string()),
    };

    // ---- fresh solve on a blocking thread ----
    let cfg = state.cfg.clone();
    let progress_tx = tx.clone();
    let board_label = spot.board.flop.concat();
    println!("solving {key} ...");
    let solve_task = tokio::task::spawn_blocking(move || {
        // the permit lives exactly as long as the CPU work, even if the request times out
        let _permit = permit;
        solve_store_contract(&spot, &cfg, &progress_tx)
    });

    let timeout = Duration::from_secs(state.cfg.io.solve_timeout_secs);
    match tokio::time::timeout(timeout, solve_task).await {
        Ok(Ok(Ok((contract, shrunk)))) => {
            println!(
                "solved {board_label}: {:.4}% pot in {} iters{}",
                contract.exploitability_pct_pot * 100.0,
                contract.iterations,
                if shrunk { " (shrunk tree)" } else { "" }
            );
            Ok(Update::Complete { cached: false, shrunk, contract: Box::new(contract) })
        }
        Ok(Ok(Err(e))) => Err(format!("solve failed: {e}")),
        Ok(Err(join)) => Err(format!("solve task panicked: {join}")),
        Err(_) => Err(format!(
            "solve timed out after {}s; it keeps running in the background and will be served \
             from cache if it completes",
            state.cfg.io.solve_timeout_secs
        )),
    }
}

/// The blocking path: solve (with the RAM-ceiling shrink fallback, same policy as build_matrix),
/// gate on invariants, store to the matrix, build the viz contract. Returns (contract, shrunk?).
fn solve_store_contract(
    spot: &Spot,
    cfg: &Config,
    tx: &mpsc::Sender<Update>,
) -> anyhow::Result<(SpotContract, bool)> {
    // lossy progress: never block the solver on a slow client
    let progress = |p: SolveProgress| {
        let _ = tx.try_send(Update::Progress(p));
    };

    let (mut solution, used_cfg, shrunk) = match solve_spot_streaming(spot, &cfg.solve, progress) {
        Ok(s) => (s, cfg.clone(), false),
        Err(e) if e.to_string().contains("ceiling") => {
            // shrink the flop tree to a single size and retry (noted in the response)
            let mut sc = cfg.clone();
            sc.solve.bet_tree.flop_bet_sizes = ["33%".to_string(), "33%".to_string()];
            sc.solve.bet_tree.bet_tree_id = format!("{}-shrunk", cfg.solve.bet_tree.bet_tree_id);
            let progress = |p: SolveProgress| {
                let _ = tx.try_send(Update::Progress(p));
            };
            let s = solve_spot_streaming(spot, &sc.solve, progress)?;
            (s, sc, true)
        }
        Err(e) => return Err(e),
    };

    // invariants gate BEFORE caching or returning: never serve or store an unvalidated solve
    // (&mut: the game-layer checks navigate the tree and return it to root)
    let report = check_invariants(&mut solution, &used_cfg);
    if !report.passed() {
        anyhow::bail!("solve completed but failed invariants: {:?}", report.notes);
    }

    store_solution(&solution, &used_cfg)?;
    let key = spot_key(&solution.spot, &used_cfg.solve.bet_tree);
    let contract = build_contract(&key, &mut solution);
    Ok((contract, shrunk))
}

/// Validate and canonicalize a submitted flop (delegates to the shared gto-store
/// implementation so the server and the Phase 5 history classifier can never drift).
pub(crate) fn canonical_board(cards: &[String]) -> Result<[String; 3], String> {
    gto_store::canonical_flop(cards).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(cards: &[&str]) -> Vec<String> {
        cards.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn canonicalizes_order_and_case() {
        // any typing order / case of the same flop -> one canonical key order
        assert_eq!(canonical_board(&b(&["2c", "KS", "7h"])).unwrap(), ["Ks", "7h", "2c"]);
        assert_eq!(canonical_board(&b(&["7d", "7h", "2c"])).unwrap(), ["7h", "7d", "2c"]);
        assert_eq!(canonical_board(&b(&["qd", "ac", "kh"])).unwrap(), ["Ac", "Kh", "Qd"]);
    }

    #[test]
    fn rejects_bad_boards() {
        assert!(canonical_board(&b(&["Ks", "7h"])).is_err()); // 2 cards
        assert!(canonical_board(&b(&["Ks", "Ks", "2c"])).is_err()); // duplicate
        assert!(canonical_board(&b(&["Xs", "7h", "2c"])).is_err()); // bad rank
        assert!(canonical_board(&b(&["Kx", "7h", "2c"])).is_err()); // bad suit
        assert!(canonical_board(&b(&["K", "7h", "2c"])).is_err()); // malformed
    }
}
