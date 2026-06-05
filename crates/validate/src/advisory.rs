//! ADVISORY theory-plausibility checks - NOT gates. Nothing in this module may panic,
//! assert, fail a build, or be wired into `check_invariants` or a phase verifier.
//!
//! Framing (why advisory): the engine is independently verified (six-check invariant gate,
//! commit c7700d8: zero-sum, full-tree normalization, best-response recheck, suit
//! isomorphism). These checks compare a CORRECT solver's output against GTO theory, so a
//! deviation points at the INPUTS - in practice the authored preflop ranges, the one part
//! of the system not independently verified - or at a legitimately atypical spot. A flag is
//! "worth a look", never a verdict; wiring these as hard gates would false-fire on
//! legitimate spots until the gate got ignored, and an ignored gate is worse than none.
//!
//! Range weights: the aggregates here weight by `read_node_weights` (the solver's
//! normalized reach weights), which carry the authored range weight times card removal -
//! so fractional-weight ranges are range-weight-correct by construction, no separate
//! handling needed.

/// Check A: minimum-defence-frequency floor. Pure pot-odds arithmetic: facing a bet of
/// `bet` into `pot`, the defender must continue with at least `pot / (pot + bet)` of their
/// range or the bettor profits with any two cards. A theoretical floor with a closed form -
/// the HIGHER-confidence check of the two.
///
/// Caveat stated for every report: exact MDF ignores equity realization, future streets,
/// and card removal, so real GTO defence legitimately sits below the pure floor, and the
/// legitimate shortfall is POSITION-DEPENDENT (the solver is the ground truth; MDF is the
/// sanity reference). Measured on the verified srp-v2 benchmark: OOP (BB) defence vs flop
/// c-bets runs +3.0 to +16.7 points below the pure floor across normal spots (equity
/// realizes badly out of position), with one borderline outlier at +20.1 (50bb AcKhQd vs
/// the 33% c-bet: composition-audited legitimate - 100% fold of ~12%-equity air on the one
/// board that crushes the BB caller, exactly MDF's realization blind spot; it sits 0.1 over
/// the threshold and stays flagged rather than retuning the threshold to chase it). IP
/// defence vs BB leads runs -14.2 to +2.4 (at or near the floor everywhere). Thresholds:
/// OOP flags at a 20-point shortfall, IP at 10 (IP has no realization excuse). Lifecycle
/// proof the flag class is real: the srp-v1 outlier (+26.6 on 100bb 9c5d3h, later +32.0
/// under the re-authored ranges) was attributed to a capped BB checking range with no raise
/// outlet, fixed by the srp-v2 flop check-raise, and now measures +3.0 with the mechanism
/// confirmed per hand (defence_trace).
pub const MDF_FLAG_GAP_OOP: f32 = 0.20;
pub const MDF_FLAG_GAP_IP: f32 = 0.10;

#[derive(Debug, Clone)]
pub struct MdfAssessment {
    /// Pot at the node BEFORE the bet, in chips.
    pub pot_chips: f32,
    pub bet_chips: f32,
    /// The theoretical floor `pot / (pot + bet)`.
    pub mdf_floor: f32,
    /// Range-weighted continue frequency (1 - weighted fold) actually played.
    pub actual_defence: f32,
    /// `mdf_floor - actual_defence`; positive = below the floor.
    pub gap: f32,
    /// Whether the defender is out of position (selects the threshold).
    pub defender_oop: bool,
    pub flagged: bool,
}

pub fn assess_mdf(
    pot_chips: f32,
    bet_chips: f32,
    actual_defence: f32,
    defender_oop: bool,
) -> MdfAssessment {
    let mdf_floor = pot_chips / (pot_chips + bet_chips);
    let gap = mdf_floor - actual_defence;
    let threshold = if defender_oop { MDF_FLAG_GAP_OOP } else { MDF_FLAG_GAP_IP };
    MdfAssessment {
        pot_chips,
        bet_chips,
        mdf_floor,
        actual_defence,
        gap,
        defender_oop,
        flagged: gap > threshold,
    }
}

/// Check B: c-bet frequency by board texture - a SOFT HEURISTIC, explicitly
/// lower-confidence than MDF. Theory predicts direction (dry/static boards favour
/// high-frequency small c-bets; wet/dynamic boards lower-frequency, more polar), but the
/// band for a given texture is not a theorem. Only GROSS deviations flag; moderate
/// variation is normal and often correct. Err heavily toward not flagging.
///
/// Bands (gross-violation thresholds, calibrated against the verified benchmark so that no
/// known-good spot flags; srp-v2 benchmark IP c-bet frequencies measured 36.6% (monotone
/// Kh9h4h) to 66.3% (paired-low 7h7d2c) across textures, every one inside its band and
/// tracking the expected direction - the spread narrowed sharply from the raise-less srp-v1
/// generation, where indiscriminate c-betting peaked at 97.9-100% because no check-raise
/// punished it): dry/static textures expect HIGH c-bet, flag only a very low frequency;
/// wet/dynamic textures legitimately range widely, flag only near-degenerate extremes.
#[derive(Debug, Clone)]
pub struct CbetAssessment {
    pub texture: String,
    /// Range-weighted bet frequency of the preflop raiser at the c-bet node.
    pub cbet_freq: f32,
    /// The expected direction for this texture, for the report.
    pub expectation: &'static str,
    pub flagged: bool,
}

pub fn assess_cbet(texture: &str, cbet_freq: f32) -> CbetAssessment {
    // (expectation text, low gross bound, high gross bound)
    let (expectation, lo, hi): (&'static str, f32, f32) = match texture {
        "rainbow-ahigh" | "rainbow-broadway" => {
            ("dry/static: high-frequency small c-bet expected", 0.30, 1.01)
        }
        "rainbow-low-disconnected" => {
            ("dry-low: moderate-to-high c-bet expected", 0.25, 1.01)
        }
        "paired-high" | "paired-low" => {
            ("paired: moderate-to-high small c-bet expected", 0.25, 1.01)
        }
        "twotone-ahigh" | "twotone-broadway" | "twotone-low" => {
            ("two-tone: moderate c-bet, wide legitimate range", 0.20, 0.97)
        }
        "rainbow-mid-connected" => {
            ("connected: lower-frequency, more polar c-bet plausible", 0.15, 0.95)
        }
        "monotone" => {
            ("monotone: low-frequency, polarized c-bet expected", 0.05, 0.90)
        }
        _ => ("unknown texture: no expectation", 0.0, 1.01),
    };
    CbetAssessment {
        texture: texture.to_string(),
        cbet_freq,
        expectation,
        flagged: cbet_freq < lo || cbet_freq > hi,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mdf_floor_arithmetic() {
        // 33 into 100: floor = 100/133 = 0.7519; 75 into 100: floor = 100/175 = 0.5714
        let a = assess_mdf(100.0, 33.0, 0.74, true);
        assert!((a.mdf_floor - 0.7519).abs() < 1e-3);
        let b = assess_mdf(100.0, 75.0, 0.55, true);
        assert!((b.mdf_floor - 0.5714).abs() < 1e-3);
    }

    #[test]
    fn mdf_legitimate_oop_shortfall_does_not_flag() {
        // 5 points below: well inside OOP equity-realization territory
        let a = assess_mdf(100.0, 33.0, 0.70, true);
        assert!(a.gap > 0.0 && !a.flagged);
        // 16.8 points below: just above the srp-v2 measured cluster top (+16.7, 50bb
        // AcKhQd vs the 75% bet) and still under the 20-point threshold
        let b = assess_mdf(100.0, 33.0, 0.584, true);
        assert!(!b.flagged);
    }

    #[test]
    fn mdf_gross_violation_flags_without_panicking() {
        // OOP, 25-point hole vs the 75.2% floor: any-two-cards exploitable, flags
        let a = assess_mdf(100.0, 33.0, 0.50, true);
        assert!(a.flagged);
        assert!((a.gap - 0.2519).abs() < 1e-3);
    }

    #[test]
    fn mdf_threshold_is_position_aware() {
        // a 13-point shortfall flags for IP (no realization excuse) but not for OOP
        let ip = assess_mdf(100.0, 33.0, 0.62, false);
        assert!(ip.flagged);
        let oop = assess_mdf(100.0, 33.0, 0.62, true);
        assert!(!oop.flagged);
    }

    #[test]
    fn cbet_normal_frequencies_do_not_flag() {
        assert!(!assess_cbet("rainbow-broadway", 0.80).flagged);
        assert!(!assess_cbet("monotone", 0.45).flagged);
        assert!(!assess_cbet("rainbow-mid-connected", 0.55).flagged);
        assert!(!assess_cbet("paired-low", 0.60).flagged);
    }

    #[test]
    fn cbet_gross_deviation_flags() {
        // near-never c-betting a dry broadway board is the canonical gross deviation
        assert!(assess_cbet("rainbow-broadway", 0.10).flagged);
        // near-always c-betting a monotone board
        assert!(assess_cbet("monotone", 0.97).flagged);
    }
}
