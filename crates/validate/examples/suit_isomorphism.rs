//! Suit-isomorphism invariant: two boards identical under a suit relabeling that preserves
//! the board's suit structure are the SAME strategic spot and must produce matching
//! strategies. This is a free consequence of the game's structure and exercises the whole
//! board-handling path (card parsing, range construction, solving) end to end.
//!
//! Permutation used here: the transposition h <-> c (s and d fixed) on the rainbow board
//! Ks7h2c, giving Ks7c2h. Why it preserves structure: the board is rainbow (three distinct
//! suits), and a suit bijection applied to BOTH the board and the hole cards maps every
//! card-removal/flush relationship onto itself; the authored ranges are class-based (no
//! suit-specific combos), so the permuted spot uses the identical range strings. A board
//! with paired suits would constrain which permutations are legal; a flush-bearing structure
//! is NOT isomorphic to a rainbow one and must never be asserted equal.
//!
//! Both spots are solved fresh (engine-level; nothing is stored to the matrix), then the
//! roots are compared under the hand relabeling. The negative control repeats the comparison
//! WITHOUT relabeling the hands: suit-board interactions (e.g. backdoor-flush combos) must
//! then disagree, proving the comparison is sensitive to the relabeling and not vacuous.
//!
//! Run from the repo root:  cargo run --release -p gto-validate --example suit_isomorphism

use anyhow::{anyhow, Context, Result};
use gto_config::Config;
use gto_engine::{solve_spot, Board, NodeStrategy, RakeSpec, Spot, Street};
use gto_preflop::{PreflopCharts, PreflopContext, PreflopNode};
use std::collections::HashMap;

const DEPTH_BB: u32 = 50; // cheaper tree than 100bb; the invariant is depth-independent

/// sigma: h <-> c, s and d fixed.
fn sigma_suit(s: char) -> char {
    match s {
        'h' => 'c',
        'c' => 'h',
        other => other,
    }
}

fn sigma_card(card: &str) -> String {
    let mut chars = card.chars();
    let rank = chars.next().unwrap();
    let suit = chars.next().unwrap();
    format!("{rank}{}", sigma_suit(suit))
}

/// Canonical form of a two-card label so "7h7c" and "7c7h" key identically: sort the two
/// cards by (rank index desc, suit index desc), matching the engine's label conventions.
fn canon_hand(label: &str) -> String {
    const RANKS: &str = "AKQJT98765432";
    const SUITS: &str = "shdc";
    let c1 = &label[0..2];
    let c2 = &label[2..4];
    let key = |c: &str| {
        let r = RANKS.find(c.chars().next().unwrap()).unwrap_or(99);
        let s = SUITS.find(c.chars().nth(1).unwrap()).unwrap_or(99);
        (r, s)
    };
    if key(c1) <= key(c2) {
        format!("{c1}{c2}")
    } else {
        format!("{c2}{c1}")
    }
}

fn build_spot(charts: &PreflopCharts, flop: [&str; 3]) -> Result<Spot> {
    let (oop_range, ip_range) = charts.ranges_for(&PreflopContext {
        stack_depth_bb: DEPTH_BB,
        node: PreflopNode::SingleRaisedPot,
    })?;
    Ok(Spot {
        stack_depth_bb: DEPTH_BB,
        initial_street: Street::Flop,
        board: Board {
            flop: [flop[0].into(), flop[1].into(), flop[2].into()],
            turn: None,
            river: None,
        },
        pot_bb: 5.0,
        effective_stack_bb: DEPTH_BB as f64 - 2.5,
        oop_range,
        ip_range,
        rake: RakeSpec::default(),
    })
}

struct Comparison {
    matched_hands: usize,
    /// Hands present in A whose mapped form does not exist in B. Under the isomorphism
    /// mapping this must be 0 (a hard error); under the identity negative control it is
    /// itself evidence of difference (hand EXISTENCE is suit-sensitive: a hand blocked by
    /// one board's cards exists on the other).
    missing_in_b: usize,
    max_ev_diff: f32,
    max_equity_diff: f32,
    max_strategy_diff: f32,
    max_agg_freq_diff: f32,
}

/// Compare root A to root B with hand mapping `map_hand` (relabeling or identity).
fn compare(
    a: &NodeStrategy,
    b: &NodeStrategy,
    allow_missing: bool,
    map_hand: impl Fn(&str) -> String,
) -> Result<Comparison> {
    let b_index: HashMap<String, usize> = b
        .hands
        .iter()
        .enumerate()
        .map(|(i, h)| (canon_hand(h), i))
        .collect();
    let n_a = a.hands.len();
    let n_b = b.hands.len();
    let num_actions = a.actions.len();
    if b.actions != a.actions {
        return Err(anyhow!("action sets differ: {:?} vs {:?}", a.actions, b.actions));
    }

    let mut c = Comparison {
        matched_hands: 0,
        missing_in_b: 0,
        max_ev_diff: 0.0,
        max_equity_diff: 0.0,
        max_strategy_diff: 0.0,
        max_agg_freq_diff: 0.0,
    };
    let mut agg_a = vec![0.0f64; num_actions];
    let mut agg_b = vec![0.0f64; num_actions];

    for (i, hand) in a.hands.iter().enumerate() {
        let mapped = canon_hand(&map_hand(hand));
        let Some(&j) = b_index.get(&mapped) else {
            if allow_missing {
                c.missing_in_b += 1;
                continue;
            }
            return Err(anyhow!("hand {hand} -> {mapped} not found in the other root"));
        };
        c.matched_hands += 1;
        c.max_ev_diff = c.max_ev_diff.max((a.ev[i] - b.ev[j]).abs());
        c.max_equity_diff = c.max_equity_diff.max((a.equity[i] - b.equity[j]).abs());
        for act in 0..num_actions {
            let pa = a.strategy[i + act * n_a];
            let pb = b.strategy[j + act * n_b];
            c.max_strategy_diff = c.max_strategy_diff.max((pa - pb).abs());
            agg_a[act] += pa as f64;
            agg_b[act] += pb as f64;
        }
    }
    for act in 0..num_actions {
        let fa = agg_a[act] / c.matched_hands as f64;
        let fb = agg_b[act] / c.matched_hands as f64;
        c.max_agg_freq_diff = c.max_agg_freq_diff.max((fa - fb).abs() as f32);
    }
    Ok(c)
}

fn main() -> Result<()> {
    let cfg = Config::load("phase1.toml").context("loading phase1.toml")?;
    let charts = PreflopCharts::load_embedded()?;

    let board_a = ["Ks", "7h", "2c"];
    let board_b = ["Ks", "7c", "2h"]; // sigma(board_a)
    println!("solving A = {} ({}bb, fresh, NOT stored)...", board_a.concat(), DEPTH_BB);
    let a = solve_spot(&build_spot(&charts, board_a)?, &cfg.solve)?;
    println!(
        "  A: exploit {:.4}% pot in {} iters",
        a.exploitability_pct_pot * 100.0,
        a.iterations_run
    );
    println!("solving B = sigma(A) = {} (h<->c, fresh, NOT stored)...", board_b.concat());
    let b = solve_spot(&build_spot(&charts, board_b)?, &cfg.solve)?;
    println!(
        "  B: exploit {:.4}% pot in {} iters",
        b.exploitability_pct_pot * 100.0,
        b.iterations_run
    );

    let pot_chips = (a.spot.pot_bb * cfg.solve.chip_per_bb as f64) as f32;

    // isomorphism: compare under the relabeling sigma (every hand MUST map; missing = error)
    let iso = compare(&a.root, &b.root, false, |h| {
        format!("{}{}", sigma_card(&h[0..2]), sigma_card(&h[2..4]))
    })?;
    println!(
        "\nrelabeled comparison ({} hands matched, {} missing):\n  max |EV diff|       = {:.4} chips ({:.4}% pot)\n  max |equity diff|   = {:.6}\n  max |aggregate freq diff| = {:.5}\n  max |per-hand strategy diff| = {:.4} (informational: mixed rows are run-unstable)",
        iso.matched_hands,
        iso.missing_in_b,
        iso.max_ev_diff,
        iso.max_ev_diff / pot_chips * 100.0,
        iso.max_equity_diff,
        iso.max_agg_freq_diff,
        iso.max_strategy_diff
    );

    // negative control: the identity mapping must show clear suit-sensitivity - hands that
    // exist on one board but are blocked on the other, and equity gaps on the survivors
    let neg = compare(&a.root, &b.root, true, |h| h.to_string())?;
    println!(
        "\nnegative control (NO relabeling, same boards):\n  hands missing in B (board-blocked) = {}\n  max |EV diff|       = {:.4} chips\n  max |equity diff|   = {:.6}\n  max |per-hand strategy diff| = {:.4}",
        neg.missing_in_b, neg.max_ev_diff, neg.max_equity_diff, neg.max_strategy_diff
    );

    // assertions: equity at the root is profile-independent card math, so relabeled equities
    // must agree to numerical precision; EVs of two independently converged ~0.4%-pot solves
    // agree to the convergence scale; aggregate action frequencies likewise. The negative
    // control must show clear separation (blocked hands and/or equity gaps) or the
    // comparison proves nothing.
    let mut ok = true;
    let checks: [(&str, bool); 4] = [
        (
            "relabeled equity matches (<= 1e-3)",
            iso.max_equity_diff <= 1e-3,
        ),
        (
            "relabeled EV within convergence scale (<= 2% pot)",
            iso.max_ev_diff <= 0.02 * pot_chips,
        ),
        (
            "relabeled aggregate action frequencies match (<= 0.02)",
            iso.max_agg_freq_diff <= 0.02,
        ),
        (
            "negative control separates (blocked hands present AND equity gap > 5x relabeled)",
            neg.missing_in_b > 0 && neg.max_equity_diff > 5.0 * iso.max_equity_diff.max(1e-6),
        ),
    ];
    println!();
    for (name, pass) in checks {
        println!("  {}  {name}", if pass { "PASS" } else { "FAIL" });
        ok &= pass;
    }

    println!("\n{}", if ok { "SUIT-ISOMORPHISM OK" } else { "SUIT-ISOMORPHISM FAILED" });
    std::process::exit(if ok { 0 } else { 1 });
}
