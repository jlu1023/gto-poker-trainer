//! Phase 4 trainer grading: judge a committed action against the solved root strategy.
//!
//! GTO is mixed, so the verdict is never a naive right/wrong. Two numbers drive it:
//!   - frequency: how often the solver takes the chosen action with this exact combo,
//!   - EV loss:   EV(best action) - EV(chosen action), in chips and as a fraction of pot.
//! Verdict precedence (D32): Correct (loss <= eps) > FineMix (freq >= floor AND loss <= minor
//! max) > MinorLeak (loss <= minor max) > ClearLeak. Bands live in `gto_config::TrainerConfig`.
//!
//! Pure functions over `NodeStrategy`; no IO, no solver dependency, fully unit-tested.

use crate::NodeStrategy;
use anyhow::{anyhow, bail, Result};
use gto_config::TrainerConfig;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Correct,
    FineMix,
    MinorLeak,
    ClearLeak,
}

/// The graded outcome for one (hand, action) commitment.
#[derive(Debug, Clone, Serialize)]
pub struct Grade {
    pub frequency: f32,
    pub ev_chosen: f32,
    pub ev_best: f32,
    pub ev_loss_chips: f32,
    pub ev_loss_pct_pot: f32,
    pub best_action_idx: usize,
    pub verdict: Verdict,
}

const RANKS: &str = "23456789TJQKA";
const SUITS: &str = "cdhs"; // index doubles as the suit tiebreak (s > h > d > c)

/// Normalize a 4-char hand label to canonical card order (rank desc, suit desc), so
/// "KhAh" == "AhKh" and case is forgiven. Returns None on malformed input.
pub fn normalize_hand(hand: &str) -> Option<String> {
    let chars: Vec<char> = hand.trim().chars().collect();
    if chars.len() != 4 {
        return None;
    }
    let parse = |r: char, s: char| -> Option<(usize, usize)> {
        Some((
            RANKS.find(r.to_ascii_uppercase())?,
            SUITS.find(s.to_ascii_lowercase())?,
        ))
    };
    let a = parse(chars[0], chars[1])?;
    let b = parse(chars[2], chars[3])?;
    if a == b {
        return None; // same card twice
    }
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    let fmt = |(ri, si): (usize, usize)| {
        format!(
            "{}{}",
            RANKS.as_bytes()[ri] as char,
            SUITS.as_bytes()[si] as char
        )
    };
    Some(format!("{}{}", fmt(hi), fmt(lo)))
}

/// Locate a specific combo in the node's hand list, order- and case-insensitive.
/// None = the combo is not in the solved root (board-blocked or not in the range).
pub fn find_hand(node: &NodeStrategy, hand: &str) -> Option<usize> {
    let want = normalize_hand(hand)?;
    node.hands
        .iter()
        .position(|h| normalize_hand(h).as_deref() == Some(want.as_str()))
}

/// Action families for nearest-mapping: wagers map to the closest wager by amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionKind {
    Passive, // Check / Call
    Wager(u32), // Bet / Raise / AllIn with chip amount
    Fold,
}

fn parse_action_label(label: &str) -> Option<ActionKind> {
    let l = label.trim();
    if l.eq_ignore_ascii_case("check") || l.eq_ignore_ascii_case("call") {
        return Some(ActionKind::Passive);
    }
    if l.eq_ignore_ascii_case("fold") {
        return Some(ActionKind::Fold);
    }
    let open = l.find('(')?;
    let close = l.rfind(')')?;
    let kind = &l[..open];
    let amount: u32 = l[open + 1..close].trim().parse().ok()?;
    match kind.to_ascii_lowercase().as_str() {
        "bet" | "raise" | "allin" | "all-in" => Some(ActionKind::Wager(amount)),
        _ => None,
    }
}

/// Resolve a requested action label against the solved action set. Exact (case-insensitive)
/// match wins. Otherwise an off-tree wager maps to the nearest wager by chip amount, and a
/// passive request maps to the passive action; the original label is returned so the UI can
/// surface "mapped from X". None = nothing comparable exists (the caller errors with the
/// available set).
pub fn map_action(actions: &[String], requested: &str) -> Option<(usize, Option<String>)> {
    if let Some(i) = actions
        .iter()
        .position(|a| a.eq_ignore_ascii_case(requested.trim()))
    {
        return Some((i, None));
    }
    let want = parse_action_label(requested)?;
    let mut best: Option<(usize, u32)> = None; // (index, distance)
    for (i, a) in actions.iter().enumerate() {
        let have = parse_action_label(a)?;
        let dist = match (want, have) {
            (ActionKind::Wager(w), ActionKind::Wager(h)) => w.abs_diff(h),
            (ActionKind::Passive, ActionKind::Passive) => 0,
            (ActionKind::Fold, ActionKind::Fold) => 0,
            _ => continue,
        };
        if best.map_or(true, |(_, d)| dist < d) {
            best = Some((i, dist));
        }
    }
    best.map(|(i, _)| (i, Some(requested.trim().to_string())))
}

/// Grade a committed action for one hand at a node with per-action EV filled.
pub fn grade(
    node: &NodeStrategy,
    hand_idx: usize,
    action_idx: usize,
    pot_chips: f32,
    bands: &TrainerConfig,
) -> Result<Grade> {
    let n = node.hands.len();
    let num_actions = node.actions.len();
    if hand_idx >= n || action_idx >= num_actions {
        bail!("hand/action index out of range");
    }
    if node.ev_per_action.len() != n * num_actions {
        bail!("node lacks per-action EV (len {} != {})", node.ev_per_action.len(), n * num_actions);
    }
    if pot_chips <= 0.0 {
        bail!("pot must be positive");
    }

    let frequency = node.strategy[hand_idx + action_idx * n];
    let (best_action_idx, ev_best) = (0..num_actions)
        .map(|a| (a, node.ev_per_action[hand_idx + a * n]))
        .filter(|(_, e)| e.is_finite())
        .max_by(|x, y| x.1.total_cmp(&y.1))
        .ok_or_else(|| anyhow!("no finite per-action EV for this hand"))?;
    let ev_chosen = node.ev_per_action[hand_idx + action_idx * n];
    if !ev_chosen.is_finite() {
        bail!("chosen action has no readable EV (terminal/chance child)");
    }

    let ev_loss_chips = (ev_best - ev_chosen).max(0.0);
    let ev_loss_pct_pot = ev_loss_chips / pot_chips;

    let verdict = if ev_loss_pct_pot <= bands.correct_eps_pct_pot as f32 {
        Verdict::Correct
    } else if frequency >= bands.mix_freq_floor as f32
        && ev_loss_pct_pot <= bands.minor_leak_max_pct_pot as f32
    {
        Verdict::FineMix
    } else if ev_loss_pct_pot <= bands.minor_leak_max_pct_pot as f32 {
        Verdict::MinorLeak
    } else {
        Verdict::ClearLeak
    };

    Ok(Grade {
        frequency,
        ev_chosen,
        ev_best,
        ev_loss_chips,
        ev_loss_pct_pot,
        best_action_idx,
        verdict,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic solved root: 3 hands x 3 actions, pot 100 chips.
    /// Layout reminder: strategy/ev_per_action are indexed [hand + action * n].
    fn node() -> NodeStrategy {
        let hands = vec!["AhKh".into(), "2c2d".into(), "7s7d".into()];
        let actions = vec!["Check".into(), "Bet(33)".into(), "Bet(75)".into()];
        let n = hands.len();
        let mut strategy = vec![0.0f32; 9];
        let mut epa = vec![0.0f32; 9];
        // AhKh: mixes everything; EVs within 0.5 chips (0.5% pot) of each other
        // 2c2d: mostly checks; Bet(33) is a 15% mix at 1.0% pot loss; Bet(75) loses 5% pot
        // 7s7d: pure check; Bet(33) loses 1.5% pot at 3% frequency (below the 10% floor)
        let put = |v: &mut Vec<f32>, h: usize, a: usize, x: f32| v[h + a * n] = x;
        put(&mut strategy, 0, 0, 0.30); put(&mut strategy, 0, 1, 0.60); put(&mut strategy, 0, 2, 0.10);
        put(&mut strategy, 1, 0, 0.84); put(&mut strategy, 1, 1, 0.15); put(&mut strategy, 1, 2, 0.01);
        put(&mut strategy, 2, 0, 0.97); put(&mut strategy, 2, 1, 0.03); put(&mut strategy, 2, 2, 0.00);
        put(&mut epa, 0, 0, 240.0); put(&mut epa, 0, 1, 240.3); put(&mut epa, 0, 2, 239.8);
        put(&mut epa, 1, 0, 100.0); put(&mut epa, 1, 1, 99.0);  put(&mut epa, 1, 2, 95.0);
        put(&mut epa, 2, 0, 150.0); put(&mut epa, 2, 1, 148.5); put(&mut epa, 2, 2, 140.0);
        NodeStrategy {
            player: 0,
            actions,
            hands,
            strategy,
            ev: vec![240.2, 99.8, 149.9],
            equity: vec![0.66, 0.48, 0.55],
            ev_per_action: epa,
        }
    }

    fn bands() -> TrainerConfig {
        // Pin the test bands explicitly so these scenarios exercise the band LOGIC at a known
        // eps, decoupled from the production default (now the tight 0.05% solve target).
        TrainerConfig { correct_eps_pct_pot: 0.005, minor_leak_max_pct_pot: 0.02, mix_freq_floor: 0.10 }
    }

    const POT: f32 = 100.0;

    #[test]
    fn exact_best_is_correct() {
        let g = grade(&node(), 0, 1, POT, &bands()).unwrap(); // AhKh Bet(33), the EV max
        assert_eq!(g.verdict, Verdict::Correct);
        assert_eq!(g.best_action_idx, 1);
        assert_eq!(g.ev_loss_chips, 0.0);
        assert!((g.frequency - 0.60).abs() < 1e-6);
    }

    #[test]
    fn within_epsilon_is_correct_even_at_low_frequency() {
        // AhKh Bet(75): only 10% frequency, but EV is 0.5 chips (= eps exactly) off best
        let g = grade(&node(), 0, 2, POT, &bands()).unwrap();
        assert_eq!(g.verdict, Verdict::Correct);
        assert!((g.ev_loss_pct_pot - 0.005).abs() < 1e-6);
    }

    #[test]
    fn mixed_action_with_small_loss_is_fine_mix() {
        // 2c2d Bet(33): 15% frequency (>= floor), 1% pot loss (<= minor max, > eps)
        let g = grade(&node(), 1, 1, POT, &bands()).unwrap();
        assert_eq!(g.verdict, Verdict::FineMix);
        assert!((g.ev_loss_pct_pot - 0.01).abs() < 1e-6);
    }

    #[test]
    fn low_frequency_moderate_loss_is_minor_leak() {
        // 7s7d Bet(33): 3% frequency (< floor), 1.5% pot loss (<= minor max)
        let g = grade(&node(), 2, 1, POT, &bands()).unwrap();
        assert_eq!(g.verdict, Verdict::MinorLeak);
    }

    #[test]
    fn big_loss_is_clear_leak_regardless() {
        // 2c2d Bet(75): 5% pot loss
        let g = grade(&node(), 1, 2, POT, &bands()).unwrap();
        assert_eq!(g.verdict, Verdict::ClearLeak);
        assert!((g.ev_loss_chips - 5.0).abs() < 1e-6);
    }

    #[test]
    fn hand_lookup_is_order_and_case_insensitive() {
        let n = node();
        assert_eq!(find_hand(&n, "AhKh"), Some(0));
        assert_eq!(find_hand(&n, "KhAh"), Some(0)); // reversed order
        assert_eq!(find_hand(&n, "khah"), Some(0)); // case
        assert_eq!(find_hand(&n, "2d2c"), Some(1)); // suit order normalized
        assert_eq!(find_hand(&n, "AsKs"), None); // not present (blocked / out of range)
        assert_eq!(find_hand(&n, "AhAh"), None); // same card twice = malformed
        assert_eq!(find_hand(&n, "Ah"), None); // malformed
    }

    #[test]
    fn off_tree_wager_maps_to_nearest_with_notice() {
        let acts = node().actions;
        // Bet(50): |50-33| = 17 beats |50-75| = 25
        let (i, mapped) = map_action(&acts, "Bet(50)").unwrap();
        assert_eq!(i, 1);
        assert_eq!(mapped.as_deref(), Some("Bet(50)"));
        // AllIn(950) is a wager too: nearest wager is Bet(75)
        let (i, mapped) = map_action(&acts, "AllIn(950)").unwrap();
        assert_eq!(i, 2);
        assert!(mapped.is_some());
        // exact match (case-insensitive): no notice
        let (i, mapped) = map_action(&acts, "bet(33)").unwrap();
        assert_eq!(i, 1);
        assert!(mapped.is_none());
        // Fold does not exist at this root and has no comparable action
        assert!(map_action(&acts, "Fold").is_none());
    }

    #[test]
    fn missing_per_action_ev_is_an_error() {
        let mut n = node();
        n.ev_per_action.clear();
        assert!(grade(&n, 0, 0, POT, &bands()).is_err());
    }

    #[test]
    fn band_boundaries_are_inclusive() {
        // loss exactly at minor max (2% pot) with frequency below floor -> MinorLeak not ClearLeak
        let mut n = node();
        n.ev_per_action[2 + 1 * 3] = 148.0; // 7s7d Bet(33): loss 2.0 chips = 2% pot exactly
        let g = grade(&n, 2, 1, POT, &bands()).unwrap();
        assert_eq!(g.verdict, Verdict::MinorLeak);
    }
}
