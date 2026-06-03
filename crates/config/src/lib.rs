//! `gto-config`: the single source of truth for every tunable in the engine.
//!
//! All knobs (bet-size trees, stack depths, target exploitability, iteration caps,
//! abstraction params, chip resolution, rake, IO and store settings) live here so the
//! engine / preflop / validate / store crates read configuration rather than hardcoding it.
//!
//! `Config::default()` returns the same values that `default.toml` documents.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Top-level configuration aggregate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub solve: SolveConfig,
    pub stacks: StacksConfig,
    pub rake: RakeConfig,
    pub io: IoConfig,
    pub store: StoreConfig,
}

/// Everything `engine::solve_spot` needs to turn a `Spot` into a solver query and run it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SolveConfig {
    /// Chip resolution: how many integer chips represent one big blind. Chosen so every
    /// configured bet size lands on an integer (default 20 == a 10/20 game).
    pub chip_per_bb: u32,
    /// Convergence target, expressed as a fraction of the starting pot (0.005 == 0.5% of pot).
    pub target_exploit_pct_pot: f64,
    /// Hard cap on solve iterations.
    pub max_iterations: u32,
    /// Print solver progress to stdout.
    pub print_progress: bool,
    pub bet_tree: BetTreeConfig,
    pub abstraction: AbstractionConfig,
}

/// Bet-size tree, per street, per `[OOP, IP]` player. Strings are postflop-solver size
/// specs (e.g. "33%", "60%, e, a"); an empty string means "no sizes of this kind".
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BetTreeConfig {
    /// Label baked into spot keys so different trees do not collide in the matrix.
    pub bet_tree_id: String,
    pub flop_bet_sizes: [String; 2],
    pub turn_bet_sizes: [String; 2],
    pub river_bet_sizes: [String; 2],
    pub flop_raise_sizes: [String; 2],
    pub turn_raise_sizes: [String; 2],
    pub river_raise_sizes: [String; 2],
    pub turn_donk_sizes: Option<String>,
    pub river_donk_sizes: Option<String>,
}

/// Tree-abstraction and memory knobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AbstractionConfig {
    /// Allocate the solver with 16-bit compressed storage instead of 32-bit float.
    pub use_compression: bool,
    /// Add an all-in action when the max bet size is at most this multiple of the pot.
    pub add_allin_threshold: f32,
    /// Force all-in when the SPR after a call is at most this value.
    pub force_allin_threshold: f32,
    /// Merge bet sizes whose resulting pot sizes are within this fraction of each other.
    pub merging_threshold: f32,
    /// Refuse to allocate a solve whose estimated memory exceeds this many GiB.
    pub ram_ceiling_gb: f64,
}

/// Rake model. Default 0/0 for pure GTO solving.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct RakeConfig {
    pub rake_rate: f32,
    pub rake_cap: f32,
}

/// Stack depths of interest, in big blinds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StacksConfig {
    pub stack_depths_bb: Vec<u32>,
    pub default_stack_bb: u32,
}

/// Output directories and serving knobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IoConfig {
    /// JSON dumps (smoke output etc.).
    pub out_dir: String,
    /// Solved-spot cache (the matrix): serialized trees + manifest.json.
    pub matrix_dir: String,
    /// Hard wall-clock cap on one on-demand solve (Phase 3 solve server).
    pub solve_timeout_secs: u64,
}

/// How solved trees are persisted to the matrix.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StoreConfig {
    pub use_zstd: bool,
    pub zstd_level: i32,
}

impl Config {
    /// Load a configuration from a TOML file. Missing fields fall back to defaults.
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path.as_ref())?;
        let cfg: Config = toml::from_str(&text)?;
        Ok(cfg)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            solve: SolveConfig::default(),
            stacks: StacksConfig::default(),
            rake: RakeConfig::default(),
            io: IoConfig::default(),
            store: StoreConfig::default(),
        }
    }
}

impl Default for SolveConfig {
    fn default() -> Self {
        Self {
            chip_per_bb: 20,
            target_exploit_pct_pot: 0.005,
            max_iterations: 1000,
            print_progress: true,
            bet_tree: BetTreeConfig::default(),
            abstraction: AbstractionConfig::default(),
        }
    }
}

impl Default for BetTreeConfig {
    fn default() -> Self {
        // Deliberately small "smoke-v0" tree: one bet size per street, no raises,
        // all-ins added only via the thresholds. Production trees override this.
        Self {
            bet_tree_id: "smoke-v0".to_string(),
            flop_bet_sizes: ["33%".to_string(), "33%".to_string()],
            turn_bet_sizes: ["50%".to_string(), "50%".to_string()],
            river_bet_sizes: ["50%".to_string(), "50%".to_string()],
            flop_raise_sizes: [String::new(), String::new()],
            turn_raise_sizes: [String::new(), String::new()],
            river_raise_sizes: [String::new(), String::new()],
            turn_donk_sizes: None,
            river_donk_sizes: None,
        }
    }
}

impl Default for AbstractionConfig {
    fn default() -> Self {
        Self {
            use_compression: false,
            add_allin_threshold: 1.5,
            force_allin_threshold: 0.15,
            merging_threshold: 0.1,
            ram_ceiling_gb: 8.0,
        }
    }
}

impl Default for RakeConfig {
    fn default() -> Self {
        Self { rake_rate: 0.0, rake_cap: 0.0 }
    }
}

impl Default for StacksConfig {
    fn default() -> Self {
        Self { stack_depths_bb: vec![100, 50, 20], default_stack_bb: 100 }
    }
}

impl Default for IoConfig {
    fn default() -> Self {
        Self {
            out_dir: "out".to_string(),
            matrix_dir: "matrix".to_string(),
            solve_timeout_secs: 300,
        }
    }
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self { use_zstd: true, zstd_level: 19 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let c = Config::default();
        assert_eq!(c.solve.chip_per_bb, 20);
        assert_eq!(c.solve.bet_tree.bet_tree_id, "smoke-v0");
        assert_eq!(c.io.matrix_dir, "matrix");
    }

    #[test]
    fn partial_toml_merges_with_defaults() {
        let toml = r#"
            [solve]
            max_iterations = 250
        "#;
        let c: Config = toml::from_str(toml).unwrap();
        assert_eq!(c.solve.max_iterations, 250);
        // untouched field keeps its default
        assert_eq!(c.solve.chip_per_bb, 20);
    }
}
