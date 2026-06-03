//! `gto-validate`: correctness checks for solved spots.
//!
//! Primary = internal invariants (no commercial solver assumed):
//!   1. solver-reported exploitability <= target,
//!   2. per-node strategy normalizes to 1 for every hand,
//!   3. zero-sum EV between the two players (chip conservation).
//!
//! Optional external cross-check (Phase 1 onward): re-solve benchmark spots with opensolver (UPI)
//! and recompute equity independently with `rs_poker`. The curated stored matrix IS the benchmark
//! set, so each stored solve is a validation entry.
//!
//! Phase 0 implements the strategy-normalization invariant in full and the exploitability check
//! from solution metadata; the zero-sum and external cross-check are stubbed.

use gto_config::Config;
use gto_engine::{NodeStrategy, SpotSolution};

/// Outcome of the internal-invariant checks.
#[derive(Debug, Clone, Default)]
pub struct ValidationReport {
    pub exploitability_ok: bool,
    pub strategy_normalized: bool,
    pub max_normalization_error: f32,
    pub notes: Vec<String>,
}

impl ValidationReport {
    /// True when every implemented invariant passed.
    pub fn passed(&self) -> bool {
        self.exploitability_ok && self.strategy_normalized
    }
}

/// Run the internal invariants on a solved spot.
pub fn check_invariants(solution: &SpotSolution, cfg: &Config) -> ValidationReport {
    let mut report = ValidationReport::default();

    // 1. exploitability <= target (as a fraction of pot)
    let target = cfg.solve.target_exploit_pct_pot as f32;
    report.exploitability_ok = solution.exploitability_pct_pot <= target + 1e-6;
    if !report.exploitability_ok {
        report.notes.push(format!(
            "exploitability {:.4}% of pot exceeds target {:.4}%",
            solution.exploitability_pct_pot * 100.0,
            target * 100.0
        ));
    }

    // 2. per-node strategy normalizes to 1 (checked on the root node)
    let (ok, err) = strategy_normalized(&solution.root);
    report.strategy_normalized = ok;
    report.max_normalization_error = err;
    if !ok {
        report
            .notes
            .push(format!("root strategy not normalized (max error {err:.2e})"));
    }

    report
}

/// For every hand at a node, verify the action probabilities sum to ~1.
/// Zero-weight hands (which sum to 0) are skipped. Returns `(ok, max_error)`.
pub fn strategy_normalized(node: &NodeStrategy) -> (bool, f32) {
    let num_hands = node.hands.len();
    let num_actions = node.actions.len();
    if num_hands == 0 || num_actions == 0 {
        return (false, f32::INFINITY);
    }
    if node.strategy.len() != num_hands * num_actions {
        return (false, f32::INFINITY);
    }

    let mut max_err = 0.0f32;
    for h in 0..num_hands {
        let mut sum = 0.0f32;
        for a in 0..num_actions {
            sum += node.strategy[h + a * num_hands];
        }
        // a hand the player never reaches has total probability ~0; skip it
        if sum > 1e-3 {
            max_err = max_err.max((sum - 1.0).abs());
        }
    }
    (max_err <= 1e-3, max_err)
}

/// Independent equity cross-check via `rs_poker`. Stub for Phase 0.
///
/// Phase 1 will recompute each node's equity from the ranges with `rs_poker` and compare against
/// the solver's reported equity, and optionally diff action frequencies against an opensolver
/// (UPI) reference solve on rainbow flops.
pub fn cross_check_equity(_solution: &SpotSolution) -> anyhow::Result<()> {
    Ok(())
}
