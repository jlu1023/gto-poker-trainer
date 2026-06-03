//! `gto-preflop`: heads-up preflop range provider.
//!
//! DECISION: HU preflop ranges are *imported* from published charts and fed to the postflop
//! solver as fixed inputs (HU preflop is fully solved; postflop-solver is postflop-only). Phase 1
//! parses a published HU set (open / 3bet / 4bet / 5bet by stack depth) into solver range strings.
//!
//! Phase 0 (this stub): returns hardcoded single-raised-pot ranges so the engine has valid inputs.

use anyhow::Result;
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

/// Handle to imported preflop charts. Phase 1 will load published HU charts from disk.
pub struct PreflopCharts {
    _private: (),
}

impl PreflopCharts {
    /// Phase 1: parse a published HU chart set from `_dir`. Phase 0: an empty handle.
    pub fn load(_dir: impl AsRef<Path>) -> Result<Self> {
        Ok(Self { _private: () })
    }
}

/// Return `(oop_range, ip_range)` as postflop-solver range strings for `ctx`.
///
/// Phase 0 stub: hardcoded ~100bb single-raised-pot ranges (BB defending vs BTN open). These are
/// reasonable placeholders that parse cleanly; Phase 1 replaces them with imported chart data.
pub fn ranges_for(ctx: &PreflopContext) -> (String, String) {
    match ctx.node {
        PreflopNode::SingleRaisedPot => (SRP_BB_RANGE.to_string(), SRP_BTN_RANGE.to_string()),
    }
}

/// OOP = big blind, defending against a button open.
const SRP_BB_RANGE: &str =
    "66+,A8s+,A5s-A4s,AJo+,K9s+,KQo,QTs+,JTs,96s+,85s+,75s+,65s,54s";

/// IP = button, opening range.
const SRP_BTN_RANGE: &str =
    "QQ-22,AQs-A2s,ATo+,K5s+,KJo+,Q8s+,J8s+,T7s+,96s+,86s+,75s+,64s+,53s+";
