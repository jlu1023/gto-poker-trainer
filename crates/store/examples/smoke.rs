//! Smoke test: solve one canonical HU single-raised-pot flop spot, print the root strategy,
//! prove the store round-trip, and dump `out/smoke.json`.
//!
//! Run from the repo root:  cargo run --release -p gto-store --example smoke

use anyhow::{bail, Result};
use gto_config::Config;
use gto_engine::{solve_spot, Board, RakeSpec, Spot, SpotSolution, Street};
use gto_preflop::{PreflopCharts, PreflopContext, PreflopNode};
use gto_store::{load_solution, spot_key, store_solution};

fn main() -> Result<()> {
    let cfg = Config::default();

    // Canonical smoke spot: 100bb HU single-raised pot, BB (OOP) vs BTN (IP), flop Td9d6h.
    // pot 5bb, effective 97.5bb. With chip_per_bb = 20 that is pot 100 / stack 1950 chips.
    let charts = PreflopCharts::load_embedded()?;
    let (oop_range, ip_range) = charts.ranges_for(&PreflopContext {
        stack_depth_bb: 100,
        node: PreflopNode::SingleRaisedPot,
    })?;
    let spot = Spot {
        stack_depth_bb: 100,
        initial_street: Street::Flop,
        board: Board {
            flop: ["Td".into(), "9d".into(), "6h".into()],
            turn: None,
            river: None,
        },
        pot_bb: 5.0,
        effective_stack_bb: 97.5,
        oop_range,
        ip_range,
        rake: RakeSpec::default(),
    };

    let key = spot_key(&spot, &cfg.solve.bet_tree);
    println!("Solving smoke spot: {key}");

    let mut solution = solve_spot(&spot, &cfg.solve)?;
    println!(
        "Converged: exploitability {:.4} chips ({:.4}% of pot) in {} iterations",
        solution.exploitability_chips,
        solution.exploitability_pct_pot * 100.0,
        solution.iterations_run
    );

    print_root(&solution);

    // internal invariants
    let report = gto_validate::check_invariants(&mut solution, &cfg);
    println!(
        "Invariants: exploitability_ok={} strategy_normalized={} zero_sum_ok={} \
         tree_normalized={} best_response_ok={} (max norm error {:.2e})",
        report.exploitability_ok,
        report.strategy_normalized,
        report.zero_sum_ok,
        report.tree_normalized,
        report.best_response_ok,
        report.max_normalization_error
    );
    for note in &report.notes {
        println!("  note: {note}");
    }

    // store -> matrix
    let tree_path = store_solution(&solution, &cfg)?;
    println!("Stored solved tree -> {}", tree_path.display());

    // round-trip: load back and compare root strategy
    let reloaded = load_solution(&key, &cfg)?;
    let matched = strategies_match(&solution.root.strategy, &reloaded.root.strategy, 1e-4);
    println!("Store round-trip strategy match: {matched}");
    if !matched {
        bail!("store round-trip mismatch: reloaded root strategy differs from the original");
    }

    // dump out/smoke.json
    std::fs::create_dir_all(&cfg.io.out_dir)?;
    let out_path = std::path::Path::new(&cfg.io.out_dir).join("smoke.json");
    std::fs::write(&out_path, serde_json::to_string_pretty(&solution.summary())?)?;
    println!("Wrote {}", out_path.display());

    // gate summary
    let exploit_ok = solution.exploitability_pct_pot <= cfg.solve.target_exploit_pct_pot as f32 + 1e-6;
    if !exploit_ok {
        bail!("did not converge below target exploitability");
    }
    println!("SMOKE OK: converged below target, round-trip verified, JSON written.");
    Ok(())
}

/// Print the average action frequency at the root (unweighted mean over hands).
fn print_root(solution: &SpotSolution) {
    let node = &solution.root;
    let num_hands = node.hands.len();
    println!(
        "Root: player {} ({} hands, {} actions)",
        node.player,
        num_hands,
        node.actions.len()
    );
    for (a, label) in node.actions.iter().enumerate() {
        let mut sum = 0.0f32;
        for h in 0..num_hands {
            sum += node.strategy[h + a * num_hands];
        }
        let avg = if num_hands > 0 { sum / num_hands as f32 } else { 0.0 };
        println!("  {label:<12} avg freq {:.1}%", avg * 100.0);
    }
}

fn strategies_match(a: &[f32], b: &[f32], tol: f32) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() <= tol)
}
