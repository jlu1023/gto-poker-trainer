//! Export the curated matrix to JSON the Phase 2 viz consumes (read-only, no solving).
//!
//! Reads the manifest, loads each stored spot via gto-store, reshapes the root node's
//! `strategy[hand + action*n]` into clean per-hand arrays, and writes one JSON per spot plus an index
//! into `viz/public/data/`. Run from the repo root:
//!   cargo run --release -p gto-store --example export_viz

use anyhow::{Context, Result};
use gto_config::Config;
use gto_store::{load_manifest, load_solution};
use serde::Serialize;
use std::path::Path;

#[derive(Serialize)]
struct Seat {
    position: &'static str,
    seat: &'static str,
}

#[derive(Serialize)]
struct HeroSeat {
    player: usize,
    position: &'static str,
    seat: &'static str,
}

#[derive(Serialize)]
struct HandEntry {
    hand: String,
    strategy: Vec<f32>,
    ev: f32,
    equity: f32,
}

#[derive(Serialize)]
struct SpotContract {
    key: String,
    board: Vec<String>,
    street: String,
    stack_depth_bb: u32,
    pot_bb: f64,
    eff_stack_bb: f64,
    exploitability_pct_pot: f32,
    iterations: u32,
    hero: HeroSeat,
    villain: Seat,
    actions: Vec<String>,
    hands: Vec<HandEntry>,
}

#[derive(Serialize)]
struct IndexEntry {
    key: String,
    board: Vec<String>,
    stack_depth_bb: u32,
    exploitability_pct_pot: f32,
}

#[derive(Serialize)]
struct IndexFile {
    spots: Vec<IndexEntry>,
}

fn main() -> Result<()> {
    // phase1.toml sets the bet tree; io.matrix_dir falls back to the default "matrix".
    let cfg = Config::load("phase1.toml").unwrap_or_default();
    let manifest = load_manifest(Path::new(&cfg.io.matrix_dir))?;

    let out_dir = Path::new("viz").join("public").join("data");
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("creating {}", out_dir.display()))?;

    let mut index = Vec::new();
    let mut exported = 0usize;

    // BTreeMap iterates keys in sorted order -> deterministic output
    for (key, _entry) in &manifest.spots {
        match load_solution(key, &cfg) {
            Ok(solution) => {
                let contract = build_contract(key, &solution);
                let path = out_dir.join(format!("{key}.json"));
                std::fs::write(&path, serde_json::to_string_pretty(&contract)?)
                    .with_context(|| format!("writing {}", path.display()))?;
                index.push(IndexEntry {
                    key: key.clone(),
                    board: contract.board.clone(),
                    stack_depth_bb: contract.stack_depth_bb,
                    exploitability_pct_pot: contract.exploitability_pct_pot,
                });
                exported += 1;
                println!("exported {key}");
            }
            Err(e) => {
                // missing/unreadable tree: skip; the front-end shows an empty state for it
                eprintln!("skipping {key}: {e}");
            }
        }
    }

    index.sort_by(|a, b| a.key.cmp(&b.key));
    let index_path = out_dir.join("index.json");
    std::fs::write(&index_path, serde_json::to_string_pretty(&IndexFile { spots: index })?)
        .with_context(|| format!("writing {}", index_path.display()))?;

    println!("\nexported {exported} spot(s) + index -> {}", out_dir.display());
    Ok(())
}

fn build_contract(key: &str, solution: &gto_engine::SpotSolution) -> SpotContract {
    let root = &solution.root;
    let n = root.hands.len();
    let num_actions = root.actions.len();

    let hands = (0..n)
        .map(|i| HandEntry {
            hand: root.hands[i].clone(),
            strategy: (0..num_actions).map(|a| root.strategy[i + a * n]).collect(),
            ev: root.ev.get(i).copied().unwrap_or(0.0),
            equity: root.equity.get(i).copied().unwrap_or(0.0),
        })
        .collect();

    let spot = &solution.spot;
    let mut board: Vec<String> = spot.board.flop.to_vec();
    if let Some(t) = &spot.board.turn {
        board.push(t.clone());
    }
    if let Some(r) = &spot.board.river {
        board.push(r.clone());
    }

    let oop = root.player == 0;
    SpotContract {
        key: key.to_string(),
        board,
        street: street_label(spot.initial_street).to_string(),
        stack_depth_bb: spot.stack_depth_bb,
        pot_bb: spot.pot_bb,
        eff_stack_bb: spot.effective_stack_bb,
        exploitability_pct_pot: solution.exploitability_pct_pot,
        iterations: solution.iterations_run,
        hero: HeroSeat {
            player: root.player,
            position: if oop { "OOP" } else { "IP" },
            seat: if oop { "BB" } else { "BTN" },
        },
        villain: Seat {
            position: if oop { "IP" } else { "OOP" },
            seat: if oop { "BTN" } else { "BB" },
        },
        actions: root.actions.clone(),
        hands,
    }
}

fn street_label(street: gto_engine::Street) -> &'static str {
    match street {
        gto_engine::Street::Flop => "flop",
        gto_engine::Street::Turn => "turn",
        gto_engine::Street::River => "river",
    }
}
