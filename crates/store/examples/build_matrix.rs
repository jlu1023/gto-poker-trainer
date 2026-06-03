//! Build the curated benchmark matrix (Phase 1).
//!
//! Loads the curated spot list (`benchmark/spots.toml`), solves each spot with the `srp-v1` tree
//! (config `phase1.toml`), asserts the internal invariants, stores each solved tree to `matrix/`,
//! and runs the rs_poker equity cross-check on one spot. The curated stored set IS the benchmark.
//!
//! Run from the repo root:  cargo run --release -p gto-store --example build_matrix

use anyhow::{bail, Context, Result};
use gto_config::{Config, SolveConfig};
use gto_engine::{solve_spot, Board, RakeSpec, Spot, SpotSolution, Street};
use gto_preflop::{PreflopCharts, PreflopContext, PreflopNode};
use gto_store::{spot_key, store_solution};
use serde::Deserialize;

const EQUITY_TOLERANCE: f32 = 0.02; // 2 percentage points

#[derive(Debug, Deserialize)]
struct SpotsFile {
    spot: Vec<SpotRow>,
}

#[derive(Debug, Deserialize)]
struct SpotRow {
    board: String,
    stack_depth_bb: u32,
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
    let spots: SpotsFile = toml::from_str(&spots_text).context("parsing benchmark/spots.toml")?;

    println!(
        "Building curated matrix: {} spots, tree '{}'",
        spots.spot.len(),
        cfg.solve.bet_tree.bet_tree_id
    );

    let mut summaries = Vec::new();
    let mut cross_checked = false;

    for row in &spots.spot {
        let spot = build_spot(row, &charts)?;
        let base_key = spot_key(&spot, &cfg.solve.bet_tree);
        println!("\n[{}] solving {}", summaries.len() + 1, base_key);

        // solve, with a RAM-ceiling shrink fallback (abort condition handling)
        let (solution, used_cfg, shrunk) = solve_with_shrink(&spot, &cfg)?;

        // internal invariants
        let report = gto_validate::check_invariants(&solution, &used_cfg);
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

        // equity cross-check on the first spot
        if !cross_checked {
            run_cross_check(&solution)?;
            cross_checked = true;
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
    let (oop_range, ip_range) = charts.ranges_for(&PreflopContext {
        stack_depth_bb: row.stack_depth_bb,
        node: PreflopNode::SingleRaisedPot,
    })?;
    Ok(Spot {
        stack_depth_bb: row.stack_depth_bb,
        initial_street: Street::Flop,
        board: Board { flop, turn: None, river: None },
        pot_bb: 5.0,
        effective_stack_bb: row.stack_depth_bb as f64 - 2.5,
        oop_range,
        ip_range,
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
