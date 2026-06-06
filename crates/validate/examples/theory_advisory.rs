//! ADVISORY theory-plausibility report: MDF floors and c-bet-by-texture over the stored
//! SRP spots. NOT a gate - this report is purely informational, never panics, never fails:
//! it ALWAYS exits 0, flags included. It is not wired into `check_invariants` or any phase
//! verifier, and it must stay that way (hard-gating heuristics false-fires on legitimate
//! spots until the gate gets ignored).
//!
//! Because the engine is independently verified (six-check invariant gate, commit c7700d8),
//! a flag here is a signal about the INPUTS - most plausibly the authored preflop ranges,
//! the one part of the system not independently verified - or a legitimately atypical spot.
//!
//! Reads cached solves only (load_solution); never solves anything. All node reads go
//! through the F5-correct `read_node`/`query_node` path; reach weights through
//! `read_node_weights`. Scope: flop opening bets only (root lead + c-bet after check);
//! raise-facing MDF is out of scope here.
//!
//! Run from the repo root:  cargo run --release -p gto-validate --example theory_advisory

use anyhow::{anyhow, Context, Result};
use gto_config::{Config, PotType};
use gto_engine::{query_node, read_node_weights, NodeQuery, NodeStrategy, SolverAction, Step};
use gto_history::aggregate::texture_bucket;
use gto_store::{load_manifest, load_solution};
use gto_validate::advisory::{
    assess_cbet, assess_cbet_pot, assess_mdf, assess_mdf_pot, calibration_for, CbetAssessment,
    MdfAssessment,
};
use std::path::Path;

/// Range-weighted frequency of one action at a node: sum_i w_i p_i(action) / sum_i w_i.
fn weighted_freq(node: &NodeStrategy, weights: &[f32], action_idx: usize) -> Result<f32> {
    let n = node.hands.len();
    if weights.len() != n {
        return Err(anyhow!("weights len {} != hands {}", weights.len(), n));
    }
    let mut num = 0.0f64;
    let mut den = 0.0f64;
    for h in 0..n {
        let w = weights[h] as f64;
        num += w * node.strategy[h + action_idx * n] as f64;
        den += w;
    }
    if den <= 0.0 {
        return Err(anyhow!("zero total weight at node"));
    }
    Ok((num / den) as f32)
}

/// The bet actions at a node, as (action index, chips, solver action for the walk step).
fn bet_actions(node: &NodeStrategy) -> Vec<(usize, f32, SolverAction)> {
    node.actions
        .iter()
        .enumerate()
        .filter_map(|(i, label)| {
            let n: i32 = label.strip_prefix("Bet(")?.strip_suffix(')')?.parse().ok()?;
            Some((i, n as f32, SolverAction::Bet(n)))
        })
        .collect()
}

fn action_index(node: &NodeStrategy, label: &str) -> Option<usize> {
    node.actions.iter().position(|a| a == label)
}

fn print_mdf(context: &str, calibrated: bool, a: &MdfAssessment) {
    let verdict = if a.flagged {
        "FLAG (worth a look - likely a range signal, not solver error)"
    } else if calibrated {
        "ok"
    } else {
        "measured (uncalibrated)"
    };
    println!(
        "    {context}: bet {:.0} into {:.0}  MDF floor {:.1}%  actual defence {:.1}%  gap {:+.1} pts  {}",
        a.bet_chips,
        a.pot_chips,
        a.mdf_floor * 100.0,
        a.actual_defence * 100.0,
        a.gap * 100.0,
        verdict
    );
}

fn print_cbet(label: &str, a: &CbetAssessment) {
    println!(
        "    {label}: {:.1}% on {} ({})  {}",
        a.cbet_freq * 100.0,
        a.texture,
        a.expectation,
        if a.flagged { "FLAG (weak heuristic signal - eyeball it, not evidence of error)" } else { "ok" }
    );
}

fn main() -> Result<()> {
    println!(
        "ADVISORY theory-plausibility report (MDF + c-bet by texture)\n\
         informational only: flags invite a look, never fail a build; the engine itself is\n\
         verified (six-check invariant gate), so deviations point at ranges or atypical spots.\n\
         MDF caveat: pure pot-odds floor; ignores equity realization / future streets / card\n\
         removal, so modest shortfalls are expected and unflagged. c-bet bands are SOFT\n\
         heuristics, lower-confidence than MDF; only gross deviations flag.\n"
    );

    let cfg = Config::load("phase1.toml").context("loading phase1.toml")?;
    let manifest = load_manifest(Path::new(&cfg.io.matrix_dir))?;
    let mut total_flags = 0usize;

    // every configured pot family, each under its own framing. HU node geometry is fixed
    // (BB/OOP acts first in every pot), so the node PATHS are shared; what changes per pot
    // is which node carries the "c-bet" reading (the AGGRESSOR's opening node: the root
    // for the 3bp OOP 3-bettor, the after-check node for the srp/4bp IP aggressor), the
    // labels, and the calibration state.
    for pot in [PotType::Srp, PotType::ThreeBet, PotType::FourBet] {
        let Ok(tree) = cfg.solve.tree_for(pot) else {
            println!("--- {} not configured; skipped ---\n", pot.label());
            continue;
        };
        let tree_suffix = format!("_{}", tree.bet_tree_id);
        let mut keys: Vec<&String> =
            manifest.spots.keys().filter(|k| k.ends_with(&tree_suffix)).collect();
        keys.sort();
        let cal = calibration_for(pot);
        println!(
            "--- {} ({}): {} stored spot(s){} ---\n",
            pot.label(),
            tree.bet_tree_id,
            keys.len(),
            if cal.calibrated { "" } else { "  [UNCALIBRATED: measuring pass, no flags]" }
        );
        // per-pot framing labels (the aggressor flips; the defender positions do not)
        let (cbet_at_root, cbet_label, root_bet_def_label, checked_bet_def_label) = match pot {
            PotType::ThreeBet => (
                true,
                "OOP 3-bettor c-bet (root)",
                "IP caller vs c-bet",
                "OOP vs stab after check-through",
            ),
            PotType::FourBet => (
                false,
                "IP 4-bettor c-bet (after check)",
                "IP vs OOP lead",
                "OOP caller vs c-bet",
            ),
            PotType::Srp => {
                (false, "IP c-bet (after check)", "IP vs BB lead", "BB vs c-bet")
            }
        };

        for key in keys {
            let mut solution = load_solution(key, &cfg)?;
            let pot_chips = (solution.spot.pot_bb * cfg.solve.chip_per_bb as f64) as f32;
            let board = solution.spot.board.flop.clone();
            let texture = texture_bucket(&board);
            println!("=== {key} ===  board {} {} {}  pot {:.0} chips  texture {texture}",
                board[0], board[1], board[2], pot_chips);

            // ---- Check B: the aggressor's opening-bet frequency ----
            let after_check_query = NodeQuery { path: vec![Step::Action(SolverAction::Check)] };
            let cbet_freq = if cbet_at_root {
                let weights = read_node_weights(&mut solution.game, &NodeQuery { path: vec![] })?;
                let mut f = 0.0f32;
                for (idx, _, _) in bet_actions(&solution.root) {
                    f += weighted_freq(&solution.root, &weights, idx)?;
                }
                f
            } else {
                let node = query_node(&mut solution, &after_check_query)?;
                let weights = read_node_weights(&mut solution.game, &after_check_query)?;
                let mut f = 0.0f32;
                for (idx, _, _) in bet_actions(&node) {
                    f += weighted_freq(&node, &weights, idx)?;
                }
                f
            };
            let cbet = assess_cbet_pot(pot, texture, cbet_freq);
            print_cbet(cbet_label, &cbet);
            total_flags += cbet.flagged as usize;

            // ---- Check A: MDF at every flop opening-bet-facing node (paths shared
            // across pots; defender position is fixed by the node, thresholds by pot) ----
            // the OOP player facing a bet after checking: [Check, Bet(s)]
            let after_check_node = query_node(&mut solution, &after_check_query)?;
            for (_, chips, act) in bet_actions(&after_check_node) {
                let q = NodeQuery {
                    path: vec![Step::Action(SolverAction::Check), Step::Action(act)],
                };
                let node = query_node(&mut solution, &q)?;
                let weights = read_node_weights(&mut solution.game, &q)?;
                let fold_idx = action_index(&node, "Fold")
                    .ok_or_else(|| anyhow!("no Fold action at the bet-facing node"))?;
                let defence = 1.0 - weighted_freq(&node, &weights, fold_idx)?;
                let a = assess_mdf_pot(pot, pot_chips, chips, defence, true);
                print_mdf(&format!("{checked_bet_def_label} Bet({chips:.0})"), cal.calibrated, &a);
                total_flags += a.flagged as usize;
            }
            // the IP player facing a root bet: [Bet(s)]
            for (_, chips, act) in bet_actions(&solution.root) {
                let q = NodeQuery { path: vec![Step::Action(act)] };
                let node = query_node(&mut solution, &q)?;
                let weights = read_node_weights(&mut solution.game, &q)?;
                let fold_idx = action_index(&node, "Fold")
                    .ok_or_else(|| anyhow!("no Fold action at the lead-facing node"))?;
                let defence = 1.0 - weighted_freq(&node, &weights, fold_idx)?;
                let a = assess_mdf_pot(pot, pot_chips, chips, defence, false);
                print_mdf(&format!("{root_bet_def_label} Bet({chips:.0})"), cal.calibrated, &a);
                total_flags += a.flagged as usize;
            }
            println!();
        }
    }

    // ---- synthetic demo: prove the flags render while the report stays advisory ----
    println!("--- SYNTHETIC demo (fabricated numbers, proves flagging works; not real spots) ---");
    let demo_mdf = assess_mdf(100.0, 33.0, 0.50, true);
    print_mdf("SYNTHETIC 50% defence vs 33% pot bet", true, &demo_mdf);
    let demo_cbet = assess_cbet("rainbow-broadway", 0.10);
    print_cbet("c-bet", &demo_cbet);
    println!(
        "\n{} flag(s) raised across the stored spots (synthetic demo excluded).\n\
         ADVISORY: this report is informational and ALWAYS exits 0.",
        total_flags
    );
    Ok(())
}
