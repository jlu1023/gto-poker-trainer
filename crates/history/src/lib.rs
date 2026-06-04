//! `gto-history`: Phase 5 hand-history leak analysis.
//!
//! Pipeline: input (JSON contract or the PokerStars-text adapter) -> envelope classifier
//! (gradeable vs skipped-with-reason; the coverage breakdown doubles as the roadmap) ->
//! grading via the existing solve/cache path (server-side) -> node aggregation (operator
//! rates vs range-conditioned solver mixes, Poisson-binomial significance, mix-EV cost) ->
//! accumulating JSONL store.
//!
//! Frequency, not sequence: nothing in this crate produces or implies a next-move
//! recommendation; outputs are rate gaps with EV cost over many instances.

pub mod aggregate;
pub mod envelope;
pub mod pstext;
pub mod schema;
pub mod store;

use gto_config::ReviewConfig;

/// Classify a batch of hands; every hand contributes at least one entry.
pub fn classify_history(
    hands: &[schema::HandRecord],
    cfg: &ReviewConfig,
) -> Vec<envelope::Classified> {
    hands
        .iter()
        .flat_map(|h| envelope::classify_hand(h, cfg))
        .collect()
}
