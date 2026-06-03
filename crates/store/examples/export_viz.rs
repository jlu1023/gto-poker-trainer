//! Export the curated matrix to JSON the Phase 2 viz consumes (read-only, no solving).
//!
//! Reads the manifest, loads each stored spot via gto-store, builds the shared viz contract
//! (`gto_store::viz`), and writes one JSON per spot plus an index into `viz/public/data/`.
//! Run from the repo root:
//!   cargo run --release -p gto-store --example export_viz

use anyhow::{Context, Result};
use gto_config::Config;
use gto_store::viz::build_contract;
use gto_store::{load_manifest, load_solution};
use serde::Serialize;
use std::path::Path;

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
