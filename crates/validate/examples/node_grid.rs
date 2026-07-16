//! Read-only per-class ORACLE for the game-tree navigator's acceptance gate.
//!
//! Loads a CACHED spot, runs the F5 battery over its whole flop layer and REFUSES to emit
//! (non-zero exit) if any flop decision node fails `ev_consistency`, then walks a label path
//! and prints per-169-hand-class reach-weighted strategy / EV / equity as JSON on stdout.
//! `verify_navigator.py` reconciles the live navigator grid against this. Never solves.
//!
//! Run from the repo root:
//!   cargo run --release -p gto-validate --example node_grid <spot-key> [path-spec]
//! Path-spec tokens (left to right): X=Check, C=Call, F=Fold, B<chips>=Bet, R<chips>=Raise,
//! A<chips>=AllIn. Omitted (or "-") = the root itself. Example:
//!   node_grid 100bb_Ks7h2c_flop_srp-v2 XB33R99   # [Check, Bet(33), Raise(99)]

use anyhow::{bail, Context, Result};
use gto_config::Config;
use gto_engine::{read_node_labeled, read_node_weights_labeled};
use gto_store::load_solution;
use gto_validate::flop_node_ev_consistency;
use std::collections::BTreeMap;
use std::iter::Peekable;
use std::str::Chars;

const RANKS: &str = "23456789TJQKA";

/// Consume digits after a sizing token and format the label (e.g. "Bet(33)").
fn sized_label(chars: &mut Peekable<Chars>, name: &str, spec: &str) -> Result<String> {
    let mut digits = String::new();
    while let Some(d) = chars.peek().copied().filter(|d| d.is_ascii_digit()) {
        digits.push(d);
        chars.next();
    }
    if digits.is_empty() {
        bail!("path spec '{spec}': {name} needs chips (e.g. B33)");
    }
    Ok(format!("{name}({digits})"))
}

/// Parse a path spec into ACTION LABELS - the exact strings the /api/node endpoint walks, so
/// the oracle and the endpoint address the node identically.
fn parse_path_labels(spec: &str) -> Result<Vec<String>> {
    if spec == "-" {
        return Ok(Vec::new());
    }
    let mut labels = Vec::new();
    let mut chars = spec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            'X' | 'x' => labels.push("Check".to_string()),
            'C' | 'c' => labels.push("Call".to_string()),
            'F' | 'f' => labels.push("Fold".to_string()),
            'B' | 'b' => labels.push(sized_label(&mut chars, "Bet", spec)?),
            'R' | 'r' => labels.push(sized_label(&mut chars, "Raise", spec)?),
            'A' | 'a' => labels.push(sized_label(&mut chars, "AllIn", spec)?),
            other => bail!("path spec '{spec}': unknown token '{other}' (use X/C/F/B<n>/R<n>/A<n>)"),
        }
    }
    Ok(labels)
}

/// Combo (e.g. "AsKh") -> 169-class label ("AKo", "AKs", "AA"). Higher rank first; `s`/`o` by
/// suitedness. Matches the viz grid's class label so oracle and UI cells align.
fn class_of(combo: &str) -> Result<String> {
    let b = combo.as_bytes();
    if b.len() != 4 {
        bail!("combo '{combo}' is not 4 chars");
    }
    let (r0, s0, r1, s1) = (b[0] as char, b[1] as char, b[2] as char, b[3] as char);
    let i0 = RANKS.find(r0).with_context(|| format!("rank '{r0}'"))?;
    let i1 = RANKS.find(r1).with_context(|| format!("rank '{r1}'"))?;
    let (hi, lo) = if i0 >= i1 { (r0, r1) } else { (r1, r0) };
    Ok(if r0 == r1 {
        format!("{hi}{lo}")
    } else if s0 == s1 {
        format!("{hi}{lo}s")
    } else {
        format!("{hi}{lo}o")
    })
}

#[derive(Default)]
struct Acc {
    w: f64,
    strat: Vec<f64>,
    epa: Vec<f64>,
    ev: f64,
    eq: f64,
}

fn main() -> Result<()> {
    let cfg = Config::load("phase1.toml").context("loading phase1.toml (run from the repo root)")?;
    let key = std::env::args()
        .nth(1)
        .context("usage: node_grid <spot-key> [path-spec]")?;
    let spec = std::env::args().nth(2).unwrap_or_else(|| "-".to_string());
    let labels = parse_path_labels(&spec)?;

    let mut solution = load_solution(&key, &cfg).with_context(|| format!("loading cached {key}"))?;
    let pot_chips = (solution.spot.pot_bb * cfg.solve.chip_per_bb as f64) as f32;

    // F5 gate: refuse to emit anything if any flop node on this spot fails ev_consistency
    let report = flop_node_ev_consistency(&mut solution.game, 500_000);
    if !report.ok {
        eprintln!(
            "F5 BATTERY FAILED on {key}: {} decision nodes, {} empty-EPA, truncated={}",
            report.decision_nodes, report.empty_epa_nodes, report.truncated
        );
        for note in &report.notes {
            eprintln!("  {note}");
        }
        std::process::exit(1);
    }
    eprintln!(
        "F5 battery OK on {key}: {} flop decision nodes pass dot-identity + normalization + EPA; \
         worst local dominance {:.2}% pot (reported, not gated - D55)",
        report.decision_nodes,
        report.max_dominance_chips / pot_chips * 100.0
    );

    let node = read_node_labeled(&mut solution.game, &labels)
        .with_context(|| format!("walking path {labels:?}"))?;
    let w = read_node_weights_labeled(&mut solution.game, &labels)?;
    let n = node.hands.len();
    let na = node.actions.len();
    let has_epa = node.ev_per_action.len() == n * na;

    // reach-weighted per-class aggregation (the same weighting the grid uses, so UI == oracle)
    let mut classes: BTreeMap<String, Acc> = BTreeMap::new();
    for h in 0..n {
        let cls = class_of(&node.hands[h])?;
        let wh = w[h] as f64;
        let acc = classes.entry(cls).or_insert_with(|| Acc {
            strat: vec![0.0; na],
            epa: vec![0.0; na],
            ..Default::default()
        });
        acc.w += wh;
        for a in 0..na {
            acc.strat[a] += wh * node.strategy[h + a * n] as f64;
            if has_epa {
                acc.epa[a] += wh * node.ev_per_action[h + a * n] as f64;
            }
        }
        acc.ev += wh * node.ev[h] as f64;
        acc.eq += wh * node.equity[h] as f64;
    }

    let mut class_json = serde_json::Map::new();
    for (cls, acc) in &classes {
        if acc.w <= 0.0 {
            continue;
        }
        let strat: Vec<f64> = acc.strat.iter().map(|s| s / acc.w).collect();
        let epa: Vec<f64> = acc.epa.iter().map(|e| e / acc.w).collect();
        class_json.insert(
            cls.clone(),
            serde_json::json!({
                "weight": acc.w,
                "strategy": strat,
                "ev": acc.ev / acc.w,
                "equity": acc.eq / acc.w,
                "ev_actions": if has_epa { serde_json::json!(epa) } else { serde_json::json!([]) },
            }),
        );
    }

    let out = serde_json::json!({
        "key": key,
        "path": labels,
        "player": node.player,
        "actions": node.actions,
        "classes": class_json,
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
