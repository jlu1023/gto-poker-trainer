//! The envelope classifier (D39): maps every hero decision in a parsed hand to
//! IN-ENVELOPE (gradeable now) or OUT (skipped, with a precise reason). The coverage
//! breakdown over these reasons doubles as the build-next roadmap.
//!
//! Counting rule (documented for the coverage report): structural skips where hero decisions
//! cannot be trusted or enumerated (not-heads-up, unparseable, no hole cards, antes,
//! non-standard blinds) produce ONE hand-level entry; inside structurally-sound hands, every
//! HERO decision produces its own entry (gradeable or skipped), so street/pot-type gaps are
//! counted in lost-opportunity units.
//!
//! No action translation, ever: a decision is gradeable only if its entire flop action
//! prefix is checks (the only prefix that maps exactly onto the solved tree) and hero's own
//! action lands on a tree action within the size tolerance.

use crate::schema::{ActionEvent, ActionKind, HandRecord, Seat};
use gto_config::{PotType, ReviewConfig};
use serde::{Deserialize, Serialize};

/// Preflop geometry guards. Canonical lines (mirroring gto-preflop's authored geometry,
/// where the solved spots' pots come from): open 2.5bb; 3-bet to 10bb @100 / 9bb @50;
/// 4-bet to 22bb @100 only (50bb 4-bet pots are SPR ~0.75 jam-or-fold, out of scope by
/// decision). The bands absorb real-hand variance around the canonical sizes; anything
/// outside skips with a precise reason - never nearest-mapped.
const OPEN_BAND_BB: (f64, f64) = (2.3, 2.7);
const THREEBET_BAND_BB_100: (f64, f64) = (9.0, 11.5);
const THREEBET_BAND_BB_50: (f64, f64) = (8.0, 10.5);
const FOURBET_BAND_BB_100: (f64, f64) = (19.0, 26.0);
const DEPTH_BUCKETS: [u32; 2] = [50, 100];
const DEPTH_TOLERANCE: f64 = 0.07;

/// Flop OPENING sizes per (pot type, depth) as (fraction of pot, solved-tree action label).
/// These are the graded check-to nodes only; raise lines are in-tree but not graded shapes
/// and never reach this mapping. The LABELS are chip-denominated and must byte-match the
/// solved trees (chips = fraction x pot x chip_per_bb): srp pot 5bb = 100 chips at both
/// depths; 3bp pot 20bb/18bb = 400/360 chips; 4bp pot 44bb = 880 chips, single 33% size.
/// The byte-match is pinned END TO END by verify_review's synthetic hands grading against
/// the real matrix (a wrong label fails with "action not in node action set"), not by
/// arithmetic assumptions here.
fn tree_sizes_for(pot: PotType, depth_bb: u32) -> &'static [(f64, &'static str)] {
    match (pot, depth_bb) {
        (PotType::Srp, _) => &[(0.33, "Bet(33)"), (0.75, "Bet(75)")],
        (PotType::ThreeBet, 100) => &[(0.33, "Bet(132)"), (0.75, "Bet(300)")],
        (PotType::ThreeBet, _) => &[(0.33, "Bet(119)"), (0.75, "Bet(270)")],
        (PotType::FourBet, _) => &[(0.33, "Bet(290)")],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkipReason {
    NotHeadsUp,
    UnparseableHand,
    HeroCardsUnknown,
    AntePot,
    NonStandardBlinds,
    LimpedPot,
    /// Five or more preflop raises (beyond the modeled 4-bet line).
    FiveBetPot,
    OpenSizeOutOfBand,
    /// The 3-bet-to amount is outside the canonical band for the depth.
    ThreebetSizeOutOfBand,
    /// The 4-bet-to amount is outside the canonical band.
    FourbetSizeOutOfBand,
    /// A 4-bet pot at 50bb: out of scope by decision (SPR ~0.75 jam-or-fold).
    FourbetDepthOutOfScope,
    DepthOutOfBand,
    PreflopDecision,
    HeroFoldedPreflop,
    NoPostflopDecision,
    TurnDecision,
    RiverDecision,
    FacingBetOnFlop,
    OffTreeHeroSize,
    /// Assigned at ingest when only a `-shrunk` solve exists (its action set lacks Bet(75);
    /// folding it into the size split would corrupt the report).
    SolveShrunkSizeUnavailable,
    /// A coverage STATE, not a skip: gradeable but not yet solved under the per-run cap.
    PendingSolve,
}

impl SkipReason {
    pub fn key(self) -> &'static str {
        match self {
            SkipReason::NotHeadsUp => "not-heads-up",
            SkipReason::UnparseableHand => "unparseable-hand",
            SkipReason::HeroCardsUnknown => "hero-cards-unknown",
            SkipReason::AntePot => "ante-pot",
            SkipReason::NonStandardBlinds => "non-standard-blinds",
            SkipReason::LimpedPot => "limped-pot",
            SkipReason::FiveBetPot => "five-bet-pot",
            SkipReason::OpenSizeOutOfBand => "open-size-out-of-band",
            SkipReason::ThreebetSizeOutOfBand => "threebet-size-out-of-band",
            SkipReason::FourbetSizeOutOfBand => "fourbet-size-out-of-band",
            SkipReason::FourbetDepthOutOfScope => "fourbet-depth-out-of-scope",
            SkipReason::DepthOutOfBand => "depth-out-of-band",
            SkipReason::PreflopDecision => "preflop-decision",
            SkipReason::HeroFoldedPreflop => "hero-folded-preflop",
            SkipReason::NoPostflopDecision => "no-postflop-decision",
            SkipReason::TurnDecision => "turn-decision",
            SkipReason::RiverDecision => "river-decision",
            SkipReason::FacingBetOnFlop => "facing-bet-on-flop",
            SkipReason::OffTreeHeroSize => "off-tree-hero-size",
            SkipReason::SolveShrunkSizeUnavailable => "solve-shrunk-size-unavailable",
            SkipReason::PendingSolve => "pending-solve",
        }
    }
}

/// The graded node shapes. HU postflop order is FIXED: the BB (always OOP) acts first in
/// every pot type - what flips per pot is the AGGRESSOR, so structurally-identical nodes
/// carry pot-distinct shape strings and aggregation never pools across pots (srp root:
/// non-aggressor check-or-lead; 3bp root: the OOP AGGRESSOR's c-bet node; 4bp root: the
/// OOP caller vs the IP 4-bettor). The srp names keep their pre-pot strings so stored
/// records stay readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeShape {
    BbRoot,
    IpAfterCheck,
    #[serde(rename = "3bp-oop-root")]
    ThreeBetOopRoot,
    #[serde(rename = "3bp-ip-after-check")]
    ThreeBetIpAfterCheck,
    #[serde(rename = "4bp-oop-root")]
    FourBetOopRoot,
    #[serde(rename = "4bp-ip-after-check")]
    FourBetIpAfterCheck,
}

impl NodeShape {
    pub fn key(self) -> &'static str {
        match self {
            NodeShape::BbRoot => "bb-root",
            NodeShape::IpAfterCheck => "ip-after-check",
            NodeShape::ThreeBetOopRoot => "3bp-oop-root",
            NodeShape::ThreeBetIpAfterCheck => "3bp-ip-after-check",
            NodeShape::FourBetOopRoot => "4bp-oop-root",
            NodeShape::FourBetIpAfterCheck => "4bp-ip-after-check",
        }
    }

    /// Whether the acting player at this shape is the OOP player (selects the hero range).
    pub fn hero_is_oop(self) -> bool {
        matches!(
            self,
            NodeShape::BbRoot | NodeShape::ThreeBetOopRoot | NodeShape::FourBetOopRoot
        )
    }
}

fn default_pot_type() -> PotType {
    PotType::Srp
}

/// A decision the engine can grade now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradeableDecision {
    pub hand_id: String,
    pub street: String, // "flop"
    pub decision_index: u32,
    pub depth_bb: u32,
    /// The preflop pot family; selects the solved tree (srp default for back-compat).
    #[serde(default = "default_pot_type")]
    pub pot_type: PotType,
    pub node_shape: NodeShape,
    /// Canonical board order (gto_store::canonical_flop).
    pub board: [String; 3],
    pub combo: String,
    /// Tree action label the hero's real action mapped to (Check exact; bets only within
    /// the size tolerance).
    pub action_taken: String,
    pub open_size_bb: f64,
    pub eff_stack_bb: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Classified {
    Gradeable(GradeableDecision),
    Skipped {
        hand_id: String,
        decision_index: Option<u32>,
        reason: SkipReason,
        detail: String,
    },
}

/// Classify every hero decision in one hand. Always returns at least one entry per hand.
pub fn classify_hand(h: &HandRecord, cfg: &ReviewConfig) -> Vec<Classified> {
    let skip_hand = |reason: SkipReason, detail: String| {
        vec![Classified::Skipped { hand_id: h.hand_id.clone(), decision_index: None, reason, detail }]
    };

    // ---- structural, hand-level ----
    if h.table_size_dealt != 2 {
        return skip_hand(SkipReason::NotHeadsUp, format!("{} players dealt", h.table_size_dealt));
    }
    if h.hero_cards.is_none() {
        return skip_hand(SkipReason::HeroCardsUnknown, "no 'Dealt to' hole cards".to_string());
    }
    if h.ante_bb > 0.0 {
        return skip_hand(SkipReason::AntePot, format!("ante {} bb", h.ante_bb));
    }
    if (h.blinds_bb.0 - 0.5).abs() > 1e-9
        || (h.blinds_bb.1 - 1.0).abs() > 1e-9
        || h.preflop.iter().any(|a| a.kind == ActionKind::Post)
    {
        return skip_hand(
            SkipReason::NonStandardBlinds,
            format!("blinds {:?} or extra posts", h.blinds_bb),
        );
    }

    // ---- preflop structure: pot type + hero preflop decisions ----
    let mut entries: Vec<Classified> = Vec::new();
    let mut decision_index: u32 = 0;
    let hero = h.hero_seat;

    let push_skip = |entries: &mut Vec<Classified>, idx: &mut u32, reason: SkipReason, detail: String| {
        entries.push(Classified::Skipped {
            hand_id: h.hand_id.clone(),
            decision_index: Some(*idx),
            reason,
            detail,
        });
        *idx += 1;
    };

    // pot-line classification over the whole preflop line (seat-checked raise sequence)
    let pot_line = classify_preflop(&h.preflop);
    let in_envelope_pot = matches!(
        pot_line,
        PotLine::Srp { .. } | PotLine::ThreeBet { .. } | PotLine::FourBet { .. }
    );

    // per-decision entries for hero's preflop actions (preflop is never graded; the pot
    // family it produced is what enters the envelope)
    for ev in &h.preflop {
        if ev.seat != hero {
            continue;
        }
        let reason = if ev.kind == ActionKind::Fold {
            SkipReason::HeroFoldedPreflop
        } else if !in_envelope_pot {
            match pot_line {
                PotLine::Limped => SkipReason::LimpedPot,
                PotLine::FiveBetPlus => SkipReason::FiveBetPot,
                PotLine::Irregular => SkipReason::UnparseableHand,
                _ => SkipReason::PreflopDecision,
            }
        } else {
            SkipReason::PreflopDecision
        };
        push_skip(&mut entries, &mut decision_index, reason, format!("{:?}", ev.kind));
    }

    if !in_envelope_pot {
        // pot family out of envelope: tag any postflop hero decisions with its reason
        let reason = match pot_line {
            PotLine::Limped => SkipReason::LimpedPot,
            PotLine::FiveBetPlus => SkipReason::FiveBetPot,
            PotLine::Irregular => SkipReason::UnparseableHand,
            PotLine::NoFlop => {
                if entries.is_empty() {
                    push_skip(&mut entries, &mut decision_index, SkipReason::NoPostflopDecision,
                        "hand ended preflop with no hero decision".to_string());
                }
                return entries;
            }
            _ => unreachable!(),
        };
        for (street, n) in hero_postflop_decisions(h, hero) {
            for _ in 0..n {
                push_skip(&mut entries, &mut decision_index, reason, format!("{street} decision in out-of-envelope pot"));
            }
        }
        if entries.is_empty() {
            push_skip(&mut entries, &mut decision_index, reason, "no hero decision parsed".to_string());
        }
        return entries;
    }

    // ---- geometry guards (tight; D39): depth bucket first, then every raise-to in its
    // canonical band for the pot family at that depth ----
    let eff = h.stacks_bb.btn.min(h.stacks_bb.bb);
    let depth_bucket = DEPTH_BUCKETS
        .iter()
        .copied()
        .find(|&d| ((eff - d as f64) / d as f64).abs() <= DEPTH_TOLERANCE);
    let geometry_skip = if depth_bucket.is_none() {
        Some((SkipReason::DepthOutOfBand, format!("eff {eff:.1} bb (buckets {DEPTH_BUCKETS:?} +-{:.0}%)", DEPTH_TOLERANCE * 100.0)))
    } else {
        let depth = depth_bucket.unwrap();
        band_skip(&pot_line, depth)
    };
    if let Some((reason, detail)) = geometry_skip {
        for (street, n) in hero_postflop_decisions(h, hero) {
            for _ in 0..n {
                push_skip(&mut entries, &mut decision_index, reason, format!("{detail}; {street}"));
            }
        }
        if entries.is_empty() {
            push_skip(&mut entries, &mut decision_index, reason, detail);
        }
        return entries;
    }
    let depth_bb = depth_bucket.unwrap();
    let (pot_type, open_size_bb, last_raise_to_bb) = match pot_line {
        PotLine::Srp { open_to_bb } => (PotType::Srp, open_to_bb, open_to_bb),
        PotLine::ThreeBet { open_to_bb, threebet_to_bb } => {
            (PotType::ThreeBet, open_to_bb, threebet_to_bb)
        }
        PotLine::FourBet { open_to_bb, fourbet_to_bb, .. } => {
            (PotType::FourBet, open_to_bb, fourbet_to_bb)
        }
        _ => unreachable!(),
    };

    // ---- flop decisions ----
    let Some(flop) = &h.flop else {
        // SRP but no flop: villain folded to the open or similar; hero decisions already entered
        if entries.is_empty() {
            push_skip(&mut entries, &mut decision_index, SkipReason::NoPostflopDecision,
                "no flop dealt".to_string());
        }
        return entries;
    };

    let board = match gto_store::canonical_flop(&flop.cards.to_vec()) {
        Ok(b) => b,
        Err(e) => {
            push_skip(&mut entries, &mut decision_index, SkipReason::UnparseableHand,
                format!("bad flop: {e}"));
            return entries;
        }
    };
    // the ACTUAL pot from the hand's own amounts (uniform HU law: the last raise-to,
    // flat-called, doubles): hero-size mapping is relative to the real pot the hero bet
    // into. Grading/D45 scale by the SOLVED spot's pot instead (review.rs) - two pots,
    // two jobs, never crossed.
    let pot_bb = 2.0 * last_raise_to_bb;

    for (k, ev) in flop.actions.iter().enumerate() {
        if ev.seat != hero {
            continue;
        }
        let prefix = &flop.actions[..k];
        let prefix_all_checks = prefix.iter().all(|p| p.kind == ActionKind::Check);
        if !prefix_all_checks {
            push_skip(&mut entries, &mut decision_index, SkipReason::FacingBetOnFlop,
                "a bet precedes this decision (bet-facing flop nodes are not graded node \
                 shapes; in-tree since srp-v2 but ungraded)".to_string());
            continue;
        }
        // check-only prefix: the node shape is fully determined by (pot family, seat,
        // index). HU postflop order is fixed (BB first in EVERY pot type); the pot family
        // distinguishes strategically-different nodes at identical tree positions.
        let shape = match (pot_type, hero, k) {
            (PotType::Srp, Seat::Bb, 0) => NodeShape::BbRoot,
            (PotType::Srp, Seat::Btn, 1) => NodeShape::IpAfterCheck,
            (PotType::ThreeBet, Seat::Bb, 0) => NodeShape::ThreeBetOopRoot,
            (PotType::ThreeBet, Seat::Btn, 1) => NodeShape::ThreeBetIpAfterCheck,
            (PotType::FourBet, Seat::Bb, 0) => NodeShape::FourBetOopRoot,
            (PotType::FourBet, Seat::Btn, 1) => NodeShape::FourBetIpAfterCheck,
            _ => {
                // e.g. parser glitch (hero BTN acting first HU postflop is impossible)
                push_skip(&mut entries, &mut decision_index, SkipReason::UnparseableHand,
                    format!("impossible flop action order (hero {} at index {k})", hero.label()));
                continue;
            }
        };
        // hero's own action must land on the pot family's tree at this depth
        let action_taken = match map_hero_action(ev, pot_bb, pot_type, depth_bb, cfg) {
            Ok(a) => a,
            Err(detail) => {
                push_skip(&mut entries, &mut decision_index, SkipReason::OffTreeHeroSize, detail);
                continue;
            }
        };
        entries.push(Classified::Gradeable(GradeableDecision {
            hand_id: h.hand_id.clone(),
            street: "flop".to_string(),
            decision_index,
            depth_bb,
            pot_type,
            node_shape: shape,
            board: board.clone(),
            combo: h.hero_cards.clone().unwrap(),
            action_taken,
            open_size_bb,
            eff_stack_bb: eff,
        }));
        decision_index += 1;
    }

    // ---- later streets: classification only ----
    for (street, reason) in [("turn", SkipReason::TurnDecision), ("river", SkipReason::RiverDecision)] {
        let n = match street {
            "turn" => h.turn.as_ref().map_or(0, |s| s.actions.iter().filter(|a| a.seat == hero).count()),
            _ => h.river.as_ref().map_or(0, |s| s.actions.iter().filter(|a| a.seat == hero).count()),
        };
        for _ in 0..n {
            push_skip(&mut entries, &mut decision_index, reason, format!("{street} decision"));
        }
    }

    if entries.is_empty() {
        push_skip(&mut entries, &mut decision_index, SkipReason::NoPostflopDecision,
            "no hero decision parsed".to_string());
    }
    entries
}

/// The classified preflop line (internal; richer than gto_config::PotType because it
/// carries the raise-to amounts the band guards and the actual-pot arithmetic need).
enum PotLine {
    Srp { open_to_bb: f64 },
    ThreeBet { open_to_bb: f64, threebet_to_bb: f64 },
    FourBet { open_to_bb: f64, threebet_to_bb: f64, fourbet_to_bb: f64 },
    /// Five or more raises: beyond the modeled tree.
    FiveBetPlus,
    Limped,
    /// A raise sequence whose seats do not alternate BTN/BB/BTN (parser glitch or a line
    /// HU structure cannot produce) - unparseable, never guessed.
    Irregular,
    NoFlop, // hand ended preflop without forming a pot type (e.g. BTN open-folds: walk)
}

/// HU preflop structure: BTN acts first. The raise SEQUENCE (with seats) determines the
/// family: [Btn] = SRP; [Btn, Bb] = 3-bet pot; [Btn, Bb, Btn] = 4-bet pot; more = 5-bet+.
/// A limp before any raise puts the hand in the limped family (limp-raise lines included:
/// their geometry matches no solved spot).
fn classify_preflop(actions: &[ActionEvent]) -> PotLine {
    let live: Vec<&ActionEvent> = actions.iter().filter(|a| a.kind != ActionKind::Post).collect();
    // a BTN limp (call of the bb) or fold before any raise decides the family up front
    match live.first() {
        Some(first) if first.kind == ActionKind::Call => return PotLine::Limped,
        Some(first) if first.kind == ActionKind::Fold => return PotLine::NoFlop, // walk
        Some(_) => {}
        None => return PotLine::NoFlop,
    }
    let raises: Vec<&&ActionEvent> =
        live.iter().filter(|a| a.kind == ActionKind::Raise).collect();
    match raises.len() {
        0 => PotLine::Limped, // checks only (BB checks option after limp) - limped family
        1 => {
            if raises[0].seat != Seat::Btn {
                return PotLine::Irregular;
            }
            PotLine::Srp { open_to_bb: raises[0].amount_bb }
        }
        2 => {
            if raises[0].seat != Seat::Btn || raises[1].seat != Seat::Bb {
                return PotLine::Irregular;
            }
            PotLine::ThreeBet {
                open_to_bb: raises[0].amount_bb,
                threebet_to_bb: raises[1].amount_bb,
            }
        }
        3 => {
            if raises[0].seat != Seat::Btn
                || raises[1].seat != Seat::Bb
                || raises[2].seat != Seat::Btn
            {
                return PotLine::Irregular;
            }
            PotLine::FourBet {
                open_to_bb: raises[0].amount_bb,
                threebet_to_bb: raises[1].amount_bb,
                fourbet_to_bb: raises[2].amount_bb,
            }
        }
        _ => PotLine::FiveBetPlus,
    }
}

/// Every raise-to in the line must sit in its canonical band for the depth; 4-bet pots
/// exist at 100bb only (decision). Returns the skip if any guard fires.
fn band_skip(line: &PotLine, depth_bb: u32) -> Option<(SkipReason, String)> {
    let open_band = |open: f64| {
        (!(OPEN_BAND_BB.0..=OPEN_BAND_BB.1).contains(&open)).then(|| {
            (SkipReason::OpenSizeOutOfBand, format!("open {open:.2} bb (band {OPEN_BAND_BB:?})"))
        })
    };
    let threebet_band = |tb: f64| {
        let band = if depth_bb == 100 { THREEBET_BAND_BB_100 } else { THREEBET_BAND_BB_50 };
        (!(band.0..=band.1).contains(&tb)).then(|| {
            (SkipReason::ThreebetSizeOutOfBand,
             format!("3-bet to {tb:.2} bb (band {band:?} at {depth_bb}bb)"))
        })
    };
    match *line {
        PotLine::Srp { open_to_bb } => open_band(open_to_bb),
        PotLine::ThreeBet { open_to_bb, threebet_to_bb } => {
            open_band(open_to_bb).or_else(|| threebet_band(threebet_to_bb))
        }
        PotLine::FourBet { open_to_bb, threebet_to_bb, fourbet_to_bb } => {
            if depth_bb != 100 {
                return Some((SkipReason::FourbetDepthOutOfScope,
                    format!("4-bet pot at {depth_bb}bb (modeled at 100bb only)")));
            }
            open_band(open_to_bb).or_else(|| threebet_band(threebet_to_bb)).or_else(|| {
                (!(FOURBET_BAND_BB_100.0..=FOURBET_BAND_BB_100.1).contains(&fourbet_to_bb))
                    .then(|| (SkipReason::FourbetSizeOutOfBand,
                        format!("4-bet to {fourbet_to_bb:.2} bb (band {FOURBET_BAND_BB_100:?})")))
            })
        }
        _ => None,
    }
}

/// Count hero decision points per postflop street (for tagging out-of-envelope pots).
fn hero_postflop_decisions(h: &HandRecord, hero: Seat) -> Vec<(&'static str, usize)> {
    let mut v = Vec::new();
    if let Some(f) = &h.flop {
        v.push(("flop", f.actions.iter().filter(|a| a.seat == hero).count()));
    }
    if let Some(t) = &h.turn {
        v.push(("turn", t.actions.iter().filter(|a| a.seat == hero).count()));
    }
    if let Some(r) = &h.river {
        v.push(("river", r.actions.iter().filter(|a| a.seat == hero).count()));
    }
    v
}

/// Map hero's real flop action onto the pot family's tree at this depth: Check exact; a
/// bet only within the configured tolerance of a tree size (else Err ->
/// off-tree-hero-size). Never nearest-map.
fn map_hero_action(
    ev: &ActionEvent,
    pot_bb: f64,
    pot_type: PotType,
    depth_bb: u32,
    cfg: &ReviewConfig,
) -> Result<String, String> {
    match ev.kind {
        ActionKind::Check => Ok("Check".to_string()),
        ActionKind::Bet => {
            if ev.all_in {
                return Err(format!("all-in bet {:.2} bb (no all-in at the graded root nodes)", ev.amount_bb));
            }
            let sizes = tree_sizes_for(pot_type, depth_bb);
            let pct = ev.amount_bb / pot_bb;
            for &(size, label) in sizes {
                if (pct - size).abs() <= cfg.hero_size_tolerance_pct_pot {
                    return Ok(label.to_string());
                }
            }
            let listed: Vec<String> =
                sizes.iter().map(|(s, _)| format!("{:.0}%", s * 100.0)).collect();
            Err(format!(
                "bet {:.2} bb = {:.0}% pot; {} tree sizes {} (+-{:.0}% tolerance)",
                ev.amount_bb,
                pct * 100.0,
                pot_type.label(),
                listed.join("/"),
                cfg.hero_size_tolerance_pct_pot * 100.0
            ))
        }
        ActionKind::Fold => Err("fold at a check-to node (not a tree action here)".to_string()),
        other => Err(format!("{other:?} does not map to a tree action at this node")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::*;

    fn base_hand() -> HandRecord {
        HandRecord {
            hand_id: "T1".into(),
            source: "test".into(),
            table_size_dealt: 2,
            hero_seat: Seat::Bb,
            stacks_bb: Stacks { btn: 100.0, bb: 100.0 },
            blinds_bb: (0.5, 1.0),
            ante_bb: 0.0,
            hero_cards: Some("AhKh".into()),
            preflop: vec![
                ActionEvent { seat: Seat::Btn, kind: ActionKind::Raise, amount_bb: 2.5, all_in: false },
                ActionEvent { seat: Seat::Bb, kind: ActionKind::Call, amount_bb: 1.5, all_in: false },
            ],
            flop: Some(FlopStreet {
                cards: ["Ks".into(), "7h".into(), "2c".into()],
                actions: vec![ActionEvent { seat: Seat::Bb, kind: ActionKind::Check, amount_bb: 0.0, all_in: false }],
            }),
            turn: None,
            river: None,
        }
    }

    fn cfg() -> gto_config::ReviewConfig {
        gto_config::ReviewConfig::default()
    }

    fn gradeable(entries: &[Classified]) -> Vec<&GradeableDecision> {
        entries.iter().filter_map(|c| match c {
            Classified::Gradeable(g) => Some(g),
            _ => None,
        }).collect()
    }

    fn reasons(entries: &[Classified]) -> Vec<SkipReason> {
        entries.iter().filter_map(|c| match c {
            Classified::Skipped { reason, .. } => Some(*reason),
            _ => None,
        }).collect()
    }

    #[test]
    fn bb_root_check_is_gradeable() {
        let entries = classify_hand(&base_hand(), &cfg());
        let g = gradeable(&entries);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].node_shape, NodeShape::BbRoot);
        assert_eq!(g[0].action_taken, "Check");
        assert_eq!(g[0].depth_bb, 100);
        assert_eq!(g[0].board, ["Ks", "7h", "2c"]);
        // hero's preflop call is a skipped preflop decision
        assert!(reasons(&entries).contains(&SkipReason::PreflopDecision));
    }

    #[test]
    fn bb_root_bet_within_tolerance_maps() {
        let mut h = base_hand();
        // pot = 5bb; 33% = 1.65bb; bet 1.7bb is within 8%-of-pot tolerance (|0.34-0.33|=0.01)
        h.flop.as_mut().unwrap().actions = vec![ActionEvent {
            seat: Seat::Bb, kind: ActionKind::Bet, amount_bb: 1.7, all_in: false,
        }];
        let entries = classify_hand(&h, &cfg());
        assert_eq!(gradeable(&entries)[0].action_taken, "Bet(33)");
    }

    #[test]
    fn off_tree_hero_size_is_skipped_not_mapped() {
        let mut h = base_hand();
        // 50% pot = 2.5bb: 0.50 is 0.17 from 0.33 and 0.25 from 0.75 -> outside 0.08 tolerance
        h.flop.as_mut().unwrap().actions = vec![ActionEvent {
            seat: Seat::Bb, kind: ActionKind::Bet, amount_bb: 2.5, all_in: false,
        }];
        let entries = classify_hand(&h, &cfg());
        assert!(gradeable(&entries).is_empty());
        assert!(reasons(&entries).contains(&SkipReason::OffTreeHeroSize));
    }

    #[test]
    fn ip_after_check_is_gradeable_and_facing_bet_is_not() {
        let mut h = base_hand();
        h.hero_seat = Seat::Btn;
        h.flop.as_mut().unwrap().actions = vec![
            ActionEvent { seat: Seat::Bb, kind: ActionKind::Check, amount_bb: 0.0, all_in: false },
            ActionEvent { seat: Seat::Btn, kind: ActionKind::Check, amount_bb: 0.0, all_in: false },
        ];
        let entries = classify_hand(&h, &cfg());
        let g = gradeable(&entries);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].node_shape, NodeShape::IpAfterCheck);

        // BB bets first instead: BTN faces a bet -> off-tree prefix
        h.flop.as_mut().unwrap().actions = vec![
            ActionEvent { seat: Seat::Bb, kind: ActionKind::Bet, amount_bb: 1.65, all_in: false },
            ActionEvent { seat: Seat::Btn, kind: ActionKind::Call, amount_bb: 1.65, all_in: false },
        ];
        let entries = classify_hand(&h, &cfg());
        assert!(gradeable(&entries).is_empty());
        assert!(reasons(&entries).contains(&SkipReason::FacingBetOnFlop));
    }

    fn threebet_hand(threebet_to: f64) -> HandRecord {
        let mut h = base_hand();
        h.preflop = vec![
            ActionEvent { seat: Seat::Btn, kind: ActionKind::Raise, amount_bb: 2.5, all_in: false },
            ActionEvent { seat: Seat::Bb, kind: ActionKind::Raise, amount_bb: threebet_to, all_in: false },
            ActionEvent { seat: Seat::Btn, kind: ActionKind::Call, amount_bb: threebet_to - 2.5, all_in: false },
        ];
        h
    }

    fn fourbet_hand(fourbet_to: f64) -> HandRecord {
        let mut h = base_hand();
        h.preflop = vec![
            ActionEvent { seat: Seat::Btn, kind: ActionKind::Raise, amount_bb: 2.5, all_in: false },
            ActionEvent { seat: Seat::Bb, kind: ActionKind::Raise, amount_bb: 10.0, all_in: false },
            ActionEvent { seat: Seat::Btn, kind: ActionKind::Raise, amount_bb: fourbet_to, all_in: false },
            ActionEvent { seat: Seat::Bb, kind: ActionKind::Call, amount_bb: fourbet_to - 10.0, all_in: false },
        ];
        h
    }

    #[test]
    fn three_bet_pot_is_now_in_envelope() {
        // canonical 3-bet to 10 at 100bb: the BB (OOP AGGRESSOR) checking the flop is the
        // 3bp-oop-root shape, pot family ThreeBet
        let entries = classify_hand(&threebet_hand(10.0), &cfg());
        let g = gradeable(&entries);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].node_shape, NodeShape::ThreeBetOopRoot);
        assert_eq!(g[0].pot_type, PotType::ThreeBet);

        // the BB c-bet maps against the ACTUAL pot (2 x 10 = 20bb): 6.6bb = 33% -> Bet(132)
        let mut h = threebet_hand(10.0);
        h.flop.as_mut().unwrap().actions = vec![ActionEvent {
            seat: Seat::Bb, kind: ActionKind::Bet, amount_bb: 6.6, all_in: false,
        }];
        let entries = classify_hand(&h, &cfg());
        assert_eq!(gradeable(&entries)[0].action_taken, "Bet(132)");

        // BTN after the BB checks = 3bp-ip-after-check
        let mut h = threebet_hand(10.0);
        h.hero_seat = Seat::Btn;
        h.flop.as_mut().unwrap().actions = vec![
            ActionEvent { seat: Seat::Bb, kind: ActionKind::Check, amount_bb: 0.0, all_in: false },
            ActionEvent { seat: Seat::Btn, kind: ActionKind::Check, amount_bb: 0.0, all_in: false },
        ];
        let entries = classify_hand(&h, &cfg());
        assert_eq!(gradeable(&entries)[0].node_shape, NodeShape::ThreeBetIpAfterCheck);
    }

    #[test]
    fn threebet_size_band_guards() {
        // 14bb 3-bet at 100bb: outside [9.0, 11.5] -> precise skip, never nearest-mapped
        let entries = classify_hand(&threebet_hand(14.0), &cfg());
        assert!(gradeable(&entries).is_empty());
        assert!(reasons(&entries).contains(&SkipReason::ThreebetSizeOutOfBand));

        // 9bb at 50bb is canonical there
        let mut h = threebet_hand(9.0);
        h.stacks_bb = Stacks { btn: 50.0, bb: 50.0 };
        let entries = classify_hand(&h, &cfg());
        assert_eq!(gradeable(&entries)[0].depth_bb, 50);
        assert_eq!(gradeable(&entries)[0].pot_type, PotType::ThreeBet);
    }

    #[test]
    fn four_bet_pot_at_100_in_envelope_at_50_out_of_scope() {
        let entries = classify_hand(&fourbet_hand(22.0), &cfg());
        let g = gradeable(&entries);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].node_shape, NodeShape::FourBetOopRoot);
        assert_eq!(g[0].pot_type, PotType::FourBet);

        // the BB donk into the 44bb pot: 14.5bb = 33% -> Bet(290) (single 4bp size)
        let mut h = fourbet_hand(22.0);
        h.flop.as_mut().unwrap().actions = vec![ActionEvent {
            seat: Seat::Bb, kind: ActionKind::Bet, amount_bb: 14.5, all_in: false,
        }];
        assert_eq!(gradeable(&classify_hand(&h, &cfg()))[0].action_taken, "Bet(290)");

        // 4-bet size out of band
        let entries = classify_hand(&fourbet_hand(30.0), &cfg());
        assert!(reasons(&entries).contains(&SkipReason::FourbetSizeOutOfBand));

        // 50bb 4-bet pots are out of scope BY DECISION (jam-or-fold)
        let mut h = fourbet_hand(20.0);
        h.stacks_bb = Stacks { btn: 50.0, bb: 50.0 };
        let entries = classify_hand(&h, &cfg());
        assert!(gradeable(&entries).is_empty());
        assert!(reasons(&entries).contains(&SkipReason::FourbetDepthOutOfScope));
    }

    #[test]
    fn five_bet_and_irregular_lines_skip_precisely() {
        let mut h = base_hand();
        h.preflop = vec![
            ActionEvent { seat: Seat::Btn, kind: ActionKind::Raise, amount_bb: 2.5, all_in: false },
            ActionEvent { seat: Seat::Bb, kind: ActionKind::Raise, amount_bb: 10.0, all_in: false },
            ActionEvent { seat: Seat::Btn, kind: ActionKind::Raise, amount_bb: 22.0, all_in: false },
            ActionEvent { seat: Seat::Bb, kind: ActionKind::Raise, amount_bb: 100.0, all_in: true },
            ActionEvent { seat: Seat::Btn, kind: ActionKind::Call, amount_bb: 78.0, all_in: true },
        ];
        let entries = classify_hand(&h, &cfg());
        assert!(gradeable(&entries).is_empty());
        assert!(reasons(&entries).contains(&SkipReason::FiveBetPot));

        // a first raise from the BB without a BTN limp is structurally impossible HU
        let mut h = base_hand();
        h.preflop = vec![
            ActionEvent { seat: Seat::Bb, kind: ActionKind::Raise, amount_bb: 3.0, all_in: false },
            ActionEvent { seat: Seat::Btn, kind: ActionKind::Call, amount_bb: 2.0, all_in: false },
        ];
        let entries = classify_hand(&h, &cfg());
        assert!(gradeable(&entries).is_empty());
        assert!(reasons(&entries).contains(&SkipReason::UnparseableHand));
    }

    #[test]
    fn limped_pots_stay_skipped() {
        let mut h = base_hand();
        h.preflop = vec![
            ActionEvent { seat: Seat::Btn, kind: ActionKind::Call, amount_bb: 0.5, all_in: false },
            ActionEvent { seat: Seat::Bb, kind: ActionKind::Check, amount_bb: 0.0, all_in: false },
        ];
        let entries = classify_hand(&h, &cfg());
        assert!(gradeable(&entries).is_empty());
        assert!(reasons(&entries).contains(&SkipReason::LimpedPot));
    }

    #[test]
    fn gradeable_decision_back_compat_defaults_pot_type_to_srp() {
        // pre-pot serialized decisions (no pot_type field) must deserialize as srp
        let old = r#"{"hand_id":"H1","street":"flop","decision_index":0,"depth_bb":100,
            "node_shape":"bb-root","board":["Ks","7h","2c"],"combo":"AhKh",
            "action_taken":"Check","open_size_bb":2.5,"eff_stack_bb":100.0}"#;
        let d: GradeableDecision = serde_json::from_str(old).unwrap();
        assert_eq!(d.pot_type, PotType::Srp);
        assert_eq!(d.node_shape, NodeShape::BbRoot);
    }

    #[test]
    fn geometry_guards_skip_tightly() {
        let mut h = base_hand();
        h.preflop[0].amount_bb = 3.0; // open out of [2.3, 2.7]
        let entries = classify_hand(&h, &cfg());
        assert!(gradeable(&entries).is_empty());
        assert!(reasons(&entries).contains(&SkipReason::OpenSizeOutOfBand));

        let mut h = base_hand();
        h.stacks_bb = Stacks { btn: 75.0, bb: 75.0 }; // not within 7% of 50 or 100
        let entries = classify_hand(&h, &cfg());
        assert!(gradeable(&entries).is_empty());
        assert!(reasons(&entries).contains(&SkipReason::DepthOutOfBand));

        let mut h = base_hand();
        h.stacks_bb = Stacks { btn: 96.0, bb: 104.0 }; // eff 96 within 7% of 100 -> in
        let entries = classify_hand(&h, &cfg());
        assert_eq!(gradeable(&entries)[0].depth_bb, 100);
    }

    #[test]
    fn structural_skips_are_hand_level() {
        let mut h = base_hand();
        h.table_size_dealt = 6;
        let entries = classify_hand(&h, &cfg());
        assert_eq!(entries.len(), 1);
        assert!(reasons(&entries).contains(&SkipReason::NotHeadsUp));

        let mut h = base_hand();
        h.hero_cards = None;
        assert!(reasons(&classify_hand(&h, &cfg())).contains(&SkipReason::HeroCardsUnknown));

        let mut h = base_hand();
        h.ante_bb = 0.1;
        assert!(reasons(&classify_hand(&h, &cfg())).contains(&SkipReason::AntePot));
    }

    #[test]
    fn turn_river_decisions_are_counted_for_the_roadmap() {
        let mut h = base_hand();
        h.turn = Some(LaterStreet {
            card: "Td".into(),
            actions: vec![
                ActionEvent { seat: Seat::Bb, kind: ActionKind::Check, amount_bb: 0.0, all_in: false },
                ActionEvent { seat: Seat::Btn, kind: ActionKind::Check, amount_bb: 0.0, all_in: false },
            ],
        });
        let entries = classify_hand(&h, &cfg());
        assert_eq!(gradeable(&entries).len(), 1); // flop check still gradeable
        assert!(reasons(&entries).contains(&SkipReason::TurnDecision));
    }

    #[test]
    fn hero_folds_preflop_is_its_own_reason() {
        let mut h = base_hand();
        h.preflop = vec![
            ActionEvent { seat: Seat::Btn, kind: ActionKind::Raise, amount_bb: 2.5, all_in: false },
            ActionEvent { seat: Seat::Bb, kind: ActionKind::Fold, amount_bb: 0.0, all_in: false },
        ];
        h.flop = None;
        let entries = classify_hand(&h, &cfg());
        assert!(gradeable(&entries).is_empty());
        assert!(reasons(&entries).contains(&SkipReason::HeroFoldedPreflop));
    }
}
