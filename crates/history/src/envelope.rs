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
use gto_config::ReviewConfig;
use serde::{Deserialize, Serialize};

/// SRP geometry guard (template open is 2.5bb; matches build_matrix / the solve server).
const OPEN_BAND_BB: (f64, f64) = (2.3, 2.7);
const DEPTH_BUCKETS: [u32; 2] = [50, 100];
const DEPTH_TOLERANCE: f64 = 0.07;
/// SRP-tree flop OPENING sizes as fractions of pot (the graded root nodes; raise lines,
/// in-tree since srp-v2, are not graded shapes and never reach this mapping).
const TREE_SIZES: [(f64, &str); 2] = [(0.33, "Bet(33)"), (0.75, "Bet(75)")];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkipReason {
    NotHeadsUp,
    UnparseableHand,
    HeroCardsUnknown,
    AntePot,
    NonStandardBlinds,
    LimpedPot,
    ThreeBetPot,
    OpenSizeOutOfBand,
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
            SkipReason::ThreeBetPot => "3-bet-pot",
            SkipReason::OpenSizeOutOfBand => "open-size-out-of-band",
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeShape {
    BbRoot,
    IpAfterCheck,
}

impl NodeShape {
    pub fn key(self) -> &'static str {
        match self {
            NodeShape::BbRoot => "bb-root",
            NodeShape::IpAfterCheck => "ip-after-check",
        }
    }
}

/// A decision the engine can grade now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradeableDecision {
    pub hand_id: String,
    pub street: String, // "flop"
    pub decision_index: u32,
    pub depth_bb: u32,
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

    // pot-type classification over the whole preflop line
    let pot_type = classify_preflop(&h.preflop);
    let (open_size_bb, srp) = match pot_type {
        PotType::Srp { open_to_bb } => (open_to_bb, true),
        _ => (0.0, false),
    };

    // per-decision entries for hero's preflop actions
    for ev in &h.preflop {
        if ev.seat != hero {
            continue;
        }
        let reason = if ev.kind == ActionKind::Fold {
            SkipReason::HeroFoldedPreflop
        } else if !srp {
            match pot_type {
                PotType::Limped => SkipReason::LimpedPot,
                PotType::ThreeBet => SkipReason::ThreeBetPot,
                _ => SkipReason::PreflopDecision,
            }
        } else {
            SkipReason::PreflopDecision
        };
        push_skip(&mut entries, &mut decision_index, reason, format!("{:?}", ev.kind));
    }

    if !srp {
        // pot type out of envelope: tag any postflop hero decisions with the pot-type reason
        let reason = match pot_type {
            PotType::Limped => SkipReason::LimpedPot,
            PotType::ThreeBet => SkipReason::ThreeBetPot,
            PotType::NoFlop => {
                if entries.is_empty() {
                    push_skip(&mut entries, &mut decision_index, SkipReason::NoPostflopDecision,
                        "hand ended preflop with no hero decision".to_string());
                }
                return entries;
            }
            PotType::Srp { .. } => unreachable!(),
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

    // ---- geometry guards (tight; D39) ----
    let eff = h.stacks_bb.btn.min(h.stacks_bb.bb);
    let depth_bucket = DEPTH_BUCKETS
        .iter()
        .copied()
        .find(|&d| ((eff - d as f64) / d as f64).abs() <= DEPTH_TOLERANCE);
    let geometry_skip = if !(OPEN_BAND_BB.0..=OPEN_BAND_BB.1).contains(&open_size_bb) {
        Some((SkipReason::OpenSizeOutOfBand, format!("open {open_size_bb:.2} bb (band {OPEN_BAND_BB:?})")))
    } else if depth_bucket.is_none() {
        Some((SkipReason::DepthOutOfBand, format!("eff {eff:.1} bb (buckets {DEPTH_BUCKETS:?} +-{:.0}%)", DEPTH_TOLERANCE * 100.0)))
    } else {
        None
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
    let pot_bb = 2.0 * open_size_bb;

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
        // check-only prefix: the node shape is fully determined
        let shape = match (hero, k) {
            (Seat::Bb, 0) => NodeShape::BbRoot,
            (Seat::Btn, 1) => NodeShape::IpAfterCheck,
            _ => {
                // e.g. parser glitch (hero BTN acting first HU postflop is impossible)
                push_skip(&mut entries, &mut decision_index, SkipReason::UnparseableHand,
                    format!("impossible flop action order (hero {} at index {k})", hero.label()));
                continue;
            }
        };
        // hero's own action must land on the tree
        let action_taken = match map_hero_action(ev, pot_bb, cfg) {
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

enum PotType {
    Srp { open_to_bb: f64 },
    Limped,
    ThreeBet,
    NoFlop, // hand ended preflop without forming a pot type (e.g. BTN open-folds? walk)
}

/// HU preflop structure: BTN acts first. SRP = exactly one raise (BTN open) then BB call.
fn classify_preflop(actions: &[ActionEvent]) -> PotType {
    let live: Vec<&ActionEvent> = actions.iter().filter(|a| a.kind != ActionKind::Post).collect();
    let raises = live.iter().filter(|a| a.kind == ActionKind::Raise).count();
    if raises >= 2 {
        return PotType::ThreeBet;
    }
    // a BTN limp (call of the bb) before any raise = limped pot
    if let Some(first) = live.first() {
        if first.kind == ActionKind::Call {
            return PotType::Limped;
        }
        if first.kind == ActionKind::Fold {
            return PotType::NoFlop; // walk
        }
    } else {
        return PotType::NoFlop;
    }
    if raises == 1 {
        let open = live.iter().find(|a| a.kind == ActionKind::Raise).unwrap();
        // SRP requires the open to be CALLED (a fold ends the hand preflop: NoFlop covers
        // that via the flop-presence check; the pot type is still "single-raised")
        return PotType::Srp { open_to_bb: open.amount_bb };
    }
    PotType::Limped // checks only (BB checks option after limp) - limped family
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

/// Map hero's real flop action onto the tree: Check exact; a bet only within the configured
/// tolerance of a tree size (else Err -> off-tree-hero-size). Never nearest-map.
fn map_hero_action(ev: &ActionEvent, pot_bb: f64, cfg: &ReviewConfig) -> Result<String, String> {
    match ev.kind {
        ActionKind::Check => Ok("Check".to_string()),
        ActionKind::Bet => {
            if ev.all_in {
                return Err(format!("all-in bet {:.2} bb (no all-in at the graded root nodes)", ev.amount_bb));
            }
            let pct = ev.amount_bb / pot_bb;
            for (size, label) in TREE_SIZES {
                if (pct - size).abs() <= cfg.hero_size_tolerance_pct_pot {
                    return Ok(label.to_string());
                }
            }
            Err(format!(
                "bet {:.2} bb = {:.0}% pot; tree sizes 33%/75% (+-{:.0}% tolerance)",
                ev.amount_bb,
                pct * 100.0,
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

    #[test]
    fn three_bet_and_limped_pots_are_skipped() {
        let mut h = base_hand();
        h.preflop = vec![
            ActionEvent { seat: Seat::Btn, kind: ActionKind::Raise, amount_bb: 2.5, all_in: false },
            ActionEvent { seat: Seat::Bb, kind: ActionKind::Raise, amount_bb: 9.0, all_in: false },
            ActionEvent { seat: Seat::Btn, kind: ActionKind::Call, amount_bb: 6.5, all_in: false },
        ];
        let entries = classify_hand(&h, &cfg());
        assert!(gradeable(&entries).is_empty());
        assert!(reasons(&entries).contains(&SkipReason::ThreeBetPot));

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
