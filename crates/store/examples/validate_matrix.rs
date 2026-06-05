//! Re-validate every stored matrix entry with the full invariant set, printing the raw
//! arithmetic (zero-sum components, best-response recomputation, tree-walk stats) so each
//! check can be hand-verified at the data layer - the F5 discipline: confirm a check passes
//! for the right reason before trusting it as a gate.
//!
//! Run from the repo root:  cargo run --release -p gto-store --example validate_matrix [key-filter]

use anyhow::{Context, Result};
use gto_config::Config;
use gto_store::{load_manifest, load_solution};
use gto_validate::{check_invariants, zero_sum_root};
use std::path::Path;
use std::time::Instant;

fn main() -> Result<()> {
    let cfg = Config::load("phase1.toml").context("loading phase1.toml")?;
    let filter = std::env::args().nth(1).unwrap_or_default();

    let manifest = load_manifest(Path::new(&cfg.io.matrix_dir))?;
    let mut keys: Vec<&String> = manifest
        .spots
        .keys()
        .filter(|k| filter.is_empty() || k.contains(&filter))
        .collect();
    keys.sort();
    println!("validating {} stored spot(s){}", keys.len(),
        if filter.is_empty() { String::new() } else { format!(" (filter '{filter}')") });

    let mut all_ok = true;
    for key in keys {
        println!("\n=== {key} ===");
        let mut solution = load_solution(key, &cfg)?;
        let pot_chips = solution.spot.pot_bb * cfg.solve.chip_per_bb as f64;

        // the raw zero-sum arithmetic, for hand-verification (cheap; no tree walk)
        let zs = zero_sum_root(&mut solution.game, pot_chips)?;
        println!(
            "  zero-sum arithmetic: avg_ev_oop={:.4} + avg_ev_ip={:.4} = {:.4} vs pot {:.1}  residual={:+.5} chips ({:+.5}% pot)",
            zs.avg_ev_oop,
            zs.avg_ev_ip,
            zs.avg_ev_oop + zs.avg_ev_ip,
            pot_chips,
            zs.residual_chips,
            zs.residual_chips / pot_chips * 100.0,
        );

        // the full gate (includes the tree walk; timed)
        let t0 = Instant::now();
        let report = check_invariants(&mut solution, &cfg);
        println!(
            "  gate: exploitability_ok={} strategy_normalized={} ev_consistency_ok={} zero_sum_ok={} tree_normalized={} best_response_ok={}",
            report.exploitability_ok,
            report.strategy_normalized,
            report.ev_consistency_ok,
            report.zero_sum_ok,
            report.tree_normalized,
            report.best_response_ok
        );
        println!(
            "  detail: best_response recomputed={:.4} chips vs stored={:.4}; tree {} decision nodes, max_norm_err={:.2e}; {:.2}s",
            report.best_response_recomputed_chips,
            solution.exploitability_chips,
            report.tree_decision_nodes,
            report.tree_max_normalization_error,
            t0.elapsed().as_secs_f64()
        );
        for n in &report.notes {
            println!("    note: {n}");
        }
        println!("  [{}]", if report.passed() { "PASS" } else { "FAIL" });
        all_ok &= report.passed();
    }

    println!("\n{}", if all_ok { "ALL SPOTS PASS" } else { "FAILURES PRESENT" });
    std::process::exit(if all_ok { 0 } else { 1 });
}
