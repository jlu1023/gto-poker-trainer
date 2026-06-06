//! `gto-preflop`: heads-up preflop range provider.
//!
//! DECISION (Phase 1): HU preflop ranges are AUTHORED in-repo (`data/hu_ranges.toml`) from
//! established HU GTO theory, not imported. No clean, free, importable HU chart set was confirmed
//! (the top candidate's data did not exist; others were 6-max / MTT / unlicensed / solver-only).
//! postflop-solver is postflop-only, so these ranges are fed to it as fixed inputs.
//!
//! Phase 3bp: this crate is also the single source for the PREFLOP GEOMETRY each pot type
//! carries to the flop (`spot_inputs`): the pot and effective stack live in the TOML next to
//! the ranges they belong to, so the `pot_bb: 5.0` / `depth - 2.5` literal class that used to
//! be scattered across four call sites is gone (the TREE_ID lesson applied to geometry).
//! The uniform HU law - last preflop raise-to R, flat-called: `pot = 2R`, `eff = depth - R` -
//! is asserted at LOAD time for every section, and the literal values are pinned by tests.
//!
//! The data is embedded at build time (`include_str!`) so the loader works regardless of CWD,
//! and a `load(path)` override reads a TOML file from disk.

use anyhow::{anyhow, bail, Context, Result};
use gto_config::PotType;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// Which preflop node's ranges to return (legacy SRP-only surface; new callers use
/// [`PreflopCharts::spot_inputs`] with a [`PotType`]).
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

/// Everything a flop `Spot` needs from the preflop story of one (pot type, depth):
/// the two ranges plus the geometry that the preflop line produced.
#[derive(Debug, Clone)]
pub struct SpotInputs {
    /// BB's flop-entering range (BB is always OOP postflop in HU).
    pub oop_range: String,
    /// BTN's flop-entering range.
    pub ip_range: String,
    /// The last preflop raise-to in bb (2.5 srp / 10 or 9 3bp / 22 4bp) - the canonical
    /// size the review envelope bands center on.
    pub raise_to_bb: f64,
    pub pot_bb: f64,
    pub eff_stack_bb: f64,
}

/// One authored TOML section: the two ranges entering a node plus its geometry.
#[derive(Debug, Clone, Deserialize)]
struct NodeEntry {
    oop: String,
    ip: String,
    raise_to_bb: f64,
    pot_bb: f64,
    eff_stack_bb: f64,
}

/// Raw shape of `hu_ranges.toml`.
#[derive(Debug, Deserialize)]
struct ChartsFile {
    /// Single-raised-pot sections, keyed by stack depth (string keys in TOML).
    srp: BTreeMap<String, NodeEntry>,
    /// 3-bet-pot sections; absent depths are simply not supported.
    #[serde(rename = "3bp", default)]
    threebet: BTreeMap<String, NodeEntry>,
    /// 4-bet-pot sections (100bb only by decision; 50bb 4-bet pots are jam-or-fold).
    #[serde(rename = "4bp", default)]
    fourbet: BTreeMap<String, NodeEntry>,
}

/// Authored HU preflop charts, keyed by stack depth in bb per pot type.
pub struct PreflopCharts {
    srp: BTreeMap<u32, NodeEntry>,
    threebet: BTreeMap<u32, NodeEntry>,
    fourbet: BTreeMap<u32, NodeEntry>,
}

/// The authored ranges, embedded at build time.
const EMBEDDED: &str = include_str!("../data/hu_ranges.toml");

fn parse_depth_map(
    raw: BTreeMap<String, NodeEntry>,
    section: &str,
) -> Result<BTreeMap<u32, NodeEntry>> {
    let mut out = BTreeMap::new();
    for (depth_str, entry) in raw {
        let depth: u32 = depth_str
            .parse()
            .map_err(|_| anyhow!("invalid stack depth key '{depth_str}' in [{section}]"))?;
        // The HU geometry law, enforced at the data layer on every load: the last raise-to
        // R, flat-called, leaves pot = 2R and eff = depth - R. A section that violates it
        // is a typo, and a typo here would silently mis-pot every downstream solve.
        let want_pot = 2.0 * entry.raise_to_bb;
        let want_eff = depth as f64 - entry.raise_to_bb;
        if (entry.pot_bb - want_pot).abs() > 1e-9 || (entry.eff_stack_bb - want_eff).abs() > 1e-9 {
            bail!(
                "[{section}.{depth}] geometry violates pot = 2*raise_to / eff = depth - raise_to: \
                 raise_to {} -> expected pot {} eff {}, got pot {} eff {}",
                entry.raise_to_bb,
                want_pot,
                want_eff,
                entry.pot_bb,
                entry.eff_stack_bb
            );
        }
        out.insert(depth, entry);
    }
    Ok(out)
}

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
        Ok(Self {
            srp: parse_depth_map(file.srp, "srp")?,
            threebet: parse_depth_map(file.threebet, "3bp")?,
            fourbet: parse_depth_map(file.fourbet, "4bp")?,
        })
    }

    fn section(&self, pot: PotType) -> &BTreeMap<u32, NodeEntry> {
        match pot {
            PotType::Srp => &self.srp,
            PotType::ThreeBet => &self.threebet,
            PotType::FourBet => &self.fourbet,
        }
    }

    /// Ranges + geometry for one (pot type, depth): the single source every Spot builder
    /// uses. Errors when the pair is not authored (e.g. 4bp at 50bb, by decision).
    pub fn spot_inputs(&self, pot: PotType, stack_depth_bb: u32) -> Result<SpotInputs> {
        let entry = self.section(pot).get(&stack_depth_bb).ok_or_else(|| {
            anyhow!(
                "no {} inputs authored for {}bb (have: {:?})",
                pot.label(),
                stack_depth_bb,
                self.section(pot).keys().collect::<Vec<_>>()
            )
        })?;
        Ok(SpotInputs {
            oop_range: entry.oop.clone(),
            ip_range: entry.ip.clone(),
            raise_to_bb: entry.raise_to_bb,
            pot_bb: entry.pot_bb,
            eff_stack_bb: entry.eff_stack_bb,
        })
    }

    /// Return `(oop_range, ip_range)` as solver range strings for `ctx`, erroring if the
    /// (node, stack depth) pair is not present. Legacy SRP-only surface kept for the solve
    /// server through Phase B; new callers use [`Self::spot_inputs`].
    pub fn ranges_for(&self, ctx: &PreflopContext) -> Result<(String, String)> {
        match ctx.node {
            PreflopNode::SingleRaisedPot => {
                let inputs = self.spot_inputs(PotType::Srp, ctx.stack_depth_bb)?;
                Ok((inputs.oop_range, inputs.ip_range))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use postflop_solver::{card_from_str, Range};

    fn charts() -> PreflopCharts {
        PreflopCharts::load_embedded().unwrap()
    }

    fn inputs(pot: PotType, depth: u32) -> SpotInputs {
        charts().spot_inputs(pot, depth).unwrap()
    }

    fn srp(depth: u32) -> (String, String) {
        let i = inputs(PotType::Srp, depth);
        (i.oop_range, i.ip_range)
    }

    #[test]
    fn every_authored_range_parses_as_postflop_range() {
        for (pot, depths) in [
            (PotType::Srp, &[100u32, 50][..]),
            (PotType::ThreeBet, &[100, 50][..]),
            (PotType::FourBet, &[100][..]),
        ] {
            for &depth in depths {
                let i = inputs(pot, depth);
                i.oop_range.parse::<Range>().unwrap_or_else(|e| {
                    panic!("{} oop {depth}bb does not parse: {e}", pot.label())
                });
                i.ip_range.parse::<Range>().unwrap_or_else(|e| {
                    panic!("{} ip {depth}bb does not parse: {e}", pot.label())
                });
            }
        }
    }

    #[test]
    fn missing_depth_errors() {
        let charts = charts();
        assert!(charts.spot_inputs(PotType::Srp, 200).is_err());
        // 4bp at 50bb is OUT OF SCOPE by decision (SPR ~0.75 jam-or-fold): the lookup must
        // error, not invent geometry - the server and envelope rely on this boundary.
        assert!(charts.spot_inputs(PotType::FourBet, 50).is_err());
    }

    /// The geometry literals, pinned. The load-time check enforces the internal LAW
    /// (pot = 2*raise_to, eff = depth - raise_to); this pins the authored VALUES so a
    /// silent re-sizing (e.g. 3-bet to 11) cannot slip through as a self-consistent edit.
    #[test]
    fn geometry_literals_are_pinned() {
        let cases = [
            (PotType::Srp, 100, 2.5, 5.0, 97.5),
            (PotType::Srp, 50, 2.5, 5.0, 47.5),
            (PotType::ThreeBet, 100, 10.0, 20.0, 90.0),
            (PotType::ThreeBet, 50, 9.0, 18.0, 41.0),
            (PotType::FourBet, 100, 22.0, 44.0, 78.0),
        ];
        for (pot, depth, raise_to, pot_bb, eff) in cases {
            let i = inputs(pot, depth);
            assert_eq!(i.raise_to_bb, raise_to, "{} {depth}bb raise_to", pot.label());
            assert_eq!(i.pot_bb, pot_bb, "{} {depth}bb pot", pot.label());
            assert_eq!(i.eff_stack_bb, eff, "{} {depth}bb eff", pot.label());
        }
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

    /// Sum of weights over all 1326 combos, and a per-combo visitor for the consistency
    /// rules. Card ids are postflop-solver's 0..52.
    fn for_each_combo(mut f: impl FnMut(u8, u8)) {
        for c1 in 0..52u8 {
            for c2 in (c1 + 1)..52u8 {
                f(c1, c2);
            }
        }
    }

    fn total_weight(range: &str) -> f64 {
        let r: Range = range.parse().unwrap();
        let mut sum = 0.0f64;
        for_each_combo(|c1, c2| sum += r.get_weight_by_cards(c1, c2) as f64);
        sum
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

    #[test]
    fn threebet_known_weights() {
        let i = inputs(PotType::ThreeBet, 100);
        // value = the premium complements the SRP weights reserved
        assert!((w(&i.oop_range, "AhAd") - 0.75).abs() < 1e-6, "AA 3-bets at 0.75");
        assert!((w(&i.oop_range, "JhJd") - 0.25).abs() < 1e-6, "JJ 3-bets at 0.25");
        // bluff wing: blocker offsuit + low suited gappers at partial budget
        assert!((w(&i.oop_range, "Ah5c") - 0.25).abs() < 1e-6, "A5o bluffs at 0.25");
        assert!((w(&i.oop_range, "9h5h") - 0.25).abs() < 1e-6, "95s bluffs at 0.25");
        // the merged middle has NO budget (SRP calls at 1.0) and must not appear
        assert_eq!(w(&i.oop_range, "ThTd"), 0.0, "TT cannot 3-bet (full SRP call)");
        assert_eq!(w(&i.oop_range, "7h2c"), 0.0);
        // BTN call slice: pairs in full, the 4-bet carve-outs at their complements
        assert!((w(&i.ip_range, "JhJd") - 1.0).abs() < 1e-6, "JJ flats the 3-bet in full");
        assert!((w(&i.ip_range, "AhAd") - 0.25).abs() < 1e-6, "AA flats at 0.25 (4-bets 0.75)");
        assert!((w(&i.ip_range, "Ah5h") - 0.75).abs() < 1e-6, "A5s flats at 0.75 (4-bets 0.25)");

        let i4 = inputs(PotType::FourBet, 100);
        assert!((w(&i4.ip_range, "Ah5h") - 0.25).abs() < 1e-6, "A5s 4-bet bluffs at 0.25");
        assert!((w(&i4.oop_range, "QhQd") - 0.5).abs() < 1e-6, "QQ calls the 4-bet at 0.5");
        assert_eq!(w(&i4.oop_range, "AhQc"), 0.0, "AQo folds to the 4-bet");
    }

    /// THE complement law the whole 3bp construction rests on: per hand, the SRP call
    /// weight plus the 3-bet weight may not exceed the hand's total mass of 1. The SRP
    /// fractional weights RESERVED the 3-bet mix; this proves the reservation is spent,
    /// never overspent, at every single combo.
    #[test]
    fn threebet_plus_srp_call_never_exceeds_one() {
        for depth in [100u32, 50] {
            let call: Range = inputs(PotType::Srp, depth).oop_range.parse().unwrap();
            let threebet: Range = inputs(PotType::ThreeBet, depth).oop_range.parse().unwrap();
            let mut worst = 0.0f32;
            for_each_combo(|c1, c2| {
                let total =
                    call.get_weight_by_cards(c1, c2) + threebet.get_weight_by_cards(c1, c2);
                if total > worst {
                    worst = total;
                }
                assert!(
                    total <= 1.0 + 1e-6,
                    "{depth}bb: combo ({c1},{c2}) call+3bet = {total} > 1"
                );
            });
            // the premiums spend their reservation EXACTLY (call + 3bet == 1)
            assert!((worst - 1.0).abs() < 1e-6, "{depth}bb: max call+3bet should be 1.0");
        }
    }

    /// BTN side of the same law: the call-vs-3-bet slice plus the 4-bet slice must fit
    /// inside the opening weight per hand (100bb; at 50bb there is no 4bp section and the
    /// call slice alone is bounded - the premium remainder is the implied preflop jam).
    #[test]
    fn btn_defence_fits_inside_the_open() {
        let open100: Range = inputs(PotType::Srp, 100).ip_range.parse().unwrap();
        let call3: Range = inputs(PotType::ThreeBet, 100).ip_range.parse().unwrap();
        let four: Range = inputs(PotType::FourBet, 100).ip_range.parse().unwrap();
        for_each_combo(|c1, c2| {
            let used = call3.get_weight_by_cards(c1, c2) + four.get_weight_by_cards(c1, c2);
            let open = open100.get_weight_by_cards(c1, c2);
            assert!(
                used <= open + 1e-6,
                "100bb: combo ({c1},{c2}) call3+4bet = {used} > open {open}"
            );
        });

        let open50: Range = inputs(PotType::Srp, 50).ip_range.parse().unwrap();
        let call3_50: Range = inputs(PotType::ThreeBet, 50).ip_range.parse().unwrap();
        for_each_combo(|c1, c2| {
            let used = call3_50.get_weight_by_cards(c1, c2);
            let open = open50.get_weight_by_cards(c1, c2);
            assert!(used <= open + 1e-6, "50bb: combo ({c1},{c2}) call3 = {used} > open {open}");
        });
    }

    /// The BB call-vs-4-bet slice must fit inside the 3-bet range per hand (the rest of
    /// the 3-bet mass folds or is the implied 5-bet jam - neither reaches a flop).
    #[test]
    fn fourbet_call_fits_inside_threebet_range() {
        let threebet: Range = inputs(PotType::ThreeBet, 100).oop_range.parse().unwrap();
        let call4: Range = inputs(PotType::FourBet, 100).oop_range.parse().unwrap();
        for_each_combo(|c1, c2| {
            let c = call4.get_weight_by_cards(c1, c2);
            let t = threebet.get_weight_by_cards(c1, c2);
            assert!(c <= t + 1e-6, "combo ({c1},{c2}) 4bet-call {c} > 3bet {t}");
        });
    }

    /// Weighted totals, asserted against the authored reality (stated honestly: the
    /// committed SRP call range keeps the merged middle at full weight, so the consistent
    /// 3-bet range is polar and SMALL - ~5%, not the ~13% of merged HU constructions - and
    /// total defend lands in the realistic vs-2.5x band, low-60s at 100bb).
    #[test]
    fn range_totals_match_the_authored_construction() {
        let combos = 1326.0;
        // BB 3-bet totals
        let t100 = total_weight(&inputs(PotType::ThreeBet, 100).oop_range) / combos;
        let t50 = total_weight(&inputs(PotType::ThreeBet, 50).oop_range) / combos;
        assert!((0.03..=0.08).contains(&t100), "100bb 3-bet {t100:.4} outside [3%, 8%]");
        assert!((0.03..=0.08).contains(&t50), "50bb 3-bet {t50:.4} outside [3%, 8%]");
        // total BB defend vs the open = SRP call + 3-bet
        let d100 = (total_weight(&inputs(PotType::Srp, 100).oop_range)
            + total_weight(&inputs(PotType::ThreeBet, 100).oop_range))
            / combos;
        let d50 = (total_weight(&inputs(PotType::Srp, 50).oop_range)
            + total_weight(&inputs(PotType::ThreeBet, 50).oop_range))
            / combos;
        assert!((0.60..=0.70).contains(&d100), "100bb total defend {d100:.4} outside [60%, 70%]");
        assert!((0.53..=0.62).contains(&d50), "50bb total defend {d50:.4} outside [53%, 62%]");
        // 4bp sizes: the 4-bet range and the call slice are small and value-dense
        let f_ip = total_weight(&inputs(PotType::FourBet, 100).ip_range);
        let f_oop = total_weight(&inputs(PotType::FourBet, 100).oop_range);
        assert!((20.0..=25.0).contains(&f_ip), "4-bet range {f_ip:.1} combos outside [20, 25]");
        assert!((11.0..=17.0).contains(&f_oop), "4-bet call {f_oop:.1} combos outside [11, 17]");
    }
}
