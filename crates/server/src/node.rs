//! POST /api/node: read a NON-ROOT node of an already-cached spot, for the explore-mode
//! game-tree navigator. Read-only and it NEVER solves: if the spot is not in the matrix it
//! returns 409, so the navigator can only ever walk a tree that already exists on disk.
//!
//! Flow: `build_spot` (shared with solve/setup/grade, so keys never drift) -> cache check
//! (primary key then the `-shrunk` variant; 409 on miss) -> load the cached game (no solve) ->
//! `read_node_labeled` for the action-label path, F5-correct per-action EV -> reshape into a
//! `NodeContract`. Every displayed node carries per-action EV (the empty-EPA tripwire rejects
//! any node that does not), so the frontend never renders a node that is not F5-checkable.
//!
//! Two gates, deliberately distinct (D55):
//! - HARD F5 gate (dot identity + EPA present): a displayed node that fails it is a regression,
//!   so this 500s rather than rendering it.
//! - SOFT display gate (dominance vs `[navigator] display_gate_pct_pot`): a child whose own
//!   local quality is worse than the gate is simply not descendable, and the UI says why. The
//!   same dominance value ships per node as the confidence signal, which is what makes the 5%
//!   gate honest rather than misleading.

use axum::extract::{Json, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use gto_config::PotType;
use gto_engine::{child_kinds_labeled, read_node_labeled, read_node_weights_labeled, ChildKind};
use gto_store::viz::{build_node_contract, ActionMeta, GateContract, NodeContract, NodeExtras};
use gto_store::{load_manifest, load_solution};
use gto_validate::{ev_dot_consistency, node_dominance};
use serde::Deserialize;
use serde_json::json;
use std::path::Path;

use crate::solve::{build_spot, default_depth, default_pot_type};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct NodeRequest {
    /// Three card strings, any order/case, e.g. ["Ks","7h","2c"].
    pub board: Vec<String>,
    #[serde(default = "default_depth")]
    pub depth: u32,
    #[serde(default = "default_pot_type")]
    pub pot_type: PotType,
    /// Action labels from the root down to the target node (exactly as received in a prior
    /// node's `actions`), e.g. ["Check","Bet(33)","Raise(99)"]. Empty = the root itself.
    #[serde(default)]
    pub path: Vec<String>,
}

/// A path walk / load failure, split so a bad client path is 400 and an internal problem
/// (corrupt cache, per-action-EV regression) is 500.
enum NodeError {
    BadPath(String),
    Internal(String),
}

pub async fn node_handler(State(state): State<AppState>, Json(req): Json<NodeRequest>) -> Response {
    let (_spot, key, effective_cfg) = match build_spot(&state, &req.board, req.depth, req.pot_type) {
        Ok(v) => v,
        Err(e) => return bad_request(e),
    };

    // the navigator never solves: the spot must already be cached (primary or shrunk key)
    let dir = Path::new(&state.cfg.io.matrix_dir);
    let manifest = match load_manifest(dir) {
        Ok(m) => m,
        Err(e) => return internal_error(format!("reading matrix manifest: {e}")),
    };
    let hit_key = if manifest.spots.contains_key(&key) {
        key.clone()
    } else if manifest.spots.contains_key(&format!("{key}-shrunk")) {
        format!("{key}-shrunk")
    } else {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": format!("spot {key} not cached; the navigator never solves") })),
        )
            .into_response();
    };

    // load + walk on a blocking thread (tree decode + reads are CPU/IO, not async work)
    let cfg = effective_cfg;
    let path = req.path;
    let result = tokio::task::spawn_blocking(move || -> Result<NodeContract, NodeError> {
        let mut solution = load_solution(&hit_key, &cfg)
            .map_err(|e| NodeError::Internal(format!("loading cached solve: {e}")))?;
        let node = read_node_labeled(&mut solution.game, &path)
            .map_err(|e| NodeError::BadPath(format!("{e}")))?;
        // every displayed node must be F5-checkable: a decision node without per-action EV is a
        // read_node regression, not a node we quietly render (mirrors review.rs's tripwire)
        if node.ev_per_action.is_empty() {
            return Err(NodeError::Internal(format!(
                "read_node regression: node {path:?} lacks per-action EV"
            )));
        }
        // HARD F5 gate on the displayed node: the dot identity must hold (D55). It is not a
        // dominance check - dominance is the display gate, computed below.
        let (dot_ok, dot_notes) = ev_dot_consistency(&node);
        if !dot_ok {
            return Err(NodeError::Internal(format!(
                "F5 dot-identity failed at node {path:?}: {dot_notes:?}"
            )));
        }
        let weights = read_node_weights_labeled(&mut solution.game, &path)
            .map_err(|e| NodeError::BadPath(format!("{e}")))?;

        let pot_chips = (solution.spot.pot_bb * cfg.solve.chip_per_bb as f64) as f32;
        let to_pct = |dom_chips: f32| dom_chips / pot_chips * 100.0;
        let node_dom_pct = to_pct(node_dominance(&node)); // this node's confidence signal

        // the reach-gate + tiers come from config (D55) and ship in the contract, so the UI
        // reads one number rather than duplicating it client-side
        let gate = GateContract {
            display_gate_pct_pot: cfg.navigator.display_gate_pct_pot,
            confidence_high_pct_pot: cfg.navigator.confidence_high_pct_pot,
            confidence_fair_pct_pot: cfg.navigator.confidence_fair_pct_pot,
        };

        // per-action child metadata + the reach-gate: a decision child is descendable only when
        // its own dominance is within the display gate (the solve is locally trained enough)
        let kinds = child_kinds_labeled(&mut solution.game, &path)
            .map_err(|e| NodeError::BadPath(format!("{e}")))?;
        let mut actions_meta = Vec::with_capacity(node.actions.len());
        for (i, label) in node.actions.iter().enumerate() {
            let kind = kinds.get(i).copied().unwrap_or(ChildKind::Decision);
            let (child_dominance_pct, descendable) = match kind {
                ChildKind::Decision => {
                    let mut child_path = path.clone();
                    child_path.push(label.clone());
                    match read_node_labeled(&mut solution.game, &child_path) {
                        Ok(child)
                            if !child.ev_per_action.is_empty()
                                && ev_dot_consistency(&child).0 =>
                        {
                            let dpct = to_pct(node_dominance(&child));
                            (Some(dpct), dpct <= gate.display_gate_pct_pot)
                        }
                        // unreadable child / no EPA / dot-identity fails -> boundary, not descendable
                        _ => (None, false),
                    }
                }
                ChildKind::Chance | ChildKind::Terminal => (None, false),
            };
            actions_meta.push(ActionMeta {
                label: label.clone(),
                child: kind.label().to_string(),
                child_dominance_pct,
                descendable,
            });
        }

        Ok(build_node_contract(
            &hit_key,
            &solution,
            &node,
            &weights,
            NodeExtras { actions_meta, path, dominance_pct: node_dom_pct, gate },
        ))
    })
    .await;

    match result {
        Err(join) => internal_error(format!("node task failed: {join}")),
        Ok(Err(NodeError::BadPath(m))) => bad_request(m),
        Ok(Err(NodeError::Internal(m))) => internal_error(m),
        Ok(Ok(contract)) => (StatusCode::OK, Json(json!(contract))).into_response(),
    }
}

fn bad_request(msg: String) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": msg }))).into_response()
}

fn internal_error(msg: String) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": msg }))).into_response()
}
