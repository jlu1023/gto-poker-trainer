//! The read-only viz data contract: the JSON shape the Phase 2 front-end consumes.
//!
//! One source of truth for both producers:
//!   - the `export_viz` example (static export of the whole matrix), and
//!   - the Phase 3 solve server (per-solve response).
//! The exporter/server reshapes the solver's `strategy[hand + action * n]` layout into clean
//! per-hand arrays so the front-end never needs to know the solver's memory layout.

use gto_engine::{read_root_range_weights, SpotSolution, Street};
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

/// Build the viz contract for a solved spot (root node only at v1). Takes `&mut` because
/// the root range weights are read off the game (F5-correct `read_node_weights`, which
/// returns to root after reading).
pub fn build_contract(key: &str, solution: &mut SpotSolution) -> SpotContract {
    // authored range mass per combo (0..1; raw root weights - board-blocked combos are
    // already excluded from the hands list). If the read fails or the shape is unexpected,
    // fall back to 1.0s (the pre-weight behavior) rather than guess.
    let weights = read_root_range_weights(&mut solution.game)
        .ok()
        .filter(|w| w.len() == solution.root.hands.len())
        .unwrap_or_else(|| vec![1.0; solution.root.hands.len()]);

    let root = &solution.root;
    let n = root.hands.len();
    let num_actions = root.actions.len();

    let has_epa = root.ev_per_action.len() == n * num_actions;
    let hands = (0..n)
        .map(|i| HandEntry {
            hand: root.hands[i].clone(),
            strategy: (0..num_actions).map(|a| root.strategy[i + a * n]).collect(),
            ev: root.ev.get(i).copied().unwrap_or(0.0),
            equity: root.equity.get(i).copied().unwrap_or(0.0),
            ev_actions: if has_epa {
                (0..num_actions).map(|a| root.ev_per_action[i + a * n]).collect()
            } else {
                Vec::new()
            },
            weight: weights[i],
        })
        .collect();

    let spot = &solution.spot;
    let mut board: Vec<String> = spot.board.flop.to_vec();
    if let Some(t) = &spot.board.turn {
        board.push(t.clone());
    }
    if let Some(r) = &spot.board.river {
        board.push(r.clone());
    }

    let oop = root.player == 0;
    SpotContract {
        key: key.to_string(),
        board,
        street: street_label(spot.initial_street).to_string(),
        stack_depth_bb: spot.stack_depth_bb,
        pot_bb: spot.pot_bb,
        eff_stack_bb: spot.effective_stack_bb,
        exploitability_pct_pot: solution.exploitability_pct_pot,
        iterations: solution.iterations_run,
        hero: HeroSeat {
            player: root.player,
            position: if oop { "OOP" } else { "IP" },
            seat: if oop { "BB" } else { "BTN" },
        },
        villain: Seat {
            position: if oop { "IP" } else { "OOP" },
            seat: if oop { "BTN" } else { "BB" },
        },
        actions: root.actions.clone(),
        hands,
    }
}

fn street_label(street: Street) -> &'static str {
    match street {
        Street::Flop => "flop",
        Street::Turn => "turn",
        Street::River => "river",
    }
}
