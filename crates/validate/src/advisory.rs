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

use gto_config::PotType;

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

/// Per-pot calibration state (the measure-then-pin protocol): srp thresholds/bands are
/// measured on the verified srp-v2 benchmark (above). 3bp/4bp calibrations are pinned from
/// PASS-1 MEASUREMENTS of the 3bp-v1/4bp-v1 benchmark - never pre-stated; while
/// `calibrated` is false the report prints measured values labelled UNCALIBRATED and
/// never flags (a threshold nobody measured is noise, not signal).
pub struct PotCalibration {
    pub mdf_flag_gap_oop: f32,
    pub mdf_flag_gap_ip: f32,
    pub calibrated: bool,
}

pub fn calibration_for(pot: PotType) -> PotCalibration {
    match pot {
        PotType::Srp => PotCalibration {
            mdf_flag_gap_oop: MDF_FLAG_GAP_OOP,
            mdf_flag_gap_ip: MDF_FLAG_GAP_IP,
            calibrated: true,
        },
        // Pass-1 measured (3bp-v1 benchmark, 8 spots, 16 gaps per side). OOP (the
        // 3-bettor facing the stab after check-through): -20.9 to +29.4; the three wide
        // gaps (+28.1 7h7d2c, +29.4 Jc9h8c, +27.9 Qs8s5h) were composition-audited via
        // defence_trace on the worst (Jc9h8c): overpairs raise 77.6%, sets 44.7%, strong
        // classes fold 0.0% - the fold mass is the POLAR range's whiffed bluff wing (45%
        // of the range at 24.7% equity), so the gaps are range-composition-driven and
        // legitimate. Honest caveat: at this give-up-heavy node a pure-MDF check has WEAK
        // signal power for a deliberately polar range - the threshold can only catch
        // catastrophic degeneracy. The signal-bearing 3bp node is the IP caller's defence:
        // measured -12.6 to +3.4 everywhere EXCEPT 50bb AcKhQd (+21.6/+24.7), audited as
        // the same realization blind spot as srp's AcKhQd borderline (JJ-22 at 10.5%
        // equity on AKQ folding honestly; every equity-bearing class defends or raises).
        PotType::ThreeBet => PotCalibration {
            mdf_flag_gap_oop: 0.32,
            mdf_flag_gap_ip: 0.27,
            calibrated: true,
        },
        // Pass-1 measured (4bp-v1 benchmark, 2 spots - THIN DATA, re-measure before
        // trusting a borderline flag once more 4bp boards exist). OOP caller vs c-bet:
        // +2.6 (9c5d3h) and +37.9 (Ks7h2c, composition-audited in Phase A: QQ/JJ at 19.6%
        // equity on K-high vs the QQ+/AK 4-bettor folding 100% - range-driven). IP vs the
        // OOP lead: -10.0 / +6.8 (the srp IP threshold holds with thin margin).
        PotType::FourBet => PotCalibration {
            mdf_flag_gap_oop: 0.40,
            mdf_flag_gap_ip: 0.10,
            calibrated: true,
        },
    }
}

/// Per-pot MDF assessment: same arithmetic, the pot family's calibration. Uncalibrated
/// pots report (gap computed, never flagged).
pub fn assess_mdf_pot(
    pot: PotType,
    pot_chips: f32,
    bet_chips: f32,
    actual_defence: f32,
    defender_oop: bool,
) -> MdfAssessment {
    let cal = calibration_for(pot);
    let mdf_floor = pot_chips / (pot_chips + bet_chips);
    let gap = mdf_floor - actual_defence;
    let threshold = if defender_oop { cal.mdf_flag_gap_oop } else { cal.mdf_flag_gap_ip };
    MdfAssessment {
        pot_chips,
        bet_chips,
        mdf_floor,
        actual_defence,
        gap,
        defender_oop,
        flagged: cal.calibrated && gap > threshold,
    }
}

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

/// Per-pot c-bet assessment. srp = the measured bands above. 3bp/4bp c-bet frequencies
/// are RECORD-ONLY by decision after pass 1: the measured 3bp spread is 13.0% to 100.0%
/// (Jc9h8c 13.0 / Qs8s5h 51.6 / 76.7-85.0 on low boards / 99.9-100.0 on broadway-ahigh) -
/// the polar 3-bet range legitimately spans the whole range BY BOARD, and with roughly one
/// sample per texture a "gross band" would be pseudo-precision, not signal. 4bp measured
/// 58.3 / 93.2 on two spots. Bands can be revisited when those benchmarks widen; the
/// calibrated MDF checks above carry the per-pot signal until then.
pub fn assess_cbet_pot(pot: PotType, texture: &str, cbet_freq: f32) -> CbetAssessment {
    match pot {
        PotType::Srp => assess_cbet(texture, cbet_freq),
        PotType::ThreeBet | PotType::FourBet => CbetAssessment {
            texture: texture.to_string(),
            cbet_freq,
            expectation: "recorded only: no calibrated band (polar-range spread 13-100% \
                          by board; samples per texture too small)",
            flagged: false,
        },
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

    #[test]
    fn pot_calibrations_clear_their_measured_audited_bands() {
        // 3bp OOP: the audited-legitimate worst (+29.4, Jc9h8c) must not flag; a gap past
        // the pinned threshold must
        assert!(!assess_mdf_pot(PotType::ThreeBet, 400.0, 132.0, 0.458, true).flagged);
        assert!(assess_mdf_pot(PotType::ThreeBet, 400.0, 132.0, 0.40, true).flagged);
        // 3bp IP: the AcKhQd blind-spot worst (+24.7) must not flag; past the threshold must
        assert!(!assess_mdf_pot(PotType::ThreeBet, 360.0, 270.0, 0.325, false).flagged);
        assert!(assess_mdf_pot(PotType::ThreeBet, 360.0, 119.0, 0.46, false).flagged);
        // 4bp OOP: the Phase-A-audited +37.9 must not flag; past 40 must
        assert!(!assess_mdf_pot(PotType::FourBet, 880.0, 290.0, 0.373, true).flagged);
        assert!(assess_mdf_pot(PotType::FourBet, 880.0, 290.0, 0.34, true).flagged);
        // srp behavior is byte-identical through the pot wrapper
        let a = assess_mdf_pot(PotType::Srp, 100.0, 33.0, 0.55, true);
        let b = assess_mdf(100.0, 33.0, 0.55, true);
        assert_eq!(a.flagged, b.flagged);
        assert!((a.gap - b.gap).abs() < 1e-9);
    }

    #[test]
    fn cbet_3bp_4bp_record_only_never_flags() {
        // record-only by decision: even degenerate-looking frequencies report, never flag
        assert!(!assess_cbet_pot(PotType::ThreeBet, "twotone-broadway", 0.13).flagged);
        assert!(!assess_cbet_pot(PotType::ThreeBet, "rainbow-ahigh", 1.0).flagged);
        assert!(!assess_cbet_pot(PotType::FourBet, "rainbow-broadway", 0.0).flagged);
        // srp still uses the calibrated bands through the same entry point
        assert!(assess_cbet_pot(PotType::Srp, "rainbow-broadway", 0.10).flagged);
    }
}
