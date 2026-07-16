//! The read-only viz data contract: the JSON shape the Phase 2 front-end consumes.
//!
//! One source of truth for both producers:
//!   - the `export_viz` example (static export of the whole matrix), and
//!   - the Phase 3 solve server (per-solve response).
//! The exporter/server reshapes the solver's `strategy[hand + action * n]` layout into clean
//! per-hand arrays so the front-end never needs to know the solver's memory layout.

use gto_engine::{read_root_range_weights, NodeStrategy, SpotSolution, Street};
use serde::Serialize;

#[derive(Serialize)]
pub struct Seat {
    pub position: &'static str,
    pub seat: &'static str,
}

#[derive(Serialize)]
pub struct HeroSeat {
    pub player: usize,
    pub position: &'static str,
    pub seat: &'static str,
}

#[derive(Serialize)]
pub struct HandEntry {
    pub hand: String,
    pub strategy: Vec<f32>,
    pub ev: f32,
    pub equity: f32,
    /// Per-action EV (chips), aligned with `actions`. Empty when the loaded root predates
    /// per-action EV (old summary sidecars); always filled on fresh reads.
    pub ev_actions: Vec<f32>,
    /// AUTHORED range mass for this combo (0..1, the raw root weight; board-blocked combos
    /// are excluded from the list entirely, so card removal lives in membership, not here).
    /// 1.0 for full-weight combos; fractional for hands the authored range plays at partial
    /// frequency (e.g. 3-bet-mix premiums). The grid's class aggregation weights by this,
    /// with weight-1.0 as the exact degenerate case.
    pub weight: f32,
}

/// One spot, exactly as the front-end consumes it (`viz/src/types.ts` `SpotContract`).
#[derive(Serialize)]
pub struct SpotContract {
    pub key: String,
    pub board: Vec<String>,
    pub street: String,
    pub stack_depth_bb: u32,
    pub pot_bb: f64,
    pub eff_stack_bb: f64,
    pub exploitability_pct_pot: f32,
    pub iterations: u32,
    pub hero: HeroSeat,
    pub villain: Seat,
    pub actions: Vec<String>,
    pub hands: Vec<HandEntry>,
}

/// Reshape a node's solver-layout `strategy[hand + action * n]` into clean per-hand entries
/// (the shape the grid consumes). `weights` is the per-hand mass aligned with `node.hands`
/// (authored range mass at the root; normalized reach mass at a deeper node - either is a
/// valid positive weighting for the grid's class aggregation, whose scale cancels).
fn reshape_hands(node: &NodeStrategy, weights: &[f32]) -> Vec<HandEntry> {
    let n = node.hands.len();
    let num_actions = node.actions.len();
    let has_epa = node.ev_per_action.len() == n * num_actions;
    (0..n)
        .map(|i| HandEntry {
            hand: node.hands[i].clone(),
            strategy: (0..num_actions).map(|a| node.strategy[i + a * n]).collect(),
            ev: node.ev.get(i).copied().unwrap_or(0.0),
            equity: node.equity.get(i).copied().unwrap_or(0.0),
            ev_actions: if has_epa {
                (0..num_actions).map(|a| node.ev_per_action[i + a * n]).collect()
            } else {
                Vec::new()
            },
            weight: weights.get(i).copied().unwrap_or(1.0),
        })
        .collect()
}

/// Hero (the acting player) and villain seat labels for the player to act at a node. Because
/// hero is always the actor, the same reshaping serves the root and any deeper node: the header
/// and table derive "whose turn" from `hero` with no separate flag.
fn seats_for(player: usize) -> (HeroSeat, Seat) {
    let oop = player == 0;
    (
        HeroSeat {
            player,
            position: if oop { "OOP" } else { "IP" },
            seat: if oop { "BB" } else { "BTN" },
        },
        Seat {
            position: if oop { "IP" } else { "OOP" },
            seat: if oop { "BTN" } else { "BB" },
        },
    )
}

/// Board card strings (flop plus any dealt turn/river) for a solved spot.
fn board_strings(solution: &SpotSolution) -> Vec<String> {
    let spot = &solution.spot;
    let mut board: Vec<String> = spot.board.flop.to_vec();
    if let Some(t) = &spot.board.turn {
        board.push(t.clone());
    }
    if let Some(r) = &spot.board.river {
        board.push(r.clone());
    }
    board
}

/// Build the viz contract for a solved spot (root node). Takes `&mut` because the root range
/// weights are read off the game (F5-correct `read_root_range_weights`, which returns to root).
pub fn build_contract(key: &str, solution: &mut SpotSolution) -> SpotContract {
    // authored range mass per combo (0..1; raw root weights - board-blocked combos are already
    // excluded from the hands list). If the read fails or the shape is unexpected, fall back to
    // 1.0s (the pre-weight behavior) rather than guess.
    let weights = read_root_range_weights(&mut solution.game)
        .ok()
        .filter(|w| w.len() == solution.root.hands.len())
        .unwrap_or_else(|| vec![1.0; solution.root.hands.len()]);

    let hands = reshape_hands(&solution.root, &weights);
    let (hero, villain) = seats_for(solution.root.player);
    let board = board_strings(solution);
    SpotContract {
        key: key.to_string(),
        board,
        street: street_label(solution.spot.initial_street).to_string(),
        stack_depth_bb: solution.spot.stack_depth_bb,
        pot_bb: solution.spot.pot_bb,
        eff_stack_bb: solution.spot.effective_stack_bb,
        exploitability_pct_pot: solution.exploitability_pct_pot,
        iterations: solution.iterations_run,
        hero,
        villain,
        actions: solution.root.actions.clone(),
        hands,
    }
}

/// What descending an action leads to, for the navigator's descend controls. The server fills
/// `child_dominance_pct` / `descendable` from the reach-gate (D55): a decision child is
/// descendable only when the solve is locally trained enough to trust it.
#[derive(Serialize)]
pub struct ActionMeta {
    pub label: String,
    /// "decision" (a further player node) | "chance" (the turn - a leaf here) | "terminal".
    pub child: String,
    /// For a decision child: its own worst dominance as a % of the starting pot (the reach-gate
    /// signal). None for chance/terminal children.
    pub child_dominance_pct: Option<f32>,
    /// True when descending is allowed: a decision child within the display gate. False for
    /// chance/terminal leaves, and for decision children the solve is not locally trained
    /// enough to trust (a self-explaining boundary in the UI).
    pub descendable: bool,
}

/// One NON-ROOT node for the game-tree navigator: a superset of `SpotContract` (so the grid,
/// legend, detail and table render it UNCHANGED - `hero` is the node's actor, so the header
/// derives "whose turn" with no new field) plus the navigator extras - the per-action child
/// metadata (descend buttons + the flop-only "-> turn" leaf + the reach-gate boundary), the
/// path walked so far (breadcrumb), and this node's own confidence signal.
#[derive(Serialize)]
pub struct NodeContract {
    #[serde(flatten)]
    pub spot: SpotContract,
    pub actions_meta: Vec<ActionMeta>,
    pub path: Vec<String>,
    /// This node's own worst unplayed-action dominance as a % of the starting pot: the per-node
    /// CONFIDENCE signal (near the ~0.1% root quality = trustworthy; higher = marginal). The
    /// display gate admits nodes at or below the configured % (D55).
    pub dominance_pct: f32,
    /// The gate + tier thresholds this node was judged against, shipped so the UI single-sources
    /// them from config instead of duplicating the numbers client-side (where they would drift).
    pub gate: GateContract,
}

/// The navigator's reach-gate and confidence tiers, as % of the starting pot (D55). Mirrors
/// `gto_config::NavigatorConfig` onto the wire.
#[derive(Serialize, Clone, Copy)]
pub struct GateContract {
    /// Dominance at or below this -> descendable.
    pub display_gate_pct_pot: f32,
    /// Dominance at or below this -> "high" confidence.
    pub confidence_high_pct_pot: f32,
    /// Dominance at or below this (and above high) -> "fair"; above it -> "marginal".
    pub confidence_fair_pct_pot: f32,
}

/// The navigator-specific parts of a node contract: everything the `/api/node` handler has to
/// compute (per-action child metadata, the walked path, this node's confidence signal and the
/// gate it was judged against) as opposed to the parts derived from the node itself. Grouped so
/// the reach-gate values travel together and cannot be passed in the wrong order.
pub struct NodeExtras {
    pub actions_meta: Vec<ActionMeta>,
    pub path: Vec<String>,
    pub dominance_pct: f32,
    pub gate: GateContract,
}

/// Build a node contract from an ALREADY-READ non-root node. The caller (the `/api/node`
/// handler) does the F5-correct reads, the empty-EPA tripwire, and computes the reach-gate /
/// confidence values (`NodeExtras`) via `gto_validate::node_dominance`; this only reshapes,
/// reusing the SAME `reshape_hands` / `seats_for` as the root contract so the grid cannot tell
/// a node from the root.
pub fn build_node_contract(
    key: &str,
    solution: &SpotSolution,
    node: &NodeStrategy,
    weights: &[f32],
    extras: NodeExtras,
) -> NodeContract {
    let hands = reshape_hands(node, weights);
    let (hero, villain) = seats_for(node.player);
    let spot = SpotContract {
        key: key.to_string(),
        board: board_strings(solution),
        street: street_label(solution.spot.initial_street).to_string(),
        stack_depth_bb: solution.spot.stack_depth_bb,
        pot_bb: solution.spot.pot_bb,
        eff_stack_bb: solution.spot.effective_stack_bb,
        exploitability_pct_pot: solution.exploitability_pct_pot,
        iterations: solution.iterations_run,
        hero,
        villain,
        actions: node.actions.clone(),
        hands,
    };
    NodeContract {
        spot,
        actions_meta: extras.actions_meta,
        path: extras.path,
        dominance_pct: extras.dominance_pct,
        gate: extras.gate,
    }
}

fn street_label(street: Street) -> &'static str {
    match street {
        Street::Flop => "flop",
        Street::Turn => "turn",
        Street::River => "river",
    }
}
