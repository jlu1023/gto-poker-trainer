//! PokerStars-text -> HandRecord adapter (D38). This module quarantines every site-format
//! landmine so the core (envelope/aggregation) never sees them: posts and dead blinds,
//! "and is all-in" markers, "Uncalled bet returned" lines, currency/locale headers,
//! sit-outs/timeouts, hero-only "Dealt to", rake/summary sections, and the dealt-count vs
//! table-max distinction (true HU = exactly 2 players DEALT).
//!
//! Scope: HU cash subset. Anything it cannot parse becomes an Err(detail) for that hand,
//! which the classifier records as `unparseable-hand` - never a fabricated record.

use crate::schema::{ActionEvent, ActionKind, FlopStreet, HandRecord, LaterStreet, Seat, Stacks};
use anyhow::{anyhow, bail, Result};

/// Split a file into hand blocks and parse each. Returns (parsed, errors-as-(id, detail)).
pub fn parse_file(text: &str, source: &str) -> (Vec<HandRecord>, Vec<(String, String)>) {
    let mut hands = Vec::new();
    let mut errors = Vec::new();
    for block in split_hands(text) {
        match parse_hand(&block, source) {
            Ok(h) => hands.push(h),
            Err(e) => {
                let id = block
                    .lines()
                    .next()
                    .and_then(|l| l.split('#').nth(1))
                    .and_then(|s| s.split(':').next())
                    .unwrap_or("unknown")
                    .to_string();
                errors.push((id, e.to_string()));
            }
        }
    }
    (hands, errors)
}

fn split_hands(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        if line.starts_with("PokerStars Hand #") || line.starts_with("PokerStars Game #") {
            if !current.trim().is_empty() {
                blocks.push(std::mem::take(&mut current));
            }
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        blocks.push(current);
    }
    blocks
}

struct PlayerInfo {
    name: String,
    stack: f64, // currency units
    seat_no: u32,
}

fn parse_hand(block: &str, source: &str) -> Result<HandRecord> {
    let mut lines = block.lines().peekable();

    // ---- header: id + stakes ----
    let header = lines.next().ok_or_else(|| anyhow!("empty hand"))?;
    let hand_id = header
        .split('#')
        .nth(1)
        .and_then(|s| s.split(':').next())
        .ok_or_else(|| anyhow!("no hand id in header"))?
        .trim()
        .to_string();
    // stakes like ($0.25/$0.50 USD) or (10/20) - take the two numbers in the parens
    let stakes = header
        .split('(')
        .nth(1)
        .and_then(|s| s.split(')').next())
        .ok_or_else(|| anyhow!("no stakes in header"))?;
    let nums: Vec<f64> = stakes
        .split('/')
        .filter_map(|part| parse_money(part))
        .collect();
    if nums.len() < 2 {
        bail!("could not parse blinds from '{stakes}'");
    }
    let (sb_cur, bb_cur) = (nums[0], nums[1]);
    if bb_cur <= 0.0 {
        bail!("non-positive big blind");
    }
    let to_bb = |x: f64| x / bb_cur;

    // ---- table line: button seat ----
    let table_line = lines
        .find(|l| l.starts_with("Table "))
        .ok_or_else(|| anyhow!("no Table line"))?;
    let button_seat: u32 = table_line
        .split("Seat #")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("no button seat in Table line"))?;

    // ---- seat lines: dealt players + stacks ----
    let mut players: Vec<PlayerInfo> = Vec::new();
    while let Some(line) = lines.peek() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Seat ") {
            let seat_no: u32 = rest
                .split(':')
                .next()
                .and_then(|s| s.trim().parse().ok())
                .ok_or_else(|| anyhow!("bad seat line '{line}'"))?;
            let name = rest
                .split(':')
                .nth(1)
                .and_then(|s| s.split('(').next())
                .map(|s| s.trim().to_string())
                .ok_or_else(|| anyhow!("no name in seat line"))?;
            let stack = rest
                .split('(')
                .nth(1)
                .and_then(parse_money_prefix)
                .ok_or_else(|| anyhow!("no stack in seat line '{line}'"))?;
            players.push(PlayerInfo { name, stack, seat_no });
            lines.next();
        } else {
            break;
        }
    }
    let table_size_dealt = players.len() as u32;

    // HU mapping: the button posts the SB and is BTN; the other player is BB. For non-HU
    // hands we still need a parseable record (the classifier skips it), so map hero/BTN
    // best-effort and mark size.
    let btn_player = players
        .iter()
        .find(|p| p.seat_no == button_seat)
        .ok_or_else(|| anyhow!("button seat {button_seat} not among dealt players"))?;
    let btn_name = btn_player.name.clone();
    let bb_player = players.iter().find(|p| p.seat_no != button_seat);
    let bb_name = bb_player.map(|p| p.name.clone()).unwrap_or_default();
    let stacks_bb = Stacks {
        btn: to_bb(btn_player.stack),
        bb: bb_player.map(|p| to_bb(p.stack)).unwrap_or(0.0),
    };
    let seat_of = |name: &str| -> Option<Seat> {
        if name == btn_name {
            Some(Seat::Btn)
        } else if name == bb_name {
            Some(Seat::Bb)
        } else {
            None
        }
    };

    // ---- streets ----
    let mut blinds_seen = (false, false);
    let mut extra_posts = 0u32;
    let mut ante_bb = 0.0f64;
    let mut hero_name: Option<String> = None;
    let mut hero_cards: Option<String> = None;
    let mut preflop: Vec<ActionEvent> = Vec::new();
    let mut flop: Option<FlopStreet> = None;
    let mut turn: Option<LaterStreet> = None;
    let mut river: Option<LaterStreet> = None;

    #[derive(PartialEq)]
    enum Street {
        Preflop,
        Flop,
        Turn,
        River,
        Done,
    }
    let mut street = Street::Preflop;

    for raw in lines {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("*** SUMMARY ***") || line.starts_with("*** SHOW DOWN ***") {
            street = Street::Done;
            continue;
        }
        if street == Street::Done {
            continue;
        }
        if line.starts_with("*** HOLE CARDS ***") {
            continue;
        }
        if let Some(rest) = line.strip_prefix("*** FLOP *** [") {
            let cards = parse_card_list(rest.split(']').next().unwrap_or(""))?;
            if cards.len() != 3 {
                bail!("flop with {} cards", cards.len());
            }
            flop = Some(FlopStreet {
                cards: [cards[0].clone(), cards[1].clone(), cards[2].clone()],
                actions: Vec::new(),
            });
            street = Street::Flop;
            continue;
        }
        if line.starts_with("*** TURN ***") {
            let card = last_bracket_card(line)?;
            turn = Some(LaterStreet { card, actions: Vec::new() });
            street = Street::Turn;
            continue;
        }
        if line.starts_with("*** RIVER ***") {
            let card = last_bracket_card(line)?;
            river = Some(LaterStreet { card, actions: Vec::new() });
            street = Street::River;
            continue;
        }
        if let Some(rest) = line.strip_prefix("Dealt to ") {
            let name = rest.split('[').next().map(|s| s.trim().to_string()).unwrap_or_default();
            if let Some(cards_part) = rest.split('[').nth(1) {
                let cards = parse_card_list(cards_part.split(']').next().unwrap_or(""))?;
                if cards.len() == 2 {
                    hero_name = Some(name);
                    hero_cards = Some(format!("{}{}", cards[0], cards[1]));
                }
            }
            continue;
        }
        // non-action lines to ignore
        if line.starts_with("Uncalled bet")
            || line.contains("collected")
            || line.contains("doesn't show")
            || line.contains("shows [")
            || line.contains("mucks")
            || line.contains("leaves the table")
            || line.contains("joins the table")
            || line.contains("said,")
            || line.contains("is disconnected")
            || line.contains("has timed out")
        {
            continue;
        }
        if line.contains("sits out") || line.contains("is sitting out") {
            continue;
        }

        // ---- player action lines: "Name: action ..." ----
        let Some((name, action_part)) = line.split_once(": ") else {
            continue;
        };
        let Some(seat) = seat_of(name) else {
            continue; // observer/other-table noise
        };
        let all_in = action_part.contains("and is all-in");

        let ev = if action_part.starts_with("posts small blind") {
            blinds_seen.0 = true;
            continue;
        } else if action_part.starts_with("posts big blind") {
            blinds_seen.1 = true;
            continue;
        } else if action_part.starts_with("posts the ante") {
            ante_bb = ante_bb.max(
                parse_money_suffix(action_part).map(to_bb).unwrap_or(0.0),
            );
            continue;
        } else if action_part.starts_with("posts") {
            // dead/missed post beyond the structural blinds -> the classifier's
            // non-standard-blinds path
            extra_posts += 1;
            ActionEvent {
                seat,
                kind: ActionKind::Post,
                amount_bb: parse_money_suffix(action_part).map(to_bb).unwrap_or(0.0),
                all_in,
            }
        } else if action_part.starts_with("folds") {
            ActionEvent { seat, kind: ActionKind::Fold, amount_bb: 0.0, all_in }
        } else if action_part.starts_with("checks") {
            ActionEvent { seat, kind: ActionKind::Check, amount_bb: 0.0, all_in }
        } else if action_part.starts_with("calls") {
            ActionEvent {
                seat,
                kind: ActionKind::Call,
                amount_bb: parse_money_suffix(action_part).map(to_bb).unwrap_or(0.0),
                all_in,
            }
        } else if action_part.starts_with("bets") {
            ActionEvent {
                seat,
                kind: ActionKind::Bet,
                amount_bb: parse_money_suffix(action_part).map(to_bb).unwrap_or(0.0),
                all_in,
            }
        } else if action_part.starts_with("raises") {
            // "raises $1 to $1.50" - the raise-TO total is the second amount
            let to_amount = action_part
                .split(" to ")
                .nth(1)
                .and_then(parse_money_prefix)
                .ok_or_else(|| anyhow!("unparseable raise '{action_part}'"))?;
            ActionEvent { seat, kind: ActionKind::Raise, amount_bb: to_bb(to_amount), all_in }
        } else {
            continue; // unknown verb: ignore rather than fail the hand
        };

        match street {
            Street::Preflop => preflop.push(ev),
            Street::Flop => {
                if let Some(f) = flop.as_mut() {
                    f.actions.push(ev);
                }
            }
            Street::Turn => {
                if let Some(t) = turn.as_mut() {
                    t.actions.push(ev);
                }
            }
            Street::River => {
                if let Some(r) = river.as_mut() {
                    r.actions.push(ev);
                }
            }
            Street::Done => {}
        }
    }

    // hero must be one of the two mapped seats for the record to be meaningful
    let hero_seat = match hero_name.as_deref().and_then(seat_of) {
        Some(s) => s,
        None => {
            if hero_cards.is_some() {
                bail!("hero '{}' not among dealt players", hero_name.unwrap_or_default());
            }
            Seat::Bb // no hole cards: classifier skips the hand anyway; seat is moot
        }
    };

    // a missing structural blind reads as non-standard (sit-out hands etc.)
    let blinds_bb = if blinds_seen.0 && blinds_seen.1 && extra_posts == 0 {
        (to_bb(sb_cur), 1.0)
    } else {
        (0.0, 0.0) // forces the classifier's non-standard-blinds skip
    };

    Ok(HandRecord {
        hand_id,
        source: source.to_string(),
        table_size_dealt,
        hero_seat,
        stacks_bb,
        blinds_bb,
        ante_bb,
        hero_cards,
        preflop,
        flop,
        turn,
        river,
    })
}

/// "$0.25", "€1.50", "0.50 USD", "1.50 in chips)" -> 0.25 / 1.5 / 0.5 / 1.5
fn parse_money(s: &str) -> Option<f64> {
    let cleaned: String = s
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    cleaned.parse().ok()
}

fn parse_money_prefix(s: &str) -> Option<f64> {
    parse_money(s.split_whitespace().next()?)
}

fn parse_money_suffix(s: &str) -> Option<f64> {
    // the last money-looking token, skipping the "and is all-in" tail
    s.replace("and is all-in", " ")
        .split_whitespace()
        .rev()
        .find_map(|tok| {
            let m = parse_money(tok)?;
            if tok.chars().any(|c| c.is_ascii_digit()) {
                Some(m)
            } else {
                None
            }
        })
}

fn parse_card_list(s: &str) -> Result<Vec<String>> {
    let cards: Vec<String> = s
        .split_whitespace()
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect();
    for c in &cards {
        if c.len() != 2 {
            bail!("bad card '{c}'");
        }
    }
    Ok(cards)
}

fn last_bracket_card(line: &str) -> Result<String> {
    let card = line
        .rsplit('[')
        .next()
        .and_then(|s| s.split(']').next())
        .map(|s| s.trim().to_string())
        .ok_or_else(|| anyhow!("no card in '{line}'"))?;
    if card.len() != 2 {
        bail!("bad street card '{card}'");
    }
    Ok(card)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HU_SRP_HAND: &str = r#"PokerStars Hand #240000000001:  Hold'em No Limit ($0.25/$0.50 USD) - 2026/06/01 20:00:00 ET
Table 'Halia II' 2-max Seat #1 is the button
Seat 1: Villain ($50 in chips)
Seat 2: Hero ($50 in chips)
Villain: posts small blind $0.25
Hero: posts big blind $0.50
*** HOLE CARDS ***
Dealt to Hero [Ah Kh]
Villain: raises $0.75 to $1.25
Hero: calls $0.75
*** FLOP *** [Ks 7h 2c]
Hero: checks
Villain: checks
*** TURN *** [Ks 7h 2c] [Td]
Hero: bets $1.25
Villain: folds
Uncalled bet ($1.25) returned to Hero
Hero collected $2.44 from pot
*** SUMMARY ***
Total pot $2.50 | Rake $0.06
"#;

    #[test]
    fn parses_the_hu_srp_hand() {
        let (hands, errors) = parse_file(HU_SRP_HAND, "test");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(hands.len(), 1);
        let h = &hands[0];
        assert_eq!(h.hand_id, "240000000001");
        assert_eq!(h.table_size_dealt, 2);
        assert_eq!(h.hero_seat, Seat::Bb);
        assert_eq!(h.hero_cards.as_deref(), Some("AhKh"));
        assert!((h.stacks_bb.btn - 100.0).abs() < 1e-9);
        assert_eq!(h.blinds_bb, (0.5, 1.0));
        // open: raises to $1.25 = 2.5bb
        assert_eq!(h.preflop.len(), 2);
        assert!(matches!(h.preflop[0].kind, ActionKind::Raise));
        assert!((h.preflop[0].amount_bb - 2.5).abs() < 1e-9);
        let flop = h.flop.as_ref().unwrap();
        assert_eq!(flop.cards, ["Ks", "7h", "2c"]);
        assert_eq!(flop.actions.len(), 2);
        // turn parsed for classification (bet + fold); the uncalled-bet line was ignored
        assert_eq!(h.turn.as_ref().unwrap().actions.len(), 2);
    }

    #[test]
    fn all_in_marker_and_uncalled_bet_are_handled() {
        let text = HU_SRP_HAND.replace(
            "Villain: raises $0.75 to $1.25",
            "Villain: raises $49.50 to $50 and is all-in",
        );
        let (hands, errors) = parse_file(&text, "test");
        assert!(errors.is_empty(), "{errors:?}");
        assert!(hands[0].preflop[0].all_in);
        assert!((hands[0].preflop[0].amount_bb - 100.0).abs() < 1e-9);
    }

    #[test]
    fn dead_post_marks_non_standard_blinds() {
        let text = HU_SRP_HAND.replace(
            "*** HOLE CARDS ***",
            "Hero: posts $0.50\n*** HOLE CARDS ***",
        );
        let (hands, errors) = parse_file(&text, "test");
        assert!(errors.is_empty(), "{errors:?}");
        // extra post forces the unparseable-blinds sentinel for the classifier
        assert_eq!(hands[0].blinds_bb, (0.0, 0.0));
    }

    #[test]
    fn multiple_hands_split_correctly() {
        let two = format!("{HU_SRP_HAND}\n\n{}", HU_SRP_HAND.replace("240000000001", "240000000002"));
        let (hands, errors) = parse_file(&two, "test");
        assert!(errors.is_empty());
        assert_eq!(hands.len(), 2);
        assert_eq!(hands[1].hand_id, "240000000002");
    }

    #[test]
    fn garbage_block_is_an_error_not_a_panic() {
        let (hands, errors) = parse_file("PokerStars Hand #99: mangled\nnot a hand at all", "test");
        assert!(hands.is_empty());
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, "99");
    }
}
