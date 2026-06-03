//! `gto-store`: precompute-and-cache storage for solved spots.
//!
//! Architecture: solve a curated set of spots once, persist each solved tree to the `matrix/`
//! directory, and look up at decision time with ZERO solving. Layout:
//!   - `matrix/<spot_key>.bin[.zst]`     the solved `PostFlopGame` (postflop-solver native bincode,
//!                                        optional zstd),
//!   - `matrix/<spot_key>.summary.json`  the spot + metadata + root strategy (serde sidecar),
//!   - `matrix/manifest.json`            spot_key -> metadata index (no solving to read).
//!
//! `load_solution` deserializes the tree, reads the sidecar, and recomputes the root from the
//! reloaded game. v1 is flat-files-plus-manifest only; a SQLite index over the files is the noted
//! deferred migration. Out-of-set spots get exact-key lookup only in Phase 0 (nearest-node mapping
//! and solve-then-cache are deferred).

pub mod viz;

use anyhow::{anyhow, Context, Result};
use gto_config::{BetTreeConfig, Config};
use gto_engine::{read_root, Spot, SpotSolution, SpotSolutionSummary, Street};
use postflop_solver::{load_data_from_file, save_data_to_file, PostFlopGame};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One manifest row: enough metadata to identify a stored solve without opening the tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub file: String,
    pub summary: String,
    pub stack_depth_bb: u32,
    pub board: String,
    pub street: String,
    pub bet_tree: String,
    pub exploitability_pct_pot: f32,
    pub iterations: u32,
}

/// The matrix index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub spots: BTreeMap<String, ManifestEntry>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self { version: 1, spots: BTreeMap::new() }
    }
}

/// Deterministic key for a spot under a given bet tree: `<depth>bb_<board>_<street>_<bet_tree_id>`.
pub fn spot_key(spot: &Spot, bet_tree: &BetTreeConfig) -> String {
    format!(
        "{}bb_{}_{}_{}",
        spot.stack_depth_bb,
        board_label(spot),
        street_label(spot.initial_street),
        bet_tree.bet_tree_id
    )
}

/// Persist a solved spot to the matrix (tree file + summary sidecar + manifest upsert).
/// Returns the path of the written tree file.
pub fn store_solution(solution: &SpotSolution, cfg: &Config) -> Result<PathBuf> {
    let dir = Path::new(&cfg.io.matrix_dir);
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating matrix dir {}", dir.display()))?;

    let key = spot_key(&solution.spot, &cfg.solve.bet_tree);

    // 1. serialize the solved tree (native bincode, optional zstd)
    let tree_ext = if cfg.store.use_zstd { "bin.zst" } else { "bin" };
    let tree_name = format!("{key}.{tree_ext}");
    let tree_path = dir.join(&tree_name);
    let compression = if cfg.store.use_zstd { Some(cfg.store.zstd_level) } else { None };
    let memo = format!("gto-poker-bot solved spot {key}");
    save_data_to_file(&solution.game, &memo, &tree_path, compression)
        .map_err(|e| anyhow!("saving tree to {}: {e}", tree_path.display()))?;

    // 2. write the serde sidecar (spot + metadata + root) so a reload can rebuild the SpotSolution
    let summary_name = format!("{key}.summary.json");
    let summary_path = dir.join(&summary_name);
    std::fs::write(&summary_path, serde_json::to_string_pretty(&solution.summary())?)
        .with_context(|| format!("writing summary {}", summary_path.display()))?;

    // 3. upsert the manifest
    let mut manifest = load_manifest(dir)?;
    manifest.spots.insert(
        key.clone(),
        ManifestEntry {
            file: tree_name,
            summary: summary_name,
            stack_depth_bb: solution.spot.stack_depth_bb,
            board: board_label(&solution.spot),
            street: street_label(solution.spot.initial_street).to_string(),
            bet_tree: cfg.solve.bet_tree.bet_tree_id.clone(),
            exploitability_pct_pot: solution.exploitability_pct_pot,
            iterations: solution.iterations_run,
        },
    );
    save_manifest(dir, &manifest)?;

    Ok(tree_path)
}

/// Load a solved spot by key. Reads the manifest, deserializes the tree, reads the sidecar, and
/// recomputes the root. No solving.
pub fn load_solution(spot_key: &str, cfg: &Config) -> Result<SpotSolution> {
    let dir = Path::new(&cfg.io.matrix_dir);
    let manifest = load_manifest(dir)?;
    let entry = manifest
        .spots
        .get(spot_key)
        .ok_or_else(|| anyhow!("spot key '{spot_key}' not found in matrix manifest"))?;

    // tree
    let tree_path = dir.join(&entry.file);
    let ceiling = (cfg.solve.abstraction.ram_ceiling_gb * (1u64 << 30) as f64) as u64;
    let (mut game, _memo): (PostFlopGame, String) =
        load_data_from_file(&tree_path, Some(ceiling))
            .map_err(|e| anyhow!("loading tree {}: {e}", tree_path.display()))?;

    // sidecar
    let summary_path = dir.join(&entry.summary);
    let summary_text = std::fs::read_to_string(&summary_path)
        .with_context(|| format!("reading summary {}", summary_path.display()))?;
    let summary: SpotSolutionSummary = serde_json::from_str(&summary_text)?;

    // recompute the root from the reloaded game
    let root = read_root(&mut game)?;

    Ok(SpotSolution {
        spot: summary.spot,
        bet_tree_id: summary.bet_tree_id,
        exploitability_chips: summary.exploitability_chips,
        exploitability_pct_pot: summary.exploitability_pct_pot,
        iterations_run: summary.iterations_run,
        root,
        game,
    })
}

/// Load only the summary sidecar for a stored spot (no tree deserialization). The fast path
/// for grading: the root in a freshly-written sidecar carries per-action EV; sidecars stored
/// before that field existed deserialize with it empty, and callers fall back to
/// `load_solution` (which recomputes the root from the tree).
pub fn load_summary(spot_key: &str, cfg: &Config) -> Result<SpotSolutionSummary> {
    let dir = Path::new(&cfg.io.matrix_dir);
    let manifest = load_manifest(dir)?;
    let entry = manifest
        .spots
        .get(spot_key)
        .ok_or_else(|| anyhow!("spot key '{spot_key}' not found in matrix manifest"))?;
    let summary_path = dir.join(&entry.summary);
    let text = std::fs::read_to_string(&summary_path)
        .with_context(|| format!("reading summary {}", summary_path.display()))?;
    Ok(serde_json::from_str(&text)?)
}

/// Read the matrix manifest, returning an empty one if it does not exist yet.
pub fn load_manifest(dir: &Path) -> Result<Manifest> {
    let path = manifest_path(dir);
    if !path.exists() {
        return Ok(Manifest::default());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading manifest {}", path.display()))?;
    Ok(serde_json::from_str(&text)?)
}

fn save_manifest(dir: &Path, manifest: &Manifest) -> Result<()> {
    let path = manifest_path(dir);
    std::fs::write(&path, serde_json::to_string_pretty(manifest)?)
        .with_context(|| format!("writing manifest {}", path.display()))?;
    Ok(())
}

fn manifest_path(dir: &Path) -> PathBuf {
    dir.join("manifest.json")
}

fn board_label(spot: &Spot) -> String {
    let mut s = spot.board.flop.concat();
    if let Some(t) = &spot.board.turn {
        s.push_str(t);
    }
    if let Some(r) = &spot.board.river {
        s.push_str(r);
    }
    s
}

fn street_label(street: Street) -> &'static str {
    match street {
        Street::Flop => "flop",
        Street::Turn => "turn",
        Street::River => "river",
    }
}
