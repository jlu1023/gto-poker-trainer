//! The read-only viz data contract: the JSON shape the Phase 2 front-end consumes.
//!
//! One source of truth for both producers:
//!   - the `export_viz` example (static export of the whole matrix), and
//!   - the Phase 3 solve server (per-solve response).
//! The exporter/server reshapes the solver's `strategy[hand + action * n]` layout into clean
//! per-hand arrays so the front-end never needs to know the solver's memory layout.

use gto_engine::{SpotSolution, Street};
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

/// Build the viz contract for a solved spot (root node only at v1).
pub fn build_contract(key: &str, solution: &SpotSolution) -> SpotContract {
    let root = &solution.root;
    let n = root.hands.len();
    let num_actions = root.actions.len();

    let hands = (0..n)
        .map(|i| HandEntry {
            hand: root.hands[i].clone(),
            strategy: (0..num_actions).map(|a| root.strategy[i + a * n]).collect(),
            ev: root.ev.get(i).copied().unwrap_or(0.0),
            equity: root.equity.get(i).copied().unwrap_or(0.0),
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
