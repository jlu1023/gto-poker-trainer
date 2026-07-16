//! `gto-validate`: correctness checks for solved spots.
//!
//! `check_invariants` runs SIX internal invariants on every fresh solve (no commercial solver
//! assumed), each hand-verified against known-good converged data before being gated:
//!   1. solver-reported exploitability <= target,
//!   2. root strategy rows finite and normalized to 1,
//!   3. per-action EV consistency (dot-product identity + no dominant unplayed action, the
//!      F5 regression gate),
//!   4. zero-sum EV at the root (avg_ev_oop + avg_ev_ip == starting pot; profile-independent),
//!   5. strategy rows finite + normalized at EVERY decision node (full-tree walk; guards the
//!      storage/decode/swap pipeline),
//!   6. best-response recheck (`compute_exploitability` recomputed on the finalized game must
//!      meet the target and match the recorded figure).
//!
//! On-demand (not per-solve): `cross_check_equity` (independent rs_poker enumeration), the
//! `validate_matrix` store example (re-runs the full gate over every stored spot, printing
//! the raw arithmetic), and the `suit_isomorphism` example (two fresh solves of suit-relabeled
//! boards must match exactly, with a board-blocked negative control).
//!
//! The opensolver (UPI) external cross-check remains future work. The curated stored matrix IS
//! the benchmark set, so each stored solve is a validation entry.

pub mod advisory;

use anyhow::{anyhow, bail, Result};
use gto_config::Config;
use gto_engine::{
    card_from_str, compute_exploitability, read_positioned_node, NodeStrategy, PostFlopGame,
    SolverAction, SolverRange, Spot, SpotSolution,
};
use rs_poker::core::{Card, Hand, Rankable, Suit, Value};

/// Outcome of the internal-invariant checks.
#[derive(Debug, Clone, Default)]
pub struct ValidationReport {
    pub exploitability_ok: bool,
    pub strategy_normalized: bool,
    pub max_normalization_error: f32,
    /// Per-action EV checks (Phase 5, audit follow-up P1): dot-product identity + no
    /// dominant unplayed action. True when ev_per_action is absent (nothing to check).
    pub ev_consistency_ok: bool,
    /// Root zero-sum identity: avg_ev_oop + avg_ev_ip == starting pot (see `zero_sum_root`).
    pub zero_sum_ok: bool,
    pub zero_sum_residual_chips: f64,
    /// Full-tree strategy walk: finite rows summing to 1 at every decision node.
    pub tree_normalized: bool,
    pub tree_decision_nodes: usize,
    pub tree_max_normalization_error: f32,
    /// Best-response recheck: independently recomputed exploitability still meets the target
    /// and matches the recorded figure.
    pub best_response_ok: bool,
    pub best_response_recomputed_chips: f32,
    pub notes: Vec<String>,
}

impl ValidationReport {
    /// True when every implemented invariant passed.
    pub fn passed(&self) -> bool {
        self.exploitability_ok
            && self.strategy_normalized
            && self.ev_consistency_ok
            && self.zero_sum_ok
            && self.tree_normalized
            && self.best_response_ok
    }
}

/// Zero-sum tolerance as a fraction of the pot. The identity is profile-independent (fp +
/// u16-quantization error only); measured residuals on the benchmark are ~1e-4 chips on
/// 100-chip pots, so 0.1% of pot has ~750x headroom and is ~300x below an F5-class shift.
const ZERO_SUM_TOL_FRAC_OF_POT: f64 = 0.001;
/// Tree-walk node backstop. A full SRP flop tree runs ~180-200k decision nodes (~470k+
/// total, ~11 s release); the budget exists only to stop a runaway tree, and a truncated
/// walk FAILS the invariant (never a silent partial pass).
const TREE_WALK_NODE_BUDGET: usize = 2_000_000;
/// Best-response slack over the convergence target (10%) and drift vs the recorded figure
/// (25% of target). Measured on the benchmark: recomputed == stored to 4 decimals (drift
/// ~0.0000 chips), so both bounds carry large headroom while still catching storage
/// corruption (which explodes the recomputation) or a mis-recorded figure.
const BR_CEILING_SLACK: f32 = 0.10;
const BR_DRIFT_TOL_FRAC_OF_TARGET: f32 = 0.25;

/// Run the internal invariants on a solved spot. Takes `&mut` because the game-layer checks
/// (zero-sum, tree walk, best-response) navigate `solution.game`; every navigation ends with
/// `back_to_root`, so the solution is returned in its canonical state.
pub fn check_invariants(solution: &mut SpotSolution, cfg: &Config) -> ValidationReport {
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

    // 2. strategy normalizes to 1 at the root (data layer; the tree walk below covers depth)
    let (ok, err) = strategy_normalized(&solution.root);
    report.strategy_normalized = ok;
    report.max_normalization_error = err;
    if !ok {
        report
            .notes
            .push(format!("root strategy not normalized (max error {err:.2e})"));
    }

    // 3. per-action EV checks (Phase 5; promotes the manual F5 catch into a permanent gate)
    let pot_chips = (solution.spot.pot_bb * cfg.solve.chip_per_bb as f64) as f32;
    let (ev_ok, ev_notes) = ev_consistency(&solution.root, pot_chips);
    report.ev_consistency_ok = ev_ok;
    report.notes.extend(ev_notes);

    // 4. zero-sum at the root (game layer; hand-verified on 100bb_Ks7h2c: 52.1122 + 47.8878
    //    = 100.0000 vs pot 100, residual +3e-5 chips)
    let pot_chips_f64 = solution.spot.pot_bb * cfg.solve.chip_per_bb as f64;
    match zero_sum_root(&mut solution.game, pot_chips_f64) {
        Ok(zs) => {
            report.zero_sum_residual_chips = zs.residual_chips;
            report.zero_sum_ok = zero_sum_ok(&zs, pot_chips_f64 * ZERO_SUM_TOL_FRAC_OF_POT);
            if !report.zero_sum_ok {
                report.notes.push(format!(
                    "zero-sum violated at root: avg_ev_oop {:.4} + avg_ev_ip {:.4} = {:.4} \
                     vs pot {:.1} (residual {:+.5} chips); check the EV reference before \
                     suspecting the engine (F5-class)",
                    zs.avg_ev_oop,
                    zs.avg_ev_ip,
                    zs.avg_ev_oop + zs.avg_ev_ip,
                    pot_chips_f64,
                    zs.residual_chips
                ));
            }
        }
        Err(e) => {
            report.zero_sum_ok = false;
            report.notes.push(format!("zero-sum check errored: {e}"));
        }
    }

    // 5. strategy rows finite + normalized at EVERY decision node (storage/decode guard)
    let walk = tree_strategy_normalized(&mut solution.game, TREE_WALK_NODE_BUDGET);
    report.tree_normalized = walk.ok && !walk.truncated;
    report.tree_decision_nodes = walk.decision_nodes;
    report.tree_max_normalization_error = walk.max_normalization_error;
    if !report.tree_normalized {
        report.notes.push(format!(
            "tree walk failed: {} decision nodes, max_norm_err {:.2e}, {} non-finite rows, \
             truncated={}",
            walk.decision_nodes, walk.max_normalization_error, walk.non_finite_rows, walk.truncated
        ));
    }

    // 6. best-response recheck (the equilibrium claim, recomputed independently)
    let target_chips = (pot_chips_f64 * cfg.solve.target_exploit_pct_pot) as f32;
    let br = best_response_recheck(&solution.game, solution.exploitability_chips, target_chips);
    report.best_response_recomputed_chips = br.recomputed_chips;
    report.best_response_ok = best_response_ok(
        &br,
        BR_CEILING_SLACK,
        target_chips * BR_DRIFT_TOL_FRAC_OF_TARGET,
    );
    if !report.best_response_ok {
        report.notes.push(format!(
            "best-response recheck failed: recomputed {:.4} chips vs stored {:.4} \
             (target {:.4})",
            br.recomputed_chips, br.stored_chips, br.target_chips
        ));
    }

    report
}

/// Per-action EV invariants on a node that carries `ev_per_action` (skipped when absent).
///
/// (a) Dot-product identity: `ev[h] == sum_a strategy[h,a] * ev_per_action[h,a]` (that is the
///     solver's own definition of the mix EV; the audit measured max error 0.0000, tolerance
///     here 0.05 chips).
/// (b) No dominant unplayed action: an action with ~0 frequency may not have meaningfully
///     higher EV than the best PLAYED action (corrected F5 form: a wide full spread is
///     legitimate for strong made hands; what is impossible at equilibrium is leaving a
///     better action unplayed). Audit-measured legitimate worst: 0.086 chips; F5-class
///     violations are 30+ chips; tolerance 1% of pot separates them cleanly.
pub fn ev_consistency(node: &NodeStrategy, pot_chips: f32) -> (bool, Vec<String>) {
    let n = node.hands.len();
    let num_actions = node.actions.len();
    if node.ev_per_action.len() != n * num_actions || n == 0 || num_actions == 0 {
        return (true, Vec::new()); // nothing to check (pre-Phase-4 sidecar or non-decision node)
    }

    let mut notes = Vec::new();
    let dot_tol = 0.05_f32; // chips
    let dominance_tol = 0.01_f32 * pot_chips; // 1% of pot, in chips
    let (max_dot_err, max_dominance) = ev_quantities(node);

    let mut ok = true;
    if max_dot_err > dot_tol {
        ok = false;
        notes.push(format!(
            "ev != strategy . ev_per_action (max error {max_dot_err:.3} chips > {dot_tol})"
        ));
    }
    if max_dominance > dominance_tol {
        ok = false;
        notes.push(format!(
            "an unplayed action dominates the played mix by {max_dominance:.3} chips \
             (> {dominance_tol:.2}); F5-class reference error suspected"
        ));
    }
    (ok, notes)
}

/// The two per-action-EV quantities at a node, over hands with strategy mass: the worst
/// dot-identity error `|ev[h] - sum_a strategy . ev_per_action|` and the worst unplayed-action
/// dominance `best_overall_EV - best_PLAYED_EV`, both in chips. `(0, 0)` when the node carries
/// no per-action EV. The single place both are computed, so the hard F5 check and the
/// display-gate signal cannot drift apart.
fn ev_quantities(node: &NodeStrategy) -> (f32, f32) {
    let n = node.hands.len();
    let num_actions = node.actions.len();
    if node.ev_per_action.len() != n * num_actions || n == 0 || num_actions == 0 {
        return (0.0, 0.0);
    }
    let (mut max_dot_err, mut max_dominance) = (0.0f32, 0.0f32);
    for h in 0..n {
        // skip hands the player never holds here (zero strategy mass)
        let mass: f32 = (0..num_actions).map(|a| node.strategy[h + a * n]).sum();
        if mass <= 1e-3 {
            continue;
        }
        let dot: f32 = (0..num_actions)
            .map(|a| node.strategy[h + a * n] * node.ev_per_action[h + a * n])
            .sum();
        max_dot_err = max_dot_err.max((dot - node.ev[h]).abs());

        let best_played = (0..num_actions)
            .filter(|&a| node.strategy[h + a * n] >= 0.01)
            .map(|a| node.ev_per_action[h + a * n])
            .fold(f32::NEG_INFINITY, f32::max);
        let best_overall = (0..num_actions)
            .map(|a| node.ev_per_action[h + a * n])
            .filter(|e| e.is_finite())
            .fold(f32::NEG_INFINITY, f32::max);
        if best_played.is_finite() && best_overall.is_finite() {
            max_dominance = max_dominance.max(best_overall - best_played);
        }
    }
    (max_dot_err, max_dominance)
}

/// The DOT-IDENTITY F5 check alone (reference-correctness): `ev[h] == sum_a strategy .
/// ev_per_action`. Offset-invariant and convergence-INDEPENDENT (both sides carry the node's
/// one shared money-reference offset, which cancels), so it is a valid HARD gate at ANY node -
/// unlike the dominance check, which reflects LOCAL convergence and is not (D55). The navigator
/// uses this as its hard per-node F5 gate. Passes trivially when the node has no per-action EV.
pub fn ev_dot_consistency(node: &NodeStrategy) -> (bool, Vec<String>) {
    let (max_dot_err, _) = ev_quantities(node);
    let dot_tol = 0.05_f32; // chips
    if max_dot_err > dot_tol {
        (
            false,
            vec![format!(
                "ev != strategy . ev_per_action (max error {max_dot_err:.3} chips > {dot_tol})"
            )],
        )
    } else {
        (true, Vec::new())
    }
}

/// The worst unplayed-action dominance at a node (chips): `best_overall_EV - best_PLAYED_EV`
/// over hands with strategy mass. A LOCAL SOLVE-QUALITY signal (how far the stored strategy is
/// from playing the best action here), NOT a reference-correctness check: it reflects local
/// (not global) convergence, is large at low-reach nodes a global-exploitability solve does not
/// train, and does not shrink with tighter global convergence (D55). It is the basis of the
/// navigator's display gate and per-node confidence - never a hard F5 gate at non-root nodes.
/// `0` when the node carries no per-action EV.
pub fn node_dominance(node: &NodeStrategy) -> f32 {
    ev_quantities(node).1
}

/// For every hand at a node, verify the action probabilities are FINITE and sum to ~1.
/// Zero-weight hands (which sum to 0) are skipped. Returns `(ok, max_error)`.
/// A non-finite probability fails outright: NaN would silently skip the sum filter
/// (NaN comparisons are false), so finiteness is checked before the sum.
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
            let p = node.strategy[h + a * num_hands];
            if !p.is_finite() {
                return (false, f32::INFINITY);
            }
            sum += p;
        }
        // a hand the player never reaches has total probability ~0; skip it
        if sum > 1e-3 {
            max_err = max_err.max((sum - 1.0).abs());
        }
    }
    (max_err <= 1e-3, max_err)
}

// ---------------------------------------------------------------------------
// game-layer invariants (#2a real implementation + audit follow-ups)
// ---------------------------------------------------------------------------

/// The zero-sum identity measured at the ROOT.
///
/// Reference-point statement (the F5 surface, stated precisely): `expected_values(player)` is
/// offset by `starting_pot/2 + node.amount + bias` (interpreter.rs:795). At the ROOT,
/// `node.amount == 0` and `bias == 0` for BOTH players, so the two players' EVs share one
/// consistent money reference, and the range-weighted averages must satisfy
/// `avg_ev_oop + avg_ev_ip == starting_pot` for ANY strategy profile (every terminal divides
/// pot + bets zero-sum; this is a payoff-structure identity, not a convergence property).
/// The check is deliberately root-only: at deeper nodes the `bias` term is player-asymmetric
/// and a wrong-reference deep check is exactly the F5 trap.
#[derive(Debug, Clone)]
pub struct ZeroSumCheck {
    pub avg_ev_oop: f64,
    pub avg_ev_ip: f64,
    pub pot_chips: f64,
    /// `avg_ev_oop + avg_ev_ip - pot_chips`
    pub residual_chips: f64,
}

/// Compute the root zero-sum residual. Weights: `normalized_weights` (actual combo weights
/// with card removal, the same weighting the equity/EV range averages use; blocked combos
/// carry weight 0 and drop out).
pub fn zero_sum_root(game: &mut PostFlopGame, pot_chips: f64) -> Result<ZeroSumCheck> {
    game.back_to_root();
    game.cache_normalized_weights();
    let mut avg = [0.0f64; 2];
    for player in 0..2 {
        let evs = game.expected_values(player);
        let weights = game.normalized_weights(player);
        if evs.len() != weights.len() {
            bail!(
                "zero-sum: ev len {} != weight len {} for player {player}",
                evs.len(),
                weights.len()
            );
        }
        let mut num = 0.0f64;
        let mut den = 0.0f64;
        for (e, w) in evs.iter().zip(weights) {
            num += *e as f64 * *w as f64;
            den += *w as f64;
        }
        if den <= 0.0 {
            bail!("zero-sum: zero total weight for player {player}");
        }
        avg[player] = num / den;
    }
    game.back_to_root();
    Ok(ZeroSumCheck {
        avg_ev_oop: avg[0],
        avg_ev_ip: avg[1],
        pot_chips,
        residual_chips: avg[0] + avg[1] - pot_chips,
    })
}

/// The identity is profile-independent, so the tolerance covers floating point and the u16
/// compression quantization only, NOT solve convergence. Hand-verified on the benchmark set
/// (measured residuals: +0.00003 chips on 100bb_Ks7h2c, -0.00013 on 50bb_Ks7h2c, 100-chip
/// pots). The 0.1%-of-pot tolerance (0.1 chips here) is ~750x the observed residual and
/// ~300x below any real reference error (F5-class shifts are whole bet sizes, 30%+ of pot).
pub fn zero_sum_ok(check: &ZeroSumCheck, tol_chips: f64) -> bool {
    check.residual_chips.is_finite() && check.residual_chips.abs() <= tol_chips
}

/// Walk every reachable node and validate decision-node strategy rows (finite + sum to 1).
///
/// What this CAN catch: NaN/Inf propagation, corrupted or mis-sized strategy storage, and
/// decode errors in the compressed (u16) strategy path at ANY node, not just the root.
/// What it CANNOT catch: a solver that mis-normalizes internally, because `strategy()`
/// re-normalizes finite values on read; the sum check therefore guards the storage/decode/swap
/// pipeline, not the solver's arithmetic. Chance branches follow `available_actions()`, which
/// lists one representative card per isomorphic class; the suit-swapped variants share the
/// same stored data by construction, so each stored strategy table is still visited.
#[derive(Debug, Clone, Default)]
pub struct TreeWalkReport {
    pub ok: bool,
    pub decision_nodes: usize,
    pub chance_branches: usize,
    pub terminal_nodes: usize,
    pub max_normalization_error: f32,
    pub non_finite_rows: usize,
    /// True if the node budget stopped the walk early (reported loudly, never silent).
    pub truncated: bool,
}

pub fn tree_strategy_normalized(game: &mut PostFlopGame, node_budget: usize) -> TreeWalkReport {
    let mut report = TreeWalkReport {
        ok: true,
        ..Default::default()
    };
    let mut stack: Vec<Vec<usize>> = vec![Vec::new()];
    let mut visited = 0usize;

    while let Some(history) = stack.pop() {
        visited += 1;
        if visited > node_budget {
            report.truncated = true;
            break;
        }
        game.apply_history(&history);

        if game.is_terminal_node() {
            report.terminal_nodes += 1;
            continue;
        }
        if game.is_chance_node() {
            for action in game.available_actions() {
                if let SolverAction::Chance(card) = action {
                    report.chance_branches += 1;
                    let mut h = history.clone();
                    h.push(card as usize);
                    stack.push(h);
                }
            }
            continue;
        }

        // decision node: every reached hand's row must be finite and sum to ~1
        report.decision_nodes += 1;
        let num_actions = game.available_actions().len();
        let strategy = game.strategy();
        let num_hands = strategy.len() / num_actions.max(1);
        for h in 0..num_hands {
            let mut sum = 0.0f32;
            let mut finite = true;
            for a in 0..num_actions {
                let p = strategy[h + a * num_hands];
                finite &= p.is_finite();
                sum += p;
            }
            if !finite {
                report.non_finite_rows += 1;
                report.ok = false;
                continue;
            }
            if sum > 1e-3 {
                let err = (sum - 1.0).abs();
                report.max_normalization_error = report.max_normalization_error.max(err);
                if err > 1e-3 {
                    report.ok = false;
                }
            }
        }

        for idx in 0..num_actions {
            let mut h = history.clone();
            h.push(idx);
            stack.push(h);
        }
    }
    game.back_to_root();
    report
}

/// The F5 result over a spot's flop decision layer (freshly checked; no shape exempted).
#[derive(Debug, Default, Clone)]
pub struct FlopNodeReport {
    pub ok: bool,
    pub decision_nodes: usize,
    pub chance_nodes_stopped: usize,
    pub terminal_nodes: usize,
    pub empty_epa_nodes: usize,
    /// Worst unplayed-action dominance across the flop layer (chips). REPORTED, not gated on:
    /// dominance is a local-convergence / display-gate signal, not a hard F5 failure (D55).
    pub max_dominance_chips: f32,
    pub truncated: bool,
    pub notes: Vec<String>,
}

/// The F5 battery behind the game-tree navigator. Walks EVERY flop DECISION node reachable by
/// action steps (a chance node STOPS the walk - the flop-only boundary; the turn is out of
/// scope) and at each reads the node with F5-correct per-action EV (`read_positioned_node`),
/// then asserts `strategy_normalized`, `ev_consistency`, and per-action EV present. No shape is
/// exempted: the root and IP-after-check are re-checked here, because a read that is correct in
/// the solve contract or the review path is only an assumption at the surface the navigator
/// renders until asserted at that surface. `pot_chips` is the starting pot in chips (as
/// `check_invariants` derives it). One pass over one cached/solved spot covers its whole flop
/// layer, deeper re-raise/all-in nodes included.
pub fn flop_node_ev_consistency(game: &mut PostFlopGame, node_budget: usize) -> FlopNodeReport {
    let mut report = FlopNodeReport {
        ok: true,
        ..Default::default()
    };
    let mut stack: Vec<Vec<usize>> = vec![Vec::new()];
    let mut visited = 0usize;

    while let Some(history) = stack.pop() {
        visited += 1;
        if visited > node_budget {
            report.truncated = true;
            report.ok = false;
            report
                .notes
                .push(format!("node budget {node_budget} exhausted; walk truncated"));
            break;
        }
        game.apply_history(&history);

        if game.is_terminal_node() {
            report.terminal_nodes += 1;
            continue;
        }
        if game.is_chance_node() {
            // flop-only: the turn card stops the walk; do NOT recurse past it
            report.chance_nodes_stopped += 1;
            continue;
        }

        report.decision_nodes += 1;
        let node = match read_positioned_node(game) {
            Ok(n) => n,
            Err(e) => {
                report.ok = false;
                report.notes.push(format!("node {history:?}: read failed: {e}"));
                continue;
            }
        };
        if node.ev_per_action.is_empty() {
            report.empty_epa_nodes += 1;
            report.ok = false;
            report.notes.push(format!(
                "node {history:?}: per-action EV absent at a decision node (would render without F5 data)"
            ));
        }
        let (sn_ok, sn_err) = strategy_normalized(&node);
        if !sn_ok {
            report.ok = false;
            report
                .notes
                .push(format!("node {history:?}: strategy not normalized (err {sn_err:.2e})"));
        }
        // HARD F5 gate: the dot identity (reference-correctness), valid at every node.
        // Dominance is a LOCAL-convergence signal - reported below, NOT gated on (D55).
        let (dot_ok, dot_notes) = ev_dot_consistency(&node);
        if !dot_ok {
            report.ok = false;
            for note in dot_notes {
                report.notes.push(format!("node {history:?}: {note}"));
            }
        }
        report.max_dominance_chips = report.max_dominance_chips.max(node_dominance(&node));

        // recurse into ACTION children (a child that is a chance/terminal node is caught by the
        // stop conditions above on its own visit)
        let num_actions = game.available_actions().len();
        for idx in 0..num_actions {
            let mut h = history.clone();
            h.push(idx);
            stack.push(h);
        }
    }

    game.back_to_root();
    report
}

/// Best-response self-check: exploitability is DEFINED as the best-responding opponent's
/// gain, and `compute_exploitability` (the crate's public primitive, the same function the
/// solve loop's reported figure comes from) recomputes it from the stored strategies by an
/// independent tree traversal. Recomputing on the finalized / reloaded game verifies that
/// the tree we stored actually is the near-equilibrium the recorded number claims.
#[derive(Debug, Clone)]
pub struct BestResponseCheck {
    pub recomputed_chips: f32,
    pub stored_chips: f32,
    pub target_chips: f32,
}

pub fn best_response_recheck(game: &PostFlopGame, stored_chips: f32, target_chips: f32) -> BestResponseCheck {
    BestResponseCheck {
        recomputed_chips: compute_exploitability(game),
        stored_chips,
        target_chips,
    }
}

/// Passes when the recomputed exploitability still meets the convergence contract
/// (<= target + slack) and does not drift from the recorded figure by more than
/// `drift_tol_chips`. The recorded figure was computed pre-`finalize` on the current
/// strategy; the recheck runs post-finalize/reload on the stored average strategy, so a
/// small drift is expected - the tolerances are set from measured benchmark drift.
pub fn best_response_ok(check: &BestResponseCheck, ceiling_slack: f32, drift_tol_chips: f32) -> bool {
    check.recomputed_chips.is_finite()
        && check.recomputed_chips <= check.target_chips * (1.0 + ceiling_slack)
        && (check.recomputed_chips - check.stored_chips).abs() <= drift_tol_chips
}

/// Result of comparing the solver's reported hero equity against an independent rs_poker enumeration.
#[derive(Debug, Clone)]
pub struct EquityCrossCheck {
    pub hero_hand: String,
    pub hero_player: usize,
    pub solver_equity: f32,
    pub enumerated_equity: f32,
    pub abs_diff: f32,
    pub villain_combos: usize,
}

impl EquityCrossCheck {
    pub fn agrees(&self, tolerance: f32) -> bool {
        self.abs_diff <= tolerance
    }
}

/// Independent equity cross-check via `rs_poker` (no `arena` feature). Recomputes the root equity of
/// one hero hand vs the opponent's range by EXHAUSTIVE turn+river enumeration (deterministic, no RNG)
/// and compares it to the solver's reported `root.equity[hero_index]`. Flop spots only.
///
/// This is a genuine independent check: rs_poker is used solely for card representation and 7-card
/// hand ranking, with the enumeration and equity tally written here.
pub fn cross_check_equity(solution: &SpotSolution, hero_index: usize) -> Result<EquityCrossCheck> {
    let node = &solution.root;
    let hero_label = node
        .hands
        .get(hero_index)
        .ok_or_else(|| anyhow!("hero_index {hero_index} out of range ({} hands)", node.hands.len()))?;
    let solver_equity = *node
        .equity
        .get(hero_index)
        .ok_or_else(|| anyhow!("no solver equity for hero_index {hero_index}"))?;

    let hero = parse_hole(hero_label)?;
    let board = parse_board(&solution.spot)?;
    if board.len() != 3 {
        bail!("equity cross-check supports flop spots only (board has {} cards)", board.len());
    }

    // the opponent of the player to act at the root
    let villain_range = if node.player == 0 {
        &solution.spot.ip_range
    } else {
        &solution.spot.oop_range
    };

    let (enumerated_equity, villain_combos) = enumerate_equity(&hero, &board, villain_range)?;
    Ok(EquityCrossCheck {
        hero_hand: hero_label.clone(),
        hero_player: node.player,
        solver_equity,
        enumerated_equity,
        abs_diff: (solver_equity - enumerated_equity).abs(),
        villain_combos,
    })
}

/// Hero equity vs a villain range on a flop, by exhaustive turn+river enumeration with card
/// removal. Returns (equity in 0..1, number of unblocked villain combos averaged over).
///
/// The villain range is parsed by the AUTHORITATIVE parser (postflop-solver, via the engine
/// re-export): it is the only parser that knows the weighted ":w" syntax the authored ranges
/// use, and the solver equity this check reconciles against is villain-reach-WEIGHTED, so the
/// independent tally must weight identically. The first weighted-villain check (3bp 100bb
/// 9c5d3h, BTN call-vs-3-bet at fractional weights) measured the unweighted version 1.8 pts
/// off the solver for exactly this reason. rs_poker remains the independent part: the 7-card
/// RANKING that produces each combo's equity. Range-string semantics are not independent -
/// they belong to the system under test.
fn enumerate_equity(hero: &[Card; 2], board: &[Card], villain_range: &str) -> Result<(f32, usize)> {
    let range: SolverRange = villain_range
        .parse()
        .map_err(|e| anyhow!("parsing villain range '{villain_range}': {e}"))?;
    let rs_card = psf_to_rs_table()?;

    let full_deck = all_cards();
    let mut base_used: Vec<Card> = board.to_vec();
    base_used.extend_from_slice(hero);

    let mut sum_equity = 0.0f64;
    let mut weight_sum = 0.0f64;
    let mut counted = 0usize;

    let mut pairs: Vec<(Card, Card, f64)> = Vec::new();
    for id0 in 0..52u8 {
        for id1 in (id0 + 1)..52u8 {
            let w = range.get_weight_by_cards(id0, id1) as f64;
            if w > 0.0 {
                pairs.push((rs_card[id0 as usize], rs_card[id1 as usize], w));
            }
        }
    }

    for &(v0, v1, w) in &pairs {
        // skip villain combos blocked by the hero's cards or the board
        if base_used.contains(&v0) || base_used.contains(&v1) {
            continue;
        }

        let mut used = base_used.clone();
        used.push(v0);
        used.push(v1);
        let remaining: Vec<Card> = full_deck.iter().copied().filter(|c| !used.contains(c)).collect();

        let mut win = 0u32;
        let mut tie = 0u32;
        let mut total = 0u32;
        for i in 0..remaining.len() {
            for j in (i + 1)..remaining.len() {
                let turn = remaining[i];
                let river = remaining[j];
                let hero_rank = Hand::new_with_cards(vec![
                    hero[0], hero[1], board[0], board[1], board[2], turn, river,
                ])
                .rank();
                let villain_rank = Hand::new_with_cards(vec![
                    v0, v1, board[0], board[1], board[2], turn, river,
                ])
                .rank();
                match hero_rank.cmp(&villain_rank) {
                    std::cmp::Ordering::Greater => win += 1,
                    std::cmp::Ordering::Equal => tie += 1,
                    std::cmp::Ordering::Less => {}
                }
                total += 1;
            }
        }
        if total > 0 {
            sum_equity += w * ((win as f64 + 0.5 * tie as f64) / total as f64);
            weight_sum += w;
            counted += 1;
        }
    }

    if counted == 0 {
        bail!("no unblocked villain combos for range '{villain_range}'");
    }
    Ok(((sum_equity / weight_sum) as f32, counted))
}

fn parse_hole(label: &str) -> Result<[Card; 2]> {
    if label.len() != 4 {
        bail!("unexpected hole-card label '{label}'");
    }
    Ok([parse_card(&label[0..2])?, parse_card(&label[2..4])?])
}

fn parse_board(spot: &Spot) -> Result<Vec<Card>> {
    let mut cards = Vec::new();
    for s in &spot.board.flop {
        cards.push(parse_card(s)?);
    }
    if let Some(t) = &spot.board.turn {
        cards.push(parse_card(t)?);
    }
    if let Some(r) = &spot.board.river {
        cards.push(parse_card(r)?);
    }
    Ok(cards)
}

fn parse_card(s: &str) -> Result<Card> {
    Card::try_from(s).map_err(|e| anyhow!("parsing card '{s}': {e:?}"))
}

fn all_cards() -> Vec<Card> {
    let mut cards = Vec::with_capacity(52);
    for suit in Suit::suits() {
        for value in Value::values() {
            cards.push(Card::new(value, suit));
        }
    }
    cards
}

/// postflop-solver card id -> rs_poker Card, built by parsing every one of the 52 card
/// STRINGS through both parsers (no encoding arithmetic is assumed for either crate). The
/// permutation check makes a parser disagreement loud instead of a silent mis-mapping.
fn psf_to_rs_table() -> Result<[Card; 52]> {
    let mut table = [Card::new(Value::Two, Suit::Club); 52];
    let mut seen = [false; 52];
    for r in "23456789TJQKA".chars() {
        for s in "cdhs".chars() {
            let label = format!("{r}{s}");
            let id = card_from_str(&label)
                .map_err(|e| anyhow!("postflop-solver rejects card '{label}': {e}"))?
                as usize;
            if id >= 52 || seen[id] {
                bail!("postflop-solver card ids are not a permutation (card '{label}' -> {id})");
            }
            seen[id] = true;
            table[id] = parse_card(&label)?;
        }
    }
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny synthetic node: 2 hands x 2 actions, strategy[hand + action*num_hands].
    fn node(strategy: Vec<f32>) -> NodeStrategy {
        NodeStrategy {
            player: 0,
            actions: vec!["Check".into(), "Bet(33)".into()],
            hands: vec!["AsAh".into(), "KsKh".into()],
            strategy,
            ev: vec![50.0, 48.0],
            equity: vec![0.8, 0.7],
            ev_per_action: Vec::new(),
        }
    }

    /// Parse a bet/raise/all-in label ("Bet(33)" / "Raise(99)" / "AllIn(240)") to its chip
    /// amount; non-sizing actions (Check/Call/Fold) yield 0.
    fn action_amount(label: &str) -> f32 {
        for pre in ["Bet(", "Raise(", "AllIn("] {
            if let Some(rest) = label.strip_prefix(pre) {
                if let Some(num) = rest.strip_suffix(')') {
                    return num.parse::<f32>().unwrap_or(0.0);
                }
            }
        }
        0.0
    }

    /// The navigator F5 battery, end to end and self-contained (no matrix): a small spot solves
    /// fast, EVERY flop decision node passes the battery (no exemptions, amendment 1), and - the
    /// load-bearing part (amendment 2) - the battery's `ev_consistency` check has TEETH: the
    /// original F5 bug shape (per-action EV taken by descending into each child, so each action's
    /// money reference shifts by its own bet amount) MUST fail it at a non-root node. A check
    /// that cannot fail proves nothing.
    #[test]
    fn flop_navigator_f5_battery_has_teeth() {
        use gto_engine::{read_node_labeled, solve_spot, Board, RakeSpec, Spot, Street};

        // small ranges + shallow stack => a real near-equilibrium that solves in a moment
        let spot = Spot {
            stack_depth_bb: 100,
            initial_street: Street::Flop,
            board: Board {
                flop: ["Ks".into(), "7h".into(), "2c".into()],
                turn: None,
                river: None,
            },
            pot_bb: 5.0,
            effective_stack_bb: 97.5,
            oop_range: "QQ+,AKs,A5s".into(),
            ip_range: "QQ+,AKs,AQs".into(),
            rake: RakeSpec::default(),
        };
        // The default (smoke-v0) tree - Check / Bet(33), no raises - is a small, shallow flop
        // tree that converges in a couple hundred iterations on a dry board, so every flop node
        // is a real near-equilibrium the battery can hold to the 1%-of-pot dominance tolerance.
        // The srp-v2 raise / check-raise shapes are battery-covered on the real cached matrix by
        // the node_grid oracle and verify_navigator.py (no shape is exempted, only relocated to
        // where a well-solved instance of it exists).
        let mut cfg = Config::default();
        cfg.solve.target_exploit_pct_pot = 0.0005;
        cfg.solve.max_iterations = 1000;

        let mut solution = solve_spot(&spot, &cfg.solve).expect("small spot solves");

        // POSITIVE: every flop decision node passes (root and IP-after-check included).
        let report = flop_node_ev_consistency(&mut solution.game, 100_000);
        assert!(
            report.ok && !report.truncated,
            "flop F5 battery failed: {} decision nodes, notes={:?}",
            report.decision_nodes,
            report.notes
        );
        assert!(
            report.decision_nodes >= 3,
            "expected several flop decision nodes, got {}",
            report.decision_nodes
        );

        // the correct read of a NON-ROOT node (IP facing a check) is F5-consistent
        let good =
            read_node_labeled(&mut solution.game, &["Check".to_string()]).expect("read [Check]");
        assert_eq!(good.player, 1, "[Check] is IP's node");
        assert!(
            ev_dot_consistency(&good).0,
            "the correct non-root read must pass the dot-identity F5 check"
        );

        // NEGATIVE: reproduce the child-navigation F5 signature at this non-root node and prove
        // ev_consistency catches it.
        let n = good.hands.len();
        let na = good.actions.len();
        let mut buggy = good.clone();
        let mut any_shift = false;
        for a in 0..na {
            let shift = action_amount(&good.actions[a]);
            if shift == 0.0 {
                continue;
            }
            any_shift = true;
            for h in 0..n {
                buggy.ev_per_action[h + a * n] += shift;
            }
        }
        assert!(any_shift, "the test node must offer a bet/raise action to shift");
        // the child-navigation shift breaks the DOT IDENTITY (ev[h] unchanged, ev_per_action
        // shifted), which is the HARD F5 gate the navigator relies on at every displayed node
        let (buggy_ok, buggy_notes) = ev_dot_consistency(&buggy);
        assert!(
            !buggy_ok,
            "child-navigated (offset-shifted) F5 read at a non-root node MUST fail the \
             dot-identity check; it passed, so the hard F5 gate lacks teeth. notes={buggy_notes:?}"
        );
    }

    /// The weighted-villain law, hand-verified at the data layer: enumerating vs
    /// "A:wa, B:wb" must equal (wa*eq_vs_A + wb*eq_vs_B) / (wa+wb) composed from the two
    /// single-villain enumerations, and the all-weights-1.0 case must equal the plain
    /// average (the degenerate-case proof that the weighting changed nothing for the
    /// unweighted ranges every prior cross-check ran on).
    #[test]
    fn enumerate_equity_weights_the_villain_range() {
        let hero = parse_hole("KsKh").unwrap();
        let board = vec![
            parse_card("9h").unwrap(),
            parse_card("7d").unwrap(),
            parse_card("2c").unwrap(),
        ];
        let (eq_aa, n_aa) = enumerate_equity(&hero, &board, "AsAd").unwrap();
        let (eq_qq, n_qq) = enumerate_equity(&hero, &board, "QsQd").unwrap();
        assert_eq!((n_aa, n_qq), (1, 1));
        assert!(eq_aa < 0.25 && eq_qq > 0.75, "KK is behind AA ({eq_aa}) and ahead of QQ ({eq_qq})");

        // degenerate case: weights 1.0 == the plain average
        let (eq_even, n_even) = enumerate_equity(&hero, &board, "AsAd,QsQd").unwrap();
        assert_eq!(n_even, 2);
        let plain = (eq_aa + eq_qq) / 2.0;
        assert!((eq_even - plain).abs() < 1e-4, "unweighted: {eq_even} vs plain {plain}");

        // weighted case: AA at 0.2 must pull the average toward the QQ matchup, exactly
        let (eq_w, n_w) = enumerate_equity(&hero, &board, "AsAd:0.2,QsQd").unwrap();
        assert_eq!(n_w, 2);
        let expect = (0.2 * eq_aa + 1.0 * eq_qq) / 1.2;
        assert!((eq_w - expect).abs() < 1e-4, "weighted: {eq_w} vs composed {expect}");
    }

    #[test]
    fn normalized_rows_pass() {
        let (ok, err) = strategy_normalized(&node(vec![0.4, 1.0, 0.6, 0.0]));
        assert!(ok, "well-formed rows must pass (max err {err})");
    }

    #[test]
    fn normalization_violation_fires() {
        // hand 0 sums to 0.9: must fail
        let (ok, err) = strategy_normalized(&node(vec![0.4, 1.0, 0.5, 0.0]));
        assert!(!ok);
        assert!((err - 0.1).abs() < 1e-6);
    }

    #[test]
    fn nan_row_fires_not_skips() {
        // NaN fails the `sum > 1e-3` filter (NaN comparisons are false), so without the
        // explicit finiteness check this row would be silently SKIPPED and pass
        let (ok, _) = strategy_normalized(&node(vec![f32::NAN, 1.0, 0.5, 0.0]));
        assert!(!ok, "a NaN probability must fail, never skip");
    }

    #[test]
    fn zero_sum_violation_fires() {
        let bad = ZeroSumCheck {
            avg_ev_oop: 55.0,
            avg_ev_ip: 48.0,
            pot_chips: 100.0,
            residual_chips: 3.0, // a 3-chip leak on a 100-chip pot
        };
        assert!(!zero_sum_ok(&bad, 0.1));
        let good = ZeroSumCheck {
            avg_ev_oop: 52.0,
            avg_ev_ip: 48.005,
            pot_chips: 100.0,
            residual_chips: 0.005,
        };
        assert!(zero_sum_ok(&good, 0.1));
    }

    #[test]
    fn best_response_violation_fires() {
        // recomputed exploitability above the convergence ceiling: fail
        let above_target = BestResponseCheck {
            recomputed_chips: 0.9,
            stored_chips: 0.45,
            target_chips: 0.5,
        };
        assert!(!best_response_ok(&above_target, 0.10, 0.125));
        // recomputed fine vs target but drifted far from the recorded figure: fail
        let drifted = BestResponseCheck {
            recomputed_chips: 0.10,
            stored_chips: 0.45,
            target_chips: 0.5,
        };
        assert!(!best_response_ok(&drifted, 0.10, 0.125));
        // within ceiling and drift: pass
        let good = BestResponseCheck {
            recomputed_chips: 0.42,
            stored_chips: 0.45,
            target_chips: 0.5,
        };
        assert!(best_response_ok(&good, 0.10, 0.125));
    }
}
