//! The v1 hand-history input contract (D38). All money in BIG BLINDS. This JSON shape is the
//! engine-facing format; site exports (PokerStars text) are converted by adapters. Hero hole
//! cards are REQUIRED to grade (the grader needs the exact combo); hands without them are
//! classified, never graded.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Seat {
    Btn,
    Bb,
}

impl Seat {
    pub fn label(self) -> &'static str {
        match self {
            Seat::Btn => "BTN",
            Seat::Bb => "BB",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionKind {
    Fold,
    Check,
    Call,
    Bet,
    Raise,
    /// A blind/dead post beyond the structural blinds (sit-out returns etc.).
    Post,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionEvent {
    pub seat: Seat,
    pub kind: ActionKind,
    /// For Raise this is the raise-TO total; for Bet/Call/Post the amount. In bb.
    #[serde(default)]
    pub amount_bb: f64,
    /// The site marked this action all-in.
    #[serde(default)]
    pub all_in: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlopStreet {
    pub cards: [String; 3],
    #[serde(default)]
    pub actions: Vec<ActionEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaterStreet {
    pub card: String,
    #[serde(default)]
    pub actions: Vec<ActionEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stacks {
    pub btn: f64,
    pub bb: f64,
}

/// One completed hand, normalized. Parsed for classification through all streets; only flop
/// decisions inside the envelope are graded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandRecord {
    pub hand_id: String,
    #[serde(default)]
    pub source: String,
    /// Players DEALT cards (not the table max). True heads-up = exactly 2.
    pub table_size_dealt: u32,
    pub hero_seat: Seat,
    /// Starting stacks in bb.
    pub stacks_bb: Stacks,
    /// (small blind, big blind) normalized to bb units; standard HU = (0.5, 1.0).
    pub blinds_bb: (f64, f64),
    #[serde(default)]
    pub ante_bb: f64,
    /// Hero's exact two cards, e.g. "AhKh". None -> hand cannot be graded.
    #[serde(default)]
    pub hero_cards: Option<String>,
    #[serde(default)]
    pub preflop: Vec<ActionEvent>,
    #[serde(default)]
    pub flop: Option<FlopStreet>,
    #[serde(default)]
    pub turn: Option<LaterStreet>,
    #[serde(default)]
    pub river: Option<LaterStreet>,
}

/// A batch of hands (the JSON file shape).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryFile {
    pub hands: Vec<HandRecord>,
}
