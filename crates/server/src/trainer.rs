//! Phase 4 trainer endpoints.
//!
//! GET  /api/setup  - pre-commit data ONLY: legal root actions (derived from the bet-tree
//!                    config, no solve), whether the hand is in the OOP range (preflop chart
//!                    data), and whether the spot is already cached. No strategy, EV, or
//!                    equity leaves the server here: commit-then-reveal is enforced by data
//!                    absence (D35).
//! POST /api/grade  - after the solve: locate the operator's exact combo in the stored root,
//!                    resolve the committed action (off-tree maps to nearest with a notice),
//!                    and grade it with the config verdict bands (D32/D36).

use axum::extract::{Json, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use gto_engine::grade::{find_hand, grade, map_action, normalize_hand};
use gto_engine::{root_actions, hand_range_weight, NodeStrategy};
use gto_store::{load_manifest, load_solution, load_summary};
use serde::Deserialize;
use serde_json::json;
use std::path::Path;

use crate::solve::{build_srp_spot, default_depth};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct SetupQuery {
    /// Board as one string, e.g. "8d6s3c".
    pub board: String,
    /// The operator's exact combo, e.g. "AhKh".
    pub hand: String,
    #[serde(default = "default_depth")]
    pub depth: u32,
}

#[derive(Debug, Deserialize)]
pub struct GradeRequest {
    pub board: Vec<String>,
    pub hand: String,
    /// The committed action label, e.g. "Bet(33)".
    pub action: String,
    #[serde(default = "default_depth")]
    pub depth: u32,
}

fn bad_request(message: String) -> Response {
    (StatusCode::BAD_REQUEST, axum::Json(json!({ "error": message }))).into_response()
}

fn split_board_string(board: &str) -> Result<Vec<String>, String> {
    let s: String = board.chars().filter(|c| !c.is_whitespace() && *c != ',').collect();
    if s.len() != 6 {
        return Err(format!("board '{board}' must be 3 cards like 8d6s3c"));
    }
    Ok(vec![s[0..2].to_string(), s[2..4].to_string(), s[4..6].to_string()])
}

/// The two cards of a normalized hand label.
fn hand_cards(normalized: &str) -> [String; 2] {
    [normalized[0..2].to_string(), normalized[2..4].to_string()]
}

pub async fn setup_handler(
    State(state): State<AppState>,
    Query(q): Query<SetupQuery>,
) -> Response {
    let board = match split_board_string(&q.board) {
        Ok(b) => b,
        Err(e) => return bad_request(e),
    };
    let (spot, key) = match build_srp_spot(&state, &board, q.depth) {
        Ok(v) => v,
        Err(e) => return bad_request(e),
    };

    let hand = match normalize_hand(&q.hand) {
        Some(h) => h,
        None => return bad_request(format!("malformed hand '{}' (want e.g. AhKh)", q.hand)),
    };
    // hand must not collide with the board
    let cards = hand_cards(&hand);
    if spot.board.flop.iter().any(|b| cards.contains(b)) {
        return bad_request(format!("hand {hand} collides with the board"));
    }

    // preflop chart membership (an authored input, not solver output)
    let weight = match hand_range_weight(&spot.oop_range, &hand) {
        Ok(w) => w,
        Err(e) => return bad_request(format!("hand check failed: {e}")),
    };

    // legal root actions from the bet-tree config alone: no game build, no solve
    let actions = match root_actions(&spot, &state.cfg.solve) {
        Ok(a) => a,
        Err(e) => return bad_request(format!("deriving root actions: {e}")),
    };

    // cache flag (manifest key lookup only)
    let dir = Path::new(&state.cfg.io.matrix_dir);
    let cached = match load_manifest(dir) {
        Ok(m) => {
            m.spots.contains_key(&key) || m.spots.contains_key(&format!("{key}-shrunk"))
        }
        Err(_) => false,
    };

    axum::Json(json!({
        "key": key,
        "depth": q.depth,
        "board": spot.board.flop,
        "hand": hand,
        "hand_in_range": weight > 0.0,
        "actions": actions,
        "cached": cached,
    }))
    .into_response()
}

pub async fn grade_handler(
    State(state): State<AppState>,
    Json(req): Json<GradeRequest>,
) -> Response {
    let (spot, key) = match build_srp_spot(&state, &req.board, req.depth) {
        Ok(v) => v,
        Err(e) => return bad_request(e),
    };

    let hand = match normalize_hand(&req.hand) {
        Some(h) => h,
        None => return bad_request(format!("malformed hand '{}' (want e.g. AhKh)", req.hand)),
    };

    // the spot must already be solved (the trainer always solves before grading)
    let dir = Path::new(&state.cfg.io.matrix_dir);
    let manifest = match load_manifest(dir) {
        Ok(m) => m,
        Err(e) => return bad_request(format!("reading matrix manifest: {e}")),
    };
    let stored_key = if manifest.spots.contains_key(&key) {
        key.clone()
    } else if manifest.spots.contains_key(&format!("{key}-shrunk")) {
        format!("{key}-shrunk")
    } else {
        return (
            StatusCode::CONFLICT,
            axum::Json(json!({ "error": format!("spot {key} not solved yet; solve it first") })),
        )
            .into_response();
    };

    // summary sidecar first (cheap); fall back to a full tree load for sidecars stored before
    // per-action EV existed (load_solution recomputes the root, which now carries it)
    let root: NodeStrategy = {
        let fast = load_summary(&stored_key, &state.cfg).ok().and_then(|s| {
            let r = s.root;
            (r.ev_per_action.len() == r.hands.len() * r.actions.len() && !r.hands.is_empty())
                .then_some(r)
        });
        match fast {
            Some(r) => r,
            None => {
                let cfg = state.cfg.clone();
                let sk = stored_key.clone();
                match tokio::task::spawn_blocking(move || load_solution(&sk, &cfg)).await {
                    Ok(Ok(solution)) => solution.root,
                    Ok(Err(e)) => return bad_request(format!("loading stored solve: {e}")),
                    Err(e) => return bad_request(format!("load task failed: {e}")),
                }
            }
        }
    };

    // locate the exact combo; distinguish board-blocked from out-of-range
    let hand_idx = match find_hand(&root, &hand) {
        Some(i) => i,
        None => {
            let cards = hand_cards(&hand);
            let msg = if spot.board.flop.iter().any(|b| cards.contains(b)) {
                format!("hand {hand} collides with the board")
            } else {
                format!("hand {hand} is not in the BB's preflop range for this spot")
            };
            return bad_request(msg);
        }
    };

    // resolve the committed action; off-tree maps to nearest with a notice
    let (action_idx, mapped_from) = match map_action(&root.actions, &req.action) {
        Some(v) => v,
        None => {
            return bad_request(format!(
                "action '{}' not available at this node; available: {:?}",
                req.action, root.actions
            ))
        }
    };

    let pot_chips = (spot.pot_bb * state.cfg.solve.chip_per_bb as f64) as f32;
    let g = match grade(&root, hand_idx, action_idx, pot_chips, &state.cfg.trainer) {
        Ok(g) => g,
        Err(e) => return bad_request(format!("grading failed: {e}")),
    };

    let n = root.hands.len();
    let distribution: Vec<_> = root
        .actions
        .iter()
        .enumerate()
        .map(|(a, label)| {
            json!({
                "action": label,
                "frequency": root.strategy[hand_idx + a * n],
                "ev": root.ev_per_action[hand_idx + a * n],
            })
        })
        .collect();

    axum::Json(json!({
        "key": stored_key,
        "depth": req.depth,
        "hand": hand,
        "chosen_action": root.actions[action_idx],
        "mapped_from": mapped_from,
        "frequency": g.frequency,
        "ev_chosen": g.ev_chosen,
        "ev_best": g.ev_best,
        "ev_loss_chips": g.ev_loss_chips,
        "ev_loss_pct_pot": g.ev_loss_pct_pot,
        "best_action": root.actions[g.best_action_idx],
        "verdict": g.verdict,
        "pot_chips": pot_chips,
        "distribution": distribution,
    }))
    .into_response()
}
