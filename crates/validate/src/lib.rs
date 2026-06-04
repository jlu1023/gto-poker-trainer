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

use anyhow::{anyhow, bail, Result};
use gto_config::Config;
use gto_engine::{NodeStrategy, Spot, SpotSolution};
use rs_poker::core::{Card, Hand, Rankable, Suit, Value};
use rs_poker::holdem::RangeParser;

/// Outcome of the internal-invariant checks.
#[derive(Debug, Clone, Default)]
pub struct ValidationReport {
    pub exploitability_ok: bool,
    pub strategy_normalized: bool,
    pub max_normalization_error: f32,
    /// Per-action EV checks (Phase 5, audit follow-up P1): dot-product identity + no
    /// dominant unplayed action. True when ev_per_action is absent (nothing to check).
    pub ev_consistency_ok: bool,
    pub notes: Vec<String>,
}

impl ValidationReport {
    /// True when every implemented invariant passed.
    pub fn passed(&self) -> bool {
        self.exploitability_ok && self.strategy_normalized && self.ev_consistency_ok
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

    // 3. per-action EV checks (Phase 5; promotes the manual F5 catch into a permanent gate)
    let pot_chips = (solution.spot.pot_bb * cfg.solve.chip_per_bb as f64) as f32;
    let (ev_ok, ev_notes) = ev_consistency(&solution.root, pot_chips);
    report.ev_consistency_ok = ev_ok;
    report.notes.extend(ev_notes);

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

    let mut max_dot_err = 0.0_f32;
    let mut max_dominance = 0.0_f32;
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

/// Hero equity vs a villain range on a flop, by exhaustive turn+river enumeration with card removal.
/// Returns (equity in 0..1, number of unblocked villain combos averaged over).
fn enumerate_equity(hero: &[Card; 2], board: &[Card], villain_range: &str) -> Result<(f32, usize)> {
    let villains = RangeParser::parse_many(villain_range)
        .map_err(|e| anyhow!("parsing villain range '{villain_range}': {e:?}"))?;

    let full_deck = all_cards();
    let mut base_used: Vec<Card> = board.to_vec();
    base_used.extend_from_slice(hero);

    let mut sum_equity = 0.0f64;
    let mut counted = 0usize;

    for fh in &villains {
        let v0 = fh[0];
        let v1 = fh[1];
        // skip villain combos blocked by the hero's cards or the board
        if v0 == v1 || base_used.contains(&v0) || base_used.contains(&v1) {
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
            sum_equity += (win as f64 + 0.5 * tie as f64) / total as f64;
            counted += 1;
        }
    }

    if counted == 0 {
        bail!("no unblocked villain combos for range '{villain_range}'");
    }
    Ok(((sum_equity / counted as f64) as f32, counted))
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
