//! `gto-config`: the single source of truth for every tunable in the engine.
//!
//! All knobs (bet-size trees, stack depths, target exploitability, iteration caps,
//! abstraction params, chip resolution, rake, IO and store settings) live here so the
//! engine / preflop / validate / store crates read configuration rather than hardcoding it.
//!
//! `Config::default()` returns the same values that `default.toml` documents.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// The HU preflop pot families the engine models. A pot type selects the preflop line
/// (and thus the flop-entering pot, stacks, and ranges - see gto-preflop `spot_inputs`)
/// and its own bet tree (`SolveConfig::tree_for`). The postflop engine itself stays
/// numeric: `Spot` carries pot/stack values, never this enum.
///
/// Serde strings ("srp" / "3bp" / "4bp") are the wire format used by the server API and
/// `benchmark/spots.toml`, and mirror the gate registry's `TREE_IDS` keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PotType {
    /// BTN opens, BB calls. IP (BTN) is the flop aggressor.
    #[serde(rename = "srp")]
    Srp,
    /// BTN opens, BB 3-bets, BTN calls. OOP (BB) is the flop aggressor.
    #[serde(rename = "3bp")]
    ThreeBet,
    /// BTN opens, BB 3-bets, BTN 4-bets, BB calls. IP (BTN) is the flop aggressor.
    #[serde(rename = "4bp")]
    FourBet,
}

impl PotType {
    /// The wire/registry label ("srp" / "3bp" / "4bp").
    pub fn label(self) -> &'static str {
        match self {
            PotType::Srp => "srp",
            PotType::ThreeBet => "3bp",
            PotType::FourBet => "4bp",
        }
    }
}

/// Top-level configuration aggregate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub solve: SolveConfig,
    pub stacks: StacksConfig,
    pub rake: RakeConfig,
    pub io: IoConfig,
    pub store: StoreConfig,
    pub trainer: TrainerConfig,
    pub review: ReviewConfig,
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
    /// The SRP bet tree (historical field name kept for TOML back-compat).
    pub bet_tree: BetTreeConfig,
    /// The 3-bet-pot bet tree; absent means 3-bet pots are not configured for solving.
    pub bet_tree_3bp: Option<BetTreeConfig>,
    /// The 4-bet-pot bet tree; absent means 4-bet pots are not configured for solving.
    pub bet_tree_4bp: Option<BetTreeConfig>,
    pub abstraction: AbstractionConfig,
}

impl SolveConfig {
    /// The single funnel for tree selection: every consumer resolves a pot type's bet tree
    /// here, so tree ids never re-appear as scattered literals (the TREE_ID lesson).
    /// Errors cleanly when a pot type has no tree configured.
    pub fn tree_for(&self, pot: PotType) -> anyhow::Result<&BetTreeConfig> {
        match pot {
            PotType::Srp => Ok(&self.bet_tree),
            PotType::ThreeBet => self.bet_tree_3bp.as_ref().ok_or_else(|| {
                anyhow::anyhow!("no [solve.bet_tree_3bp] configured (3-bet pots unavailable)")
            }),
            PotType::FourBet => self.bet_tree_4bp.as_ref().ok_or_else(|| {
                anyhow::anyhow!("no [solve.bet_tree_4bp] configured (4-bet pots unavailable)")
            }),
        }
    }
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

/// Phase 5 hand-history review knobs.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct ReviewConfig {
    /// Per-ingestion-run cap on FRESH solves; capped spots become the pending-solve state.
    pub max_fresh_solves_per_run: u32,
    /// Minimum instances at a node before it may appear in the ranked-leak list.
    pub min_sample_n: u32,
    /// |z| threshold (Poisson-binomial) for a ranked leak.
    pub z_threshold: f64,
    /// D45: a played-at-mix-floor action may not exceed the mix EV by more than this fraction
    /// of pot (F5 regression tripwire through read_node). Calibration: audit-measured
    /// legitimate played-gaps reach ~1.4% pot on converged solves; F5-class violations are
    /// 30%+ of pot; 2% separates them cleanly (the literal solve target of 0.5% would
    /// false-fire on audited-good data).
    pub ev_sanity_eps_pct_pot: f64,
    /// Hero's own bet maps to a tree size only within this fraction of pot (else skipped
    /// `off-tree-hero-size`; nearest-mapping would fabricate the size split).
    pub hero_size_tolerance_pct_pot: f64,
    /// Frequency floor above which an action counts as "meaningfully played" for D45.
    pub mix_floor: f64,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            max_fresh_solves_per_run: 25,
            min_sample_n: 10,
            z_threshold: 2.0,
            ev_sanity_eps_pct_pot: 0.02,
            hero_size_tolerance_pct_pot: 0.08,
            mix_floor: 0.10,
        }
    }
}

/// Trainer verdict bands (Phase 4). All EV-loss thresholds are fractions of the starting pot.
/// Verdict precedence: Correct (loss <= eps) > FineMix (freq >= floor AND loss <= minor max)
/// > MinorLeak (loss <= minor max) > ClearLeak.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct TrainerConfig {
    /// Chosen action within this EV of the best action -> Correct regardless of frequency.
    /// Default equals the solve convergence target: verdicts are honest to solve precision.
    pub correct_eps_pct_pot: f64,
    /// EV loss above this -> ClearLeak.
    pub minor_leak_max_pct_pot: f64,
    /// Solver frequency at or above this (with small loss) -> FineMix, not a mistake.
    pub mix_freq_floor: f64,
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
            trainer: TrainerConfig::default(),
            review: ReviewConfig::default(),
        }
    }
}

impl Default for TrainerConfig {
    fn default() -> Self {
        Self {
            correct_eps_pct_pot: 0.005,
            minor_leak_max_pct_pot: 0.02,
            mix_freq_floor: 0.10,
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
            bet_tree_3bp: None,
            bet_tree_4bp: None,
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

    #[test]
    fn tree_for_resolves_srp_and_errors_on_unconfigured_pots() {
        let c = Config::default();
        assert_eq!(c.solve.tree_for(PotType::Srp).unwrap().bet_tree_id, "smoke-v0");
        assert!(c.solve.tree_for(PotType::ThreeBet).is_err());
        assert!(c.solve.tree_for(PotType::FourBet).is_err());
    }

    #[test]
    fn optional_pot_trees_parse_from_toml() {
        let toml = r#"
            [solve.bet_tree_3bp]
            bet_tree_id = "3bp-v1"
            [solve.bet_tree_4bp]
            bet_tree_id = "4bp-v1"
        "#;
        let c: Config = toml::from_str(toml).unwrap();
        assert_eq!(c.solve.tree_for(PotType::ThreeBet).unwrap().bet_tree_id, "3bp-v1");
        assert_eq!(c.solve.tree_for(PotType::FourBet).unwrap().bet_tree_id, "4bp-v1");
        // the optional sections inherit BetTreeConfig defaults for unspecified fields
        assert_eq!(
            c.solve.bet_tree_3bp.as_ref().unwrap().flop_bet_sizes,
            ["33%".to_string(), "33%".to_string()]
        );
    }

    #[test]
    fn pot_type_wire_format_is_stable() {
        // the serde strings are load-bearing: server API field, spots.toml, registry keys
        for (pot, s) in [
            (PotType::Srp, "\"srp\""),
            (PotType::ThreeBet, "\"3bp\""),
            (PotType::FourBet, "\"4bp\""),
        ] {
            assert_eq!(serde_json::to_string(&pot).unwrap(), s);
            let back: PotType = serde_json::from_str(s).unwrap();
            assert_eq!(back, pot);
            assert_eq!(format!("\"{}\"", pot.label()), s);
        }
    }
}
