//! `gto-preflop`: heads-up preflop range provider.
//!
//! DECISION (Phase 1): HU preflop ranges are AUTHORED in-repo (`data/hu_ranges.toml`) from
//! established HU GTO theory, not imported. No clean, free, importable HU chart set was confirmed
//! (the top candidate's data did not exist; others were 6-max / MTT / unlicensed / solver-only).
//! postflop-solver is postflop-only, so these ranges are fed to it as fixed inputs.
//!
//! The data is embedded at build time (`include_str!`) so the loader works regardless of CWD, and a
//! `load(path)` override reads a TOML file from disk.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// Which preflop node's ranges to return.
#[derive(Debug, Clone)]
pub struct PreflopContext {
    pub stack_depth_bb: u32,
    pub node: PreflopNode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreflopNode {
    /// BTN opens, BB calls (the canonical single-raised pot).
    SingleRaisedPot,
}

/// The two ranges entering a node: `oop` (BB postflop) and `ip` (BTN postflop), as solver strings.
#[derive(Debug, Clone, Deserialize)]
struct NodeRanges {
    oop: String,
    ip: String,
}

/// Raw shape of `hu_ranges.toml`.
#[derive(Debug, Deserialize)]
struct ChartsFile {
    /// Single-raised-pot ranges, keyed by stack depth (string keys in TOML).
    srp: BTreeMap<String, NodeRanges>,
}

/// Authored HU preflop charts. `srp` is keyed by stack depth in bb.
pub struct PreflopCharts {
    srp: BTreeMap<u32, NodeRanges>,
}

/// The authored ranges, embedded at build time.
const EMBEDDED: &str = include_str!("../data/hu_ranges.toml");

impl PreflopCharts {
    /// Load the authored charts embedded at build time (CWD-independent).
    pub fn load_embedded() -> Result<Self> {
        Self::from_toml(EMBEDDED).context("parsing embedded hu_ranges.toml")
    }

    /// Load charts from a TOML file on disk (overrides the embedded set).
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading preflop charts {}", path.display()))?;
        Self::from_toml(&text).with_context(|| format!("parsing {}", path.display()))
    }

    fn from_toml(text: &str) -> Result<Self> {
        let file: ChartsFile = toml::from_str(text)?;
        let mut srp = BTreeMap::new();
        for (depth_str, ranges) in file.srp {
            let depth: u32 = depth_str
                .parse()
                .map_err(|_| anyhow!("invalid stack depth key '{depth_str}' in [srp]"))?;
            srp.insert(depth, ranges);
        }
        Ok(Self { srp })
    }

    /// Return `(oop_range, ip_range)` as solver range strings for `ctx`, erroring if the
    /// (node, stack depth) pair is not present.
    pub fn ranges_for(&self, ctx: &PreflopContext) -> Result<(String, String)> {
        match ctx.node {
            PreflopNode::SingleRaisedPot => {
                let r = self.srp.get(&ctx.stack_depth_bb).ok_or_else(|| {
                    anyhow!(
                        "no SRP ranges authored for {}bb (have: {:?})",
                        ctx.stack_depth_bb,
                        self.srp.keys().collect::<Vec<_>>()
                    )
                })?;
                Ok((r.oop.clone(), r.ip.clone()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use postflop_solver::{card_from_str, Range};

    fn srp(depth: u32) -> (String, String) {
        PreflopCharts::load_embedded()
            .unwrap()
            .ranges_for(&PreflopContext {
                stack_depth_bb: depth,
                node: PreflopNode::SingleRaisedPot,
            })
            .unwrap()
    }

    #[test]
    fn every_authored_range_parses_as_postflop_range() {
        for depth in [100u32, 50] {
            let (oop, ip) = srp(depth);
            oop.parse::<Range>()
                .unwrap_or_else(|e| panic!("oop {depth}bb does not parse: {e}"));
            ip.parse::<Range>()
                .unwrap_or_else(|e| panic!("ip {depth}bb does not parse: {e}"));
        }
    }

    #[test]
    fn missing_depth_errors() {
        let charts = PreflopCharts::load_embedded().unwrap();
        assert!(charts
            .ranges_for(&PreflopContext { stack_depth_bb: 200, node: PreflopNode::SingleRaisedPot })
            .is_err());
    }

    /// Weight of a specific combo in a range string, via the authoritative parser (the
    /// rs_poker parser does not know the weighted ":w" syntax, so membership and weight
    /// checks both go through postflop-solver).
    fn w(range: &str, hand: &str) -> f32 {
        let r: Range = range.parse().unwrap_or_else(|e| panic!("range parse: {e}"));
        let c1 = card_from_str(&hand[0..2]).unwrap();
        let c2 = card_from_str(&hand[2..4]).unwrap();
        r.get_weight_by_cards(c1, c2)
    }

    #[test]
    fn known_hand_membership_and_weights() {
        let (oop100, ip100) = srp(100);
        // 3-bet-mix top: premiums enter the SRP call range at REDUCED weight
        assert!((w(&oop100, "AhAd") - 0.25).abs() < 1e-6, "AA should call at 0.25");
        assert!((w(&oop100, "JhJd") - 0.75).abs() < 1e-6, "JJ should call at 0.75");
        // full-weight core, including the low coverage the audit found missing
        assert!((w(&oop100, "9h8h") - 1.0).abs() < 1e-6, "98s defends in full");
        assert!((w(&oop100, "7h6c") - 1.0).abs() < 1e-6, "76o defends in full");
        // partial-weight margins
        assert!((w(&oop100, "Ah5c") - 0.75).abs() < 1e-6, "A5o defends at 0.75");
        assert!((w(&oop100, "6h4c") - 0.5).abs() < 1e-6, "64o defends at 0.5");
        // trash stays out
        assert_eq!(w(&oop100, "7h2c"), 0.0, "72o must not defend");
        // ip range is unchanged and unweighted
        assert!((w(&ip100, "AhAd") - 1.0).abs() < 1e-6);

        let (oop50, ip50) = srp(50);
        assert!((w(&oop50, "AhAd") - 0.25).abs() < 1e-6);
        assert!((w(&oop50, "Th7c") - 0.5).abs() < 1e-6, "T7o defends at 0.5 shallow");
        assert_eq!(w(&oop50, "7h2c"), 0.0);
        assert!((w(&ip50, "AhAd") - 1.0).abs() < 1e-6);
    }
}
