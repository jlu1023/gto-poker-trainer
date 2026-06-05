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
use gto_config::Config;
use gto_engine::{query_node, read_node_weights, NodeQuery, NodeStrategy, SolverAction, Step};
use gto_history::aggregate::texture_bucket;
use gto_store::{load_manifest, load_solution};
use gto_validate::advisory::{assess_cbet, assess_mdf, CbetAssessment, MdfAssessment};
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

fn print_mdf(context: &str, a: &MdfAssessment) {
    println!(
        "    {context}: bet {:.0} into {:.0}  MDF floor {:.1}%  actual defence {:.1}%  gap {:+.1} pts  {}",
        a.bet_chips,
        a.pot_chips,
        a.mdf_floor * 100.0,
        a.actual_defence * 100.0,
        a.gap * 100.0,
        if a.flagged { "FLAG (worth a look - likely a range signal, not solver error)" } else { "ok" }
    );
}

fn print_cbet(a: &CbetAssessment) {
    println!(
        "    c-bet: {:.1}% on {} ({})  {}",
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
    let tree_suffix = format!("_{}", cfg.solve.bet_tree.bet_tree_id);
    let manifest = load_manifest(Path::new(&cfg.io.matrix_dir))?;
    let mut keys: Vec<&String> = manifest
        .spots
        .keys()
        .filter(|k| k.ends_with(&tree_suffix))
        .collect();
    keys.sort();
    println!(
        "{} stored {} spot(s); cached reads only, nothing is solved\n",
        keys.len(),
        cfg.solve.bet_tree.bet_tree_id
    );

    let mut total_flags = 0usize;

    for key in keys {
        let mut solution = load_solution(key, &cfg)?;
        let pot_chips = (solution.spot.pot_bb * cfg.solve.chip_per_bb as f64) as f32;
        let board = solution.spot.board.flop.clone();
        let texture = texture_bucket(&board);
        println!("=== {key} ===  board {} {} {}  pot {:.0} chips  texture {texture}",
            board[0], board[1], board[2], pot_chips);

        // ---- Check B: c-bet frequency (IP's bet mix after the BB checks) ----
        let cbet_query = NodeQuery { path: vec![Step::Action(SolverAction::Check)] };
        let cbet_node = query_node(&mut solution, &cbet_query)?;
        let cbet_weights = read_node_weights(&mut solution.game, &cbet_query)?;
        let mut cbet_freq = 0.0f32;
        for (idx, _, _) in bet_actions(&cbet_node) {
            cbet_freq += weighted_freq(&cbet_node, &cbet_weights, idx)?;
        }
        let cbet = assess_cbet(texture, cbet_freq);
        print_cbet(&cbet);
        total_flags += cbet.flagged as usize;

        // ---- Check A: MDF at every flop opening-bet-facing node ----
        // BB defending the c-bet: path [Check, Bet(s)]
        for (_, chips, act) in bet_actions(&cbet_node) {
            let q = NodeQuery {
                path: vec![Step::Action(SolverAction::Check), Step::Action(act)],
            };
            let node = query_node(&mut solution, &q)?;
            let weights = read_node_weights(&mut solution.game, &q)?;
            let fold_idx = action_index(&node, "Fold")
                .ok_or_else(|| anyhow!("no Fold action at the bet-facing node"))?;
            let defence = 1.0 - weighted_freq(&node, &weights, fold_idx)?;
            let a = assess_mdf(pot_chips, chips, defence, true);
            print_mdf(&format!("BB vs c-bet Bet({chips:.0})"), &a);
            total_flags += a.flagged as usize;
        }
        // IP defending the BB lead: path [Bet(s)] from the root
        for (_, chips, act) in bet_actions(&solution.root) {
            let q = NodeQuery { path: vec![Step::Action(act)] };
            let node = query_node(&mut solution, &q)?;
            let weights = read_node_weights(&mut solution.game, &q)?;
            let fold_idx = action_index(&node, "Fold")
                .ok_or_else(|| anyhow!("no Fold action at the lead-facing node"))?;
            let defence = 1.0 - weighted_freq(&node, &weights, fold_idx)?;
            let a = assess_mdf(pot_chips, chips, defence, false);
            print_mdf(&format!("IP vs BB lead Bet({chips:.0})"), &a);
            total_flags += a.flagged as usize;
        }
        println!();
    }

    // ---- synthetic demo: prove the flags render while the report stays advisory ----
    println!("--- SYNTHETIC demo (fabricated numbers, proves flagging works; not real spots) ---");
    let demo_mdf = assess_mdf(100.0, 33.0, 0.50, true);
    print_mdf("SYNTHETIC 50% defence vs 33% pot bet", &demo_mdf);
    let demo_cbet = assess_cbet("rainbow-broadway", 0.10);
    print_cbet(&demo_cbet);
    println!(
        "\n{} flag(s) raised across the stored spots (synthetic demo excluded).\n\
         ADVISORY: this report is informational and ALWAYS exits 0.",
        total_flags
    );
    Ok(())
}
