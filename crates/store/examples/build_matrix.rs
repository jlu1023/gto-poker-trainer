//! Build the curated benchmark matrix.
//!
//! Loads the curated spot list (`benchmark/spots.toml`), solves each spot with its pot type's
//! bet tree (config `phase1.toml` via `tree_for`; geometry + ranges from gto-preflop
//! `spot_inputs`), asserts the internal invariants, stores each solved tree to `matrix/`, and
//! runs the rs_poker equity cross-check once per pot type. The curated stored set IS the
//! benchmark.
//!
//! Run from the repo root:  cargo run --release -p gto-store --example build_matrix [filter]
//! The optional arg keeps only spots whose key contains the substring (e.g. one exact key
//! for a cost pre-flight, or "3bp-v1" for one pot type) - already-stored spots outside the
//! filter are never re-solved.

use anyhow::{bail, Context, Result};
use gto_config::{Config, PotType, SolveConfig};
use gto_engine::{solve_spot, Board, RakeSpec, Spot, SpotSolution, Street};
use gto_preflop::PreflopCharts;
use gto_store::{spot_key, store_solution};
use serde::Deserialize;
use std::collections::BTreeSet;

const EQUITY_TOLERANCE: f32 = 0.02; // 2 percentage points

#[derive(Debug, Deserialize)]
struct SpotsFile {
    spot: Vec<SpotRow>,
}

fn default_pot_type() -> PotType {
    PotType::Srp
}

#[derive(Debug, Deserialize)]
struct SpotRow {
    board: String,
    stack_depth_bb: u32,
    #[serde(default = "default_pot_type")]
    pot_type: PotType,
}

struct Summary {
    key: String,
    exploit_pct: f32,
    iterations: u32,
    invariants_ok: bool,
    shrunk: bool,
}

fn main() -> Result<()> {
    let cfg = Config::load("phase1.toml").context("loading phase1.toml")?;
    let charts = PreflopCharts::load_embedded()?;
    let spots_text = std::fs::read_to_string("benchmark/spots.toml")
        .context("reading benchmark/spots.toml")?;
    let mut spots: SpotsFile =
        toml::from_str(&spots_text).context("parsing benchmark/spots.toml")?;

    // optional substring filter on the would-be spot key (pre-flights, single-pot rebuilds)
    let filter = std::env::args().nth(1).unwrap_or_default();
    if !filter.is_empty() {
        let cfg_ref = &cfg;
        spots.spot.retain(|row| {
            let tree_id = match cfg_ref.solve.tree_for(row.pot_type) {
                Ok(t) => t.bet_tree_id.clone(),
                Err(_) => return false,
            };
            format!("{}bb_{}_flop_{}", row.stack_depth_bb, row.board, tree_id).contains(&filter)
        });
        println!("filter '{}' -> {} spot(s)", filter, spots.spot.len());
        if spots.spot.is_empty() {
            bail!("filter '{}' matches no benchmark spots", filter);
        }
    }

    let trees: Vec<String> = spots
        .spot
        .iter()
        .map(|r| r.pot_type)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|p| Ok(format!("{}={}", p.label(), cfg.solve.tree_for(p)?.bet_tree_id)))
        .collect::<Result<_>>()?;
    println!(
        "Building curated matrix: {} spots, trees [{}]",
        spots.spot.len(),
        trees.join(", ")
    );

    let mut summaries = Vec::new();
    let mut cross_checked: BTreeSet<PotType> = BTreeSet::new();

    for row in &spots.spot {
        let spot = build_spot(row, &charts)?;
        // the row's effective config: the active bet tree is the pot type's tree, so the
        // engine, the store key, and the shrink fallback all see one consistent tree
        let mut row_cfg = cfg.clone();
        row_cfg.solve.bet_tree = cfg.solve.tree_for(row.pot_type)?.clone();
        let base_key = spot_key(&spot, &row_cfg.solve.bet_tree);
        println!("\n[{}] solving {} ({})", summaries.len() + 1, base_key, row.pot_type.label());

        // solve, with a RAM-ceiling shrink fallback (abort condition handling)
        let (mut solution, used_cfg, shrunk) = solve_with_shrink(&spot, &row_cfg)?;

        // internal invariants (&mut: the game-layer checks navigate the tree)
        let report = gto_validate::check_invariants(&mut solution, &used_cfg);
        if !report.passed() {
            bail!("invariants failed for {}: {:?}", base_key, report.notes);
        }

        // store to the matrix
        let path = store_solution(&solution, &used_cfg)?;
        let key = spot_key(&solution.spot, &used_cfg.solve.bet_tree);
        println!(
            "  exploit {:.4}% pot in {} iters; invariants ok; stored -> {}",
            solution.exploitability_pct_pot * 100.0,
            solution.iterations_run,
            path.display()
        );

        // equity cross-check on the first spot of each pot type (the check is range- and
        // pot-generic, but each pot type brings new authored ranges worth one cross-check)
        if cross_checked.insert(row.pot_type) {
            run_cross_check(&solution)?;
        }

        summaries.push(Summary {
            key,
            exploit_pct: solution.exploitability_pct_pot * 100.0,
            iterations: solution.iterations_run,
            invariants_ok: report.passed(),
            shrunk,
        });
    }

    print_summary(&summaries);
    println!("\nMATRIX OK: {} spots solved, invariants passed, stored.", summaries.len());
    Ok(())
}

fn build_spot(row: &SpotRow, charts: &PreflopCharts) -> Result<Spot> {
    let flop = parse_flop(&row.board)?;
    // geometry + ranges from the single source (no pot/stack literals here)
    let inputs = charts.spot_inputs(row.pot_type, row.stack_depth_bb)?;
    Ok(Spot {
        stack_depth_bb: row.stack_depth_bb,
        initial_street: Street::Flop,
        board: Board { flop, turn: None, river: None },
        pot_bb: inputs.pot_bb,
        effective_stack_bb: inputs.eff_stack_bb,
        oop_range: inputs.oop_range,
        ip_range: inputs.ip_range,
        rake: RakeSpec::default(),
    })
}

/// Solve with the batch config; on a RAM-ceiling breach, shrink the flop tree to a single size,
/// retry, and record the tradeoff. Returns (solution, config actually used, shrunk?).
fn solve_with_shrink(spot: &Spot, cfg: &Config) -> Result<(SpotSolution, Config, bool)> {
    match solve_spot(spot, &cfg.solve) {
        Ok(s) => Ok((s, cfg.clone(), false)),
        Err(e) if e.to_string().contains("ceiling") => {
            println!("  RAM ceiling hit ({e}); shrinking flop tree to a single size and retrying");
            let mut shrunk = cfg.clone();
            shrunk.solve = shrink_solve_config(&cfg.solve);
            let s = solve_spot(spot, &shrunk.solve)?;
            Ok((s, shrunk, true))
        }
        Err(e) => Err(e),
    }
}

fn shrink_solve_config(cfg: &SolveConfig) -> SolveConfig {
    let mut c = cfg.clone();
    c.bet_tree.flop_bet_sizes = ["33%".to_string(), "33%".to_string()];
    c.bet_tree.bet_tree_id = format!("{}-shrunk", c.bet_tree.bet_tree_id);
    c
}

fn run_cross_check(solution: &SpotSolution) -> Result<()> {
    let check = gto_validate::cross_check_equity(solution, 0)?;
    println!(
        "  equity cross-check ({}): solver {:.4} vs rs_poker {:.4} (|diff| {:.4}, {} villain combos)",
        check.hero_hand,
        check.solver_equity,
        check.enumerated_equity,
        check.abs_diff,
        check.villain_combos
    );
    if !check.agrees(EQUITY_TOLERANCE) {
        bail!(
            "equity cross-check failed: |{:.4} - {:.4}| = {:.4} > {:.4}",
            check.solver_equity,
            check.enumerated_equity,
            check.abs_diff,
            EQUITY_TOLERANCE
        );
    }
    Ok(())
}

fn parse_flop(board: &str) -> Result<[String; 3]> {
    if board.len() != 6 {
        bail!("board '{board}' must be 6 chars (3 cards)");
    }
    Ok([
        board[0..2].to_string(),
        board[2..4].to_string(),
        board[4..6].to_string(),
    ])
}

fn print_summary(summaries: &[Summary]) {
    println!("\n{:<34} {:>10} {:>6} {:>5} {:>7}", "spot key", "exploit%", "iters", "inv", "shrunk");
    for s in summaries {
        println!(
            "{:<34} {:>9.4} {:>6} {:>5} {:>7}",
            s.key,
            s.exploit_pct,
            s.iterations,
            if s.invariants_ok { "ok" } else { "FAIL" },
            if s.shrunk { "yes" } else { "no" }
        );
    }
}
