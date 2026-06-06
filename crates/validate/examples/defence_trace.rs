//! Per-hand defence trace at a bet-facing flop node: the mechanistic acceptance tool for
//! tree/range changes (operator amendment to the srp-v2 plan). For each hand class bucket
//! it prints reach mass, fold/call/RAISE frequencies, and average equity, plus the list of
//! hands taking the raise line - so "the number moved" can be attributed to the MECHANISM
//! (e.g. capped-top defends moving fold -> check-raise) rather than trusted blindly.
//!
//! Read-only over the cached matrix; never solves.
//!
//! Run from the repo root:
//!   cargo run --release -p gto-validate --example defence_trace [spot-key] [bet-chips] [path]
//! Defaults: the srp-v2 100bb 9c5d3h spot, Bet(33), path [Check, Bet(bet-chips)].
//!
//! The optional 3rd arg is a path spec for non-SRP node shapes (tokens, left to right:
//! `X` = Check, `B<chips>` = Bet(chips)); without it the original two-arg behavior is
//! byte-identical. 3bp examples (the BB is the OOP aggressor, so the c-bet is at the ROOT):
//!   defence_trace 100bb_9c5d3h_flop_3bp-v1 132 B132    # BTN (capped caller) vs the c-bet
//!   defence_trace 100bb_9c5d3h_flop_3bp-v1 132 XB132   # BB vs the stab after check-through

use anyhow::{anyhow, bail, Context, Result};
use gto_config::Config;
use gto_engine::{query_node, read_node_weights, NodeQuery, SolverAction, Step};
use gto_store::load_solution;

const RANKS: &str = "23456789TJQKA";

fn rank_idx(c: char) -> i32 {
    RANKS.find(c).map(|i| i as i32).unwrap_or(-1)
}

/// Straight-draw-ish detector: >=4 distinct ranks in a 5-rank window using both hole ranks
/// plus the board (counts made straights too; fine for bucketing).
fn straightish(h0: i32, h1: i32, board: &[i32]) -> bool {
    let mut ranks: Vec<i32> = vec![h0, h1];
    ranks.extend_from_slice(board);
    ranks.sort_unstable();
    ranks.dedup();
    if ranks.contains(&12) {
        ranks.insert(0, -1); // ace plays low
    }
    for lo in -1..=8 {
        let in_window = ranks.iter().filter(|&&r| r >= lo && r < lo + 5).count();
        let hole_in = (h0 >= lo && h0 < lo + 5) || (h1 >= lo && h1 < lo + 5);
        if in_window >= 4 && hole_in {
            return true;
        }
    }
    false
}

fn bucket(hand: &str, board: &[i32]) -> &'static str {
    let r0 = rank_idx(hand.chars().next().unwrap());
    let r1 = rank_idx(hand.chars().nth(2).unwrap());
    let top = *board.iter().max().unwrap();
    let on_board = |r: i32| board.contains(&r);
    if r0 == r1 {
        if on_board(r0) {
            return "set";
        }
        return if r0 > top { "overpair" } else { "pp-under-top" };
    }
    if on_board(r0) || on_board(r1) {
        let paired = if on_board(r0) { r0 } else { r1 };
        return if paired == top { "top-pair" } else { "mid/low-pair" };
    }
    if straightish(r0, r1, board) {
        return "straight-draw";
    }
    if r0 > top && r1 > top {
        return "two-overcards";
    }
    "air/other"
}

/// Parse a path spec ("XB132" -> [Check, Bet(132)]). Tokens: `X` = Check, `B<chips>` = Bet.
fn parse_path_spec(spec: &str) -> Result<Vec<Step>> {
    let mut steps = Vec::new();
    let mut chars = spec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            'X' | 'x' => steps.push(Step::Action(SolverAction::Check)),
            'B' | 'b' => {
                let mut digits = String::new();
                while let Some(d) = chars.peek().filter(|d| d.is_ascii_digit()) {
                    digits.push(*d);
                    chars.next();
                }
                let chips: i32 =
                    digits.parse().map_err(|_| anyhow!("path spec '{spec}': B needs chips"))?;
                steps.push(Step::Action(SolverAction::Bet(chips)));
            }
            other => bail!("path spec '{spec}': unknown token '{other}' (use X or B<chips>)"),
        }
    }
    if steps.is_empty() {
        bail!("path spec '{spec}' is empty");
    }
    Ok(steps)
}

fn main() -> Result<()> {
    let cfg = Config::load("phase1.toml").context("loading phase1.toml")?;
    let default_key = format!("100bb_9c5d3h_flop_{}", cfg.solve.bet_tree.bet_tree_id);
    let key = std::env::args().nth(1).unwrap_or(default_key);
    let bet_chips: i32 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(33);

    let mut solution = load_solution(&key, &cfg)?;
    let board_ranks: Vec<i32> = solution
        .spot
        .board
        .flop
        .iter()
        .map(|c| rank_idx(c.chars().next().unwrap()))
        .collect();

    // default path = the SRP acceptance shape [Check, Bet(bet_chips)]; a 3rd arg overrides
    let path = match std::env::args().nth(3) {
        Some(spec) => parse_path_spec(&spec)?,
        None => vec![
            Step::Action(SolverAction::Check),
            Step::Action(SolverAction::Bet(bet_chips)),
        ],
    };
    let path_label: String = path
        .iter()
        .map(|s| match s {
            Step::Action(SolverAction::Check) => "Check".to_string(),
            Step::Action(SolverAction::Bet(c)) => format!("Bet({c})"),
            other => format!("{other:?}"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let q = NodeQuery { path };
    let node = query_node(&mut solution, &q)?;
    let w = read_node_weights(&mut solution.game, &q)?;
    let n = node.hands.len();
    let fold_idx = node
        .actions
        .iter()
        .position(|a| a == "Fold")
        .ok_or_else(|| anyhow!("no Fold at this node"))?;
    let raise_idxs: Vec<usize> = node
        .actions
        .iter()
        .enumerate()
        .filter(|(_, a)| a.starts_with("Raise") || a.starts_with("AllIn"))
        .map(|(i, _)| i)
        .collect();

    println!("=== {key}  node [{path_label}]  player {} (0=BB) ===", node.player);
    println!("  actions: {:?}", node.actions);

    // aggregates
    let total_w: f64 = w.iter().map(|&x| x as f64).sum();
    let freq = |idx: usize| -> f64 {
        (0..n).map(|h| w[h] as f64 * node.strategy[h + idx * n] as f64).sum::<f64>() / total_w
    };
    let fold_f = freq(fold_idx);
    let raise_f: f64 = raise_idxs.iter().map(|&i| freq(i)).sum();
    println!(
        "  aggregate: fold {:.1}%  defence {:.1}%  (of which raise {:.1}%)",
        fold_f * 100.0,
        (1.0 - fold_f) * 100.0,
        raise_f * 100.0
    );

    // bucket table
    let buckets = [
        "set", "overpair", "top-pair", "mid/low-pair", "pp-under-top", "straight-draw",
        "two-overcards", "air/other",
    ];
    println!(
        "  {:<14} {:>8} {:>8} {:>8} {:>9}",
        "bucket", "share%", "fold%", "raise%", "avg-eq%"
    );
    for b in buckets {
        let sel: Vec<usize> = (0..n)
            .filter(|&h| bucket(&node.hands[h], &board_ranks) == b)
            .collect();
        if sel.is_empty() {
            println!("  {:<14} {:>8}", b, "-");
            continue;
        }
        let bw: f64 = sel.iter().map(|&h| w[h] as f64).sum();
        let fold: f64 =
            sel.iter().map(|&h| w[h] as f64 * node.strategy[h + fold_idx * n] as f64).sum::<f64>() / bw;
        let raise: f64 = raise_idxs
            .iter()
            .map(|&a| sel.iter().map(|&h| w[h] as f64 * node.strategy[h + a * n] as f64).sum::<f64>())
            .sum::<f64>()
            / bw;
        let eq: f64 = sel.iter().map(|&h| w[h] as f64 * node.equity[h] as f64).sum::<f64>() / bw;
        println!(
            "  {:<14} {:>7.1}% {:>7.1}% {:>7.1}% {:>8.1}%",
            b,
            bw / total_w * 100.0,
            fold * 100.0,
            raise * 100.0,
            eq * 100.0
        );
    }

    // the mechanism list: hands taking the raise line with meaningful frequency
    let mut raisers: Vec<(f32, &str, f32)> = (0..n)
        .filter(|&h| w[h] > 1e-4)
        .filter_map(|h| {
            let r: f32 = raise_idxs.iter().map(|&a| node.strategy[h + a * n]).sum();
            (r >= 0.05).then_some((r, node.hands[h].as_str(), node.equity[h]))
        })
        .collect();
    raisers.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    println!("  hands raising >= 5% ({}):", raisers.len());
    for (r, hand, eq) in raisers.iter().take(20) {
        println!(
            "    {:<6} raise {:>5.1}%  eq {:>5.1}%  [{}]",
            hand,
            r * 100.0,
            eq * 100.0,
            bucket(hand, &board_ranks)
        );
    }
    Ok(())
}
