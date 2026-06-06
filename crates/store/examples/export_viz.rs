//! Export the curated matrix to JSON the Phase 2 viz consumes (read-only, no solving).
//!
//! Reads the manifest, loads each CURATED stored spot via gto-store, builds the shared viz
//! contract (`gto_store::viz`), and writes one JSON per spot plus an index into
//! `viz/public/data/`. Curated = the keys derivable from `benchmark/spots.toml` (the one
//! list) plus the Phase 0 smoke spot; ad-hoc on-demand solves accumulate in the matrix as
//! cache but never leak into the committed fixture set.
//! Run from the repo root:
//!   cargo run --release -p gto-store --example export_viz

use anyhow::{Context, Result};
use gto_config::{Config, PotType};
use gto_store::viz::build_contract;
use gto_store::{load_manifest, load_solution};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Deserialize)]
struct SpotsFile {
    spot: Vec<SpotRow>,
}

fn default_pot_type() -> PotType {
    PotType::Srp
}

#[derive(Deserialize)]
struct SpotRow {
    board: String,
    stack_depth_bb: u32,
    #[serde(default = "default_pot_type")]
    pot_type: PotType,
}

/// The curated key set: every spots.toml row under its pot type's tree id.
fn curated_keys(cfg: &Config) -> Result<BTreeSet<String>> {
    let text = std::fs::read_to_string("benchmark/spots.toml")
        .context("reading benchmark/spots.toml")?;
    let spots: SpotsFile = toml::from_str(&text).context("parsing benchmark/spots.toml")?;
    let mut keys = BTreeSet::new();
    for row in spots.spot {
        let tree_id = &cfg.solve.tree_for(row.pot_type)?.bet_tree_id;
        keys.insert(format!("{}bb_{}_flop_{}", row.stack_depth_bb, row.board, tree_id));
    }
    Ok(keys)
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

    let curated = curated_keys(&cfg)?;
    let mut index = Vec::new();
    let mut exported = 0usize;
    let mut skipped_uncurated = 0usize;

    // BTreeMap iterates keys in sorted order -> deterministic output
    for (key, _entry) in &manifest.spots {
        // the smoke spot (its own tree id) stays exported; ad-hoc cache entries do not
        if !curated.contains(key) && !key.ends_with("_smoke-v0") {
            skipped_uncurated += 1;
            continue;
        }
        match load_solution(key, &cfg) {
            Ok(mut solution) => {
                let contract = build_contract(key, &mut solution);
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

    println!(
        "\nexported {exported} spot(s) + index -> {} ({} uncurated cache entr{} skipped)",
        out_dir.display(),
        skipped_uncurated,
        if skipped_uncurated == 1 { "y" } else { "ies" }
    );
    Ok(())
}
