//! Node aggregation (D40): operator action rates vs the solver's mixes, with EV cost.
//!
//! D44 boundary (stated wherever this output surfaces): the GTO comparator g(a) is the MEAN
//! OF THE SOLVER'S MIXES FOR THE HANDS THE OPERATOR ACTUALLY HELD - it is range-conditioned,
//! not the node's unconditional GTO frequency. This report measures action-given-range leaks
//! only; it cannot detect range-construction leaks (a skewed arriving range moves g(a) with
//! the selection bias).
//!
//! Significance: Poisson-binomial under the null "operator played GTO on these exact spots":
//! mu = sum_i p_i(a), sigma^2 = sum_i p_i(a)(1 - p_i(a)), z = (count - mu)/sigma. Plain
//! binomial SE is the wrong null for heterogeneous mixes and mishandles near-pure nodes.
//!
//! EV cost primary = sum_i (ev_mix_i - ev_taken_i): the mix-EV loss (ev_mix IS the solver's
//! strategy-weighted dot product). Secondary = sum vs best ("max exploitable"). D45: before
//! any summing/clipping, a played-at-mix-floor action must not beat the mix EV by more than
//! the configured epsilon - a violation is an F5-class reference error and fails loudly.

use anyhow::{bail, Result};
use gto_config::ReviewConfig;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const RANK_ORDER: &str = "23456789TJQKA";

/// One graded decision: the JSONL row and the aggregation input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradedRecord {
    pub schema_version: u32,
    pub hand_id: String,
    pub street: String,
    pub decision_index: u32,
    pub depth_bb: u32,
    pub node_shape: String,
    pub texture: String,
    pub board: [String; 3],
    pub combo: String,
    pub action_taken: String,
    pub actions: Vec<String>,
    /// Solver mix for the exact combo at the node, aligned with `actions`.
    pub p: Vec<f32>,
    /// Strategy-weighted mix EV (chips) = dot(p, ev_actions).
    pub ev_mix: f32,
    /// Per-action EV (chips), aligned with `actions`.
    pub ev_actions: Vec<f32>,
    pub source_file: String,
    pub ingested_at: String,
}

/// Texture bucket for a CANONICAL board (rank desc). ~10 buckets; A-high and paired splits
/// carry the strongest known frequency differences in HU SRP (plan review).
pub fn texture_bucket(board: &[String; 3]) -> &'static str {
    let rank = |c: &String| RANK_ORDER.find(c.as_bytes()[0] as char).unwrap_or(0);
    let r0 = rank(&board[0]); // top (canonical order)
    let r1 = rank(&board[1]);
    let r2 = rank(&board[2]);
    let s: Vec<u8> = board.iter().map(|c| c.as_bytes()[1]).collect();
    let n_suits = {
        let mut u = vec![s[0]];
        for x in &s[1..] {
            if !u.contains(x) {
                u.push(*x);
            }
        }
        u.len()
    };
    let paired = r0 == r1 || r1 == r2;
    let a_high = r0 == 12;
    let broadway_top = (9..=11).contains(&r0); // J, Q, K top

    if paired {
        return if r0 >= 10 { "paired-high" } else { "paired-low" }; // Q+ top = high
    }
    match n_suits {
        1 => "monotone",
        2 => {
            if a_high {
                "twotone-ahigh"
            } else if broadway_top {
                "twotone-broadway"
            } else {
                "twotone-low"
            }
        }
        _ => {
            if a_high {
                "rainbow-ahigh"
            } else if broadway_top {
                "rainbow-broadway"
            } else if r0 - r2 <= 4 {
                "rainbow-mid-connected"
            } else {
                "rainbow-low-disconnected"
            }
        }
    }
}

/// D45: the per-instance EV sanity assertion (F5 regression tripwire through read_node).
/// A played-at-mix-floor action cannot beat the equilibrium mix EV by more than epsilon.
pub fn assert_record_sane(rec: &GradedRecord, pot_chips: f32, cfg: &ReviewConfig) -> Result<()> {
    let Some(taken_idx) = rec.actions.iter().position(|a| a == &rec.action_taken) else {
        bail!("record {}#{}: taken action '{}' not in node actions {:?}",
            rec.hand_id, rec.decision_index, rec.action_taken, rec.actions);
    };
    if rec.p.len() != rec.actions.len() || rec.ev_actions.len() != rec.actions.len() {
        bail!("record {}#{}: vector lengths inconsistent", rec.hand_id, rec.decision_index);
    }
    let eps_chips = (cfg.ev_sanity_eps_pct_pot as f32) * pot_chips;
    let p_taken = rec.p[taken_idx];
    let beat = rec.ev_actions[taken_idx] - rec.ev_mix;
    if p_taken >= cfg.mix_floor as f32 && beat > eps_chips {
        bail!(
            "D45 EV sanity violation on {}#{} ({} {} {}): action '{}' (solver mix {:.1}%) \
             exceeds the mix EV by {:.2} chips (> {:.2} allowed). An F5-class reference-point \
             error has likely re-entered via read_node; refusing to aggregate.",
            rec.hand_id, rec.decision_index, rec.depth_bb, rec.board.concat(), rec.combo,
            rec.action_taken, p_taken * 100.0, beat, eps_chips
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct BoardDrill {
    pub board: String,
    pub n: usize,
    pub taken_counts: Vec<u32>,
    /// Mean solver mix for the hands held on this exact board.
    pub g: Vec<f64>,
    pub ev_cost_mix_bb: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeReport {
    pub depth_bb: u32,
    pub node_shape: String,
    pub texture: String,
    pub n: usize,
    pub actions: Vec<String>,
    pub taken_counts: Vec<u32>,
    /// Operator rate per action.
    pub r: Vec<f64>,
    /// Range-conditioned GTO comparator per action (mean of per-instance mixes; D44).
    pub g: Vec<f64>,
    pub delta: Vec<f64>,
    /// Poisson-binomial z per action.
    pub z: Vec<f64>,
    /// Signed mix-EV cost (bb); negative = beat the mix within noise.
    pub ev_cost_mix_bb: f64,
    /// Displayed cost: clipped at zero ONLY for display (D45 keeps the guard upstream).
    pub displayed_cost_bb: f64,
    /// Sum of vs-best losses (bb): the "max exploitable" upper bound.
    pub ev_cost_best_bb: f64,
    pub ranked: bool,
    pub boards: Vec<BoardDrill>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub total_records: usize,
    pub distinct_hands: usize,
    /// Ranked leaks first (by displayed cost desc), then the rest.
    pub nodes: Vec<NodeReport>,
    pub low_sample_nodes: usize,
    pub total_cost_mix_bb: f64,
    pub total_cost_best_bb: f64,
    /// D44 caveat, shipped WITH the data so every surface carries it.
    pub comparator_caveat: &'static str,
}

pub const COMPARATOR_CAVEAT: &str = "GTO% here is the solver's mix for the hands you actually \
held at this node (range-conditioned). It is NOT the node's unconditional GTO frequency, and \
this report cannot detect range-construction leaks: if your arriving range is already skewed, \
GTO% moves with it.";

/// Aggregate graded records into the leak report. Runs the D45 assertion on every record
/// BEFORE any arithmetic; chip_per_bb converts chips to bb; pot_chips is the root pot (100).
pub fn aggregate(
    records: &[GradedRecord],
    pot_chips: f32,
    chip_per_bb: f64,
    cfg: &ReviewConfig,
) -> Result<Report> {
    for rec in records {
        assert_record_sane(rec, pot_chips, cfg)?;
    }

    let mut groups: BTreeMap<(u32, String, String), Vec<&GradedRecord>> = BTreeMap::new();
    for rec in records {
        groups
            .entry((rec.depth_bb, rec.node_shape.clone(), rec.texture.clone()))
            .or_default()
            .push(rec);
    }

    let mut nodes = Vec::new();
    for ((depth_bb, shape, texture), recs) in groups {
        let actions = recs[0].actions.clone();
        for r in &recs {
            if r.actions != actions {
                bail!(
                    "heterogeneous action sets inside node ({depth_bb}, {shape}, {texture}); \
                     shrunk-tree records must be excluded upstream"
                );
            }
        }
        let n = recs.len();
        let n_act = actions.len();

        let mut taken_counts = vec![0u32; n_act];
        let mut mu = vec![0f64; n_act];
        let mut var = vec![0f64; n_act];
        let mut cost_mix_chips = 0f64;
        let mut cost_best_chips = 0f64;
        let mut by_board: BTreeMap<String, (usize, Vec<u32>, Vec<f64>, f64)> = BTreeMap::new();

        for rec in &recs {
            let taken = rec.actions.iter().position(|a| a == &rec.action_taken).unwrap();
            taken_counts[taken] += 1;
            for a in 0..n_act {
                let p = rec.p[a] as f64;
                mu[a] += p;
                var[a] += p * (1.0 - p);
            }
            let ev_taken = rec.ev_actions[taken] as f64;
            cost_mix_chips += rec.ev_mix as f64 - ev_taken;
            let best = rec.ev_actions.iter().copied().fold(f32::NEG_INFINITY, f32::max) as f64;
            cost_best_chips += best - ev_taken;

            let entry = by_board.entry(rec.board.concat()).or_insert_with(|| {
                (0usize, vec![0u32; n_act], vec![0f64; n_act], 0f64)
            });
            entry.0 += 1;
            entry.1[taken] += 1;
            for a in 0..n_act {
                entry.2[a] += rec.p[a] as f64;
            }
            entry.3 += (rec.ev_mix as f64 - ev_taken) / chip_per_bb;
        }

        let r: Vec<f64> = taken_counts.iter().map(|&c| c as f64 / n as f64).collect();
        let g: Vec<f64> = mu.iter().map(|m| m / n as f64).collect();
        let delta: Vec<f64> = r.iter().zip(&g).map(|(ri, gi)| ri - gi).collect();
        let z: Vec<f64> = (0..n_act)
            .map(|a| {
                if var[a] < 1e-9 {
                    let diff = taken_counts[a] as f64 - mu[a];
                    if diff.abs() < 1e-9 { 0.0 } else { f64::INFINITY * diff.signum() }
                } else {
                    (taken_counts[a] as f64 - mu[a]) / var[a].sqrt()
                }
            })
            .collect();

        let ranked = n >= cfg.min_sample_n as usize
            && z.iter().any(|zi| zi.abs() > cfg.z_threshold);
        let ev_cost_mix_bb = cost_mix_chips / chip_per_bb;
        let boards = by_board
            .into_iter()
            .map(|(board, (bn, counts, gsum, cost))| BoardDrill {
                board,
                n: bn,
                taken_counts: counts,
                g: gsum.iter().map(|x| x / bn as f64).collect(),
                ev_cost_mix_bb: cost,
            })
            .collect();

        nodes.push(NodeReport {
            depth_bb,
            node_shape: shape,
            texture,
            n,
            actions,
            taken_counts,
            r,
            g,
            delta,
            z,
            ev_cost_mix_bb,
            displayed_cost_bb: ev_cost_mix_bb.max(0.0),
            ev_cost_best_bb: cost_best_chips / chip_per_bb,
            ranked,
            boards,
        });
    }

    // ranked leaks first, by displayed cost desc; low-sample/insignificant after
    nodes.sort_by(|a, b| {
        b.ranked
            .cmp(&a.ranked)
            .then(b.displayed_cost_bb.total_cmp(&a.displayed_cost_bb))
    });

    let distinct_hands = {
        let mut ids: Vec<&str> = records.iter().map(|r| r.hand_id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        ids.len()
    };
    let low_sample_nodes = nodes.iter().filter(|nd| !nd.ranked).count();
    let total_cost_mix_bb = nodes.iter().map(|nd| nd.ev_cost_mix_bb).sum();
    let total_cost_best_bb = nodes.iter().map(|nd| nd.ev_cost_best_bb).sum();

    Ok(Report {
        total_records: records.len(),
        distinct_hands,
        nodes,
        low_sample_nodes,
        total_cost_mix_bb,
        total_cost_best_bb,
        comparator_caveat: COMPARATOR_CAVEAT,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(hand: &str, board: [&str; 3], taken: &str, p: [f32; 3], ev: [f32; 3]) -> GradedRecord {
        let p_vec = p.to_vec();
        let ev_mix: f32 = p.iter().zip(ev.iter()).map(|(pi, ei)| pi * ei).sum();
        GradedRecord {
            schema_version: 1,
            hand_id: hand.into(),
            street: "flop".into(),
            decision_index: 0,
            depth_bb: 100,
            node_shape: "bb-root".into(),
            texture: texture_bucket(&[board[0].into(), board[1].into(), board[2].into()]).into(),
            board: [board[0].into(), board[1].into(), board[2].into()],
            combo: "AhKh".into(),
            action_taken: taken.into(),
            actions: vec!["Check".into(), "Bet(33)".into(), "Bet(75)".into()],
            p: p_vec,
            ev_mix,
            ev_actions: ev.to_vec(),
            source_file: "t".into(),
            ingested_at: "t".into(),
        }
    }

    fn cfg() -> ReviewConfig {
        ReviewConfig::default()
    }

    #[test]
    fn texture_buckets_are_exact() {
        let b = |a: &str, b_: &str, c: &str| [a.to_string(), b_.to_string(), c.to_string()];
        assert_eq!(texture_bucket(&b("Ks", "7h", "2c")), "rainbow-broadway");
        assert_eq!(texture_bucket(&b("As", "7h", "2c")), "rainbow-ahigh");
        assert_eq!(texture_bucket(&b("8d", "6s", "3c")), "rainbow-low-disconnected"); // 8-3 span 5 > 4
        assert_eq!(texture_bucket(&b("Kh", "9h", "4h")), "monotone");
        assert_eq!(texture_bucket(&b("Qs", "8s", "5h")), "twotone-broadway");
        assert_eq!(texture_bucket(&b("Ah", "8s", "5h")), "twotone-ahigh");
        assert_eq!(texture_bucket(&b("7h", "7d", "2c")), "paired-low");
        assert_eq!(texture_bucket(&b("Qh", "Qd", "2c")), "paired-high");
        assert_eq!(texture_bucket(&b("6c", "5d", "4h")), "rainbow-mid-connected");
        assert_eq!(texture_bucket(&b("9c", "5d", "3h")), "rainbow-low-disconnected");
    }

    #[test]
    fn hand_computed_node_reconciles_exactly() {
        // 3 instances at one node, mixes chosen so every number is hand-checkable.
        // i1: p=(0.5,0.5,0), took Check.  i2: p=(0.25,0.75,0), took Bet(33).
        // i3: p=(0.75,0.25,0), took Check.
        let records = vec![
            rec("h1", ["Ks", "7h", "2c"], "Check",   [0.50, 0.50, 0.0], [100.0, 100.4, 99.0]),
            rec("h2", ["Ks", "7h", "2c"], "Bet(33)", [0.25, 0.75, 0.0], [200.0, 200.2, 199.0]),
            rec("h3", ["Qd", "9h", "3s"], "Check",   [0.75, 0.25, 0.0], [150.0, 150.3, 149.0]),
        ];
        // h3 is a different texture (Q-high rainbow-broadway same as Ks7h2c? Qd9h3s: top Q ->
        // rainbow-broadway too) -> same node bucket; intended: one node, two boards.
        let report = aggregate(&records, 100.0, 20.0, &cfg()).unwrap();
        assert_eq!(report.nodes.len(), 1);
        let node = &report.nodes[0];
        assert_eq!(node.n, 3);
        // r: Check 2/3, Bet(33) 1/3
        assert!((node.r[0] - 2.0 / 3.0).abs() < 1e-12);
        assert!((node.r[1] - 1.0 / 3.0).abs() < 1e-12);
        // g: Check (0.5+0.25+0.75)/3 = 0.5; Bet33 (0.5+0.75+0.25)/3 = 0.5
        assert!((node.g[0] - 0.5).abs() < 1e-9);
        assert!((node.g[1] - 0.5).abs() < 1e-9);
        // z for Check: mu = 1.5, var = 0.25+0.1875+0.1875 = 0.625, z = (2-1.5)/sqrt(0.625)
        let z_expected = 0.5 / 0.625f64.sqrt();
        assert!((node.z[0] - z_expected).abs() < 1e-9);
        // EV cost (chips): i1 mix=100.2 took Check 100.0 -> 0.2; i2 mix=200.15 took Bet33
        // 200.2 -> -0.05; i3 mix=150.075 took Check 150.0 -> 0.075; total 0.225 chips
        let expected_cost_bb = (0.2 - 0.05 + 0.075) / 20.0;
        assert!((node.ev_cost_mix_bb - expected_cost_bb).abs() < 1e-6,
            "got {} want {}", node.ev_cost_mix_bb, expected_cost_bb);
        // vs best: i1 best 100.4 -> 0.4; i2 best 200.2 -> 0.0; i3 best 150.3 -> 0.3
        assert!((node.ev_cost_best_bb - 0.7 / 20.0).abs() < 1e-6);
        // 3 instances < min_n 10 -> not ranked (the small-sample guard)
        assert!(!node.ranked);
        // drilldown: two boards, 2 + 1 instances
        assert_eq!(node.boards.len(), 2);
    }

    #[test]
    fn min_n_guard_suppresses_tiny_sample_leaks() {
        // 2 instances of an extreme deviation: solver always bets, operator always checks.
        let records = vec![
            rec("h1", ["Ks", "7h", "2c"], "Check", [0.01, 0.99, 0.0], [100.0, 100.3, 99.0]),
            rec("h2", ["Ks", "7h", "2c"], "Check", [0.01, 0.99, 0.0], [100.0, 100.3, 99.0]),
        ];
        let report = aggregate(&records, 100.0, 20.0, &cfg()).unwrap();
        assert!(!report.nodes[0].ranked, "2 instances must never rank as a leak");
    }

    #[test]
    fn d45_assertion_fires_on_reference_shifted_evs() {
        // Corrupted record: the taken action is meaningfully mixed (60%) yet its EV "beats"
        // the mix by 33 chips - the F5 signature. ev_mix deliberately NOT the dot product.
        let mut bad = rec("h1", ["Ks", "7h", "2c"], "Bet(33)",
            [0.40, 0.60, 0.0], [100.0, 135.0, 99.0]);
        bad.ev_mix = 102.0; // a plausible-looking mix EV; Bet(33) at 135 beats it by 33
        let err = aggregate(&[bad], 100.0, 20.0, &cfg()).unwrap_err();
        assert!(err.to_string().contains("D45"), "got: {err}");
    }

    #[test]
    fn near_pure_high_g_does_not_false_fire_z() {
        // 12 instances where GTO is ~pure bet and the operator always bets: var ~ 0, count ==
        // mu -> z = 0, not infinity/noise.
        let records: Vec<_> = (0..12)
            .map(|i| rec(&format!("h{i}"), ["Ks", "7h", "2c"], "Bet(33)",
                [0.0, 1.0, 0.0], [100.0, 101.0, 99.0]))
            .collect();
        let report = aggregate(&records, 100.0, 20.0, &cfg()).unwrap();
        let node = &report.nodes[0];
        assert_eq!(node.z[1], 0.0);
        assert!(!node.ranked);
    }
}
