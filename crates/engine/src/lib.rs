//! `gto-engine`: the core HUNL solve + node-query layer.
//!
//! Responsibilities:
//!   - `Spot`: the data model for a live heads-up no-limit hold'em situation.
//!   - `solve_spot`: map a `Spot` onto a `postflop-solver` query, build it, solve to target,
//!     and read back the root strategy / EV / equity.
//!   - `query_node`: walk a solved tree to an arbitrary node and read its strategy / EV / equity.
//!
//! Design note (highest-risk piece): postflop-solver solves a *subgame* starting at a chosen
//! street. The preflop (and any earlier-street) action history is NOT passed to the solver. It is
//! fully encoded in three inputs: the two ranges entering the spot, the starting pot, and the
//! effective stack remaining. So "represent a live spot" means pinning down street, board, pot,
//! effective stack, both ranges, and rake. Money is carried in big blinds and converted to integer
//! chips via `chip_per_bb` at mapping time.

use anyhow::{anyhow, bail, Result};
use gto_config::SolveConfig;
use postflop_solver::{
    card_from_str, compute_exploitability, finalize, flop_from_str, holes_to_strings, solve_step,
    Action, ActionTree, BetSizeOptions, BoardState, CardConfig, DonkSizeOptions, PostFlopGame,
    Range, TreeConfig, NOT_DEALT,
};
use serde::{Deserialize, Serialize};

// re-exported so callers can build `Step`s and inspect cards without depending on the solver crate
pub use postflop_solver::{Action as SolverAction, Card};

/// Which street solving begins on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Street {
    Flop,
    Turn,
    River,
}

/// Board cards as human-readable strings (e.g. "Td"), parsed to solver cards at mapping time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Board {
    pub flop: [String; 3],
    pub turn: Option<String>,
    pub river: Option<String>,
}

/// Per-spot rake. Default 0/0 for pure GTO solving.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct RakeSpec {
    pub rate: f32,
    pub cap: f32,
}

/// A live HUNL spot. The unit a curated matrix entry is built from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Spot {
    /// Nominal starting depth in bb (label / spot key), e.g. 100.
    pub stack_depth_bb: u32,
    pub initial_street: Street,
    pub board: Board,
    /// Dead money in the middle at the start of `initial_street`, in bb.
    pub pot_bb: f64,
    /// Remaining effective stack fed to the solver, in bb (min of the two stacks).
    pub effective_stack_bb: f64,
    /// Out-of-position player's range (BB in a single-raised pot), solver string format.
    pub oop_range: String,
    /// In-position player's range (BTN in a single-raised pot).
    pub ip_range: String,
    #[serde(default)]
    pub rake: RakeSpec,
}

/// One step on a path from the root to a target node.
/// Semantic (not raw indices) so a query survives action-tree edits.
#[derive(Debug, Clone)]
pub enum Step {
    /// A player action, matched against `available_actions()` at query time.
    Action(Action),
    /// A chance card (turn / river runout).
    Chance(Card),
}

/// A path of `Step`s identifying a node to read.
#[derive(Debug, Clone, Default)]
pub struct NodeQuery {
    pub path: Vec<Step>,
}

/// The strategy / EV / equity at one node, for the player to act.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeStrategy {
    /// 0 = OOP, 1 = IP.
    pub player: usize,
    /// Available actions, rendered (Check / Bet(n) / Call / Fold / Raise(n) / AllIn(n)).
    pub actions: Vec<String>,
    /// Hand labels for `player` (e.g. "KsJs"), aligned with the strategy/ev/equity vectors.
    pub hands: Vec<String>,
    /// Action probabilities, indexed `strategy[hand + action * num_hands]`.
    pub strategy: Vec<f32>,
    /// Expected value per hand (chips).
    pub ev: Vec<f32>,
    /// Equity per hand (0..1).
    pub equity: Vec<f32>,
}

/// A solved spot. Owns the solved game so it can be persisted natively and queried after reload
/// with zero solving.
pub struct SpotSolution {
    pub spot: Spot,
    pub bet_tree_id: String,
    pub exploitability_chips: f32,
    pub exploitability_pct_pot: f32,
    pub iterations_run: u32,
    pub root: NodeStrategy,
    /// The solved game tree (serialized via postflop-solver's native format by the store crate).
    pub game: PostFlopGame,
}

/// Serde-friendly view of a `SpotSolution` (everything except the heavy game), written to
/// `out/smoke.json` and stored as the matrix sidecar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpotSolutionSummary {
    pub spot: Spot,
    pub bet_tree_id: String,
    pub exploitability_chips: f32,
    pub exploitability_pct_pot: f32,
    pub iterations_run: u32,
    pub root: NodeStrategy,
}

impl SpotSolution {
    pub fn summary(&self) -> SpotSolutionSummary {
        SpotSolutionSummary {
            spot: self.spot.clone(),
            bet_tree_id: self.bet_tree_id.clone(),
            exploitability_chips: self.exploitability_chips,
            exploitability_pct_pot: self.exploitability_pct_pot,
            iterations_run: self.iterations_run,
            root: self.root.clone(),
        }
    }
}

/// A convergence sample emitted during a streaming solve (every 10 iterations).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct SolveProgress {
    pub iteration: u32,
    pub exploitability_pct_pot: f32,
}

/// Build, solve, and read the root of a spot.
pub fn solve_spot(spot: &Spot, cfg: &SolveConfig) -> Result<SpotSolution> {
    solve_spot_streaming(spot, cfg, |_| {})
}

/// Like `solve_spot`, but invokes `on_progress` after each exploitability check (every 10
/// iterations) so a caller can stream convergence (Phase 3 solve server).
pub fn solve_spot_streaming<F>(spot: &Spot, cfg: &SolveConfig, mut on_progress: F) -> Result<SpotSolution>
where
    F: FnMut(SolveProgress),
{
    let card_config = build_card_config(spot)?;
    let tree_config = build_tree_config(spot, cfg)?;

    let action_tree = ActionTree::new(tree_config).map_err(|e| anyhow!("action tree: {e}"))?;
    let mut game =
        PostFlopGame::with_config(card_config, action_tree).map_err(|e| anyhow!("game build: {e}"))?;

    // memory guard: refuse to allocate beyond the configured ceiling
    let (mem, mem_compressed) = game.memory_usage();
    let ceiling = (cfg.abstraction.ram_ceiling_gb * (1u64 << 30) as f64) as u64;
    let use_compression = cfg.abstraction.use_compression || mem > ceiling;
    let effective_mem = if use_compression { mem_compressed } else { mem };
    if effective_mem > ceiling {
        bail!(
            "solve would need {:.2} GiB (> {:.2} GiB ceiling); shrink the bet tree or stack depth",
            effective_mem as f64 / (1u64 << 30) as f64,
            cfg.abstraction.ram_ceiling_gb
        );
    }
    game.allocate_memory(use_compression);

    let starting_pot = (spot.pot_bb * cfg.chip_per_bb as f64).round() as f32;
    let target = starting_pot * cfg.target_exploit_pct_pot as f32;

    // manual solve loop so we can count iterations, stop early on convergence, and stream progress
    let mut iterations = 0u32;
    let mut exploitability = f32::INFINITY;
    for i in 0..cfg.max_iterations {
        solve_step(&game, i);
        iterations = i + 1;
        if (i + 1) % 10 == 0 {
            exploitability = compute_exploitability(&game);
            on_progress(SolveProgress {
                iteration: iterations,
                exploitability_pct_pot: if starting_pot > 0.0 {
                    exploitability / starting_pot
                } else {
                    f32::INFINITY
                },
            });
            if exploitability <= target {
                break;
            }
        }
    }
    if !exploitability.is_finite() {
        exploitability = compute_exploitability(&game);
    }
    finalize(&mut game);

    let root = read_root(&mut game)?;

    Ok(SpotSolution {
        spot: spot.clone(),
        bet_tree_id: cfg.bet_tree.bet_tree_id.clone(),
        exploitability_chips: exploitability,
        exploitability_pct_pot: if starting_pot > 0.0 {
            exploitability / starting_pot
        } else {
            f32::INFINITY
        },
        iterations_run: iterations,
        root,
        game,
    })
}

/// Read the root node of an already-solved game.
pub fn read_root(game: &mut PostFlopGame) -> Result<NodeStrategy> {
    game.back_to_root();
    read_current_node(game)
}

/// Walk a solved game to the node identified by `query` and read it.
pub fn query_node(solution: &mut SpotSolution, query: &NodeQuery) -> Result<NodeStrategy> {
    let game = &mut solution.game;
    game.back_to_root();
    for step in &query.path {
        match step {
            Step::Chance(card) => {
                if !game.is_chance_node() {
                    bail!("query expected a chance node but the current node is a player node");
                }
                game.play(*card as usize);
            }
            Step::Action(action) => {
                if game.is_chance_node() {
                    bail!("query expected a player node but the current node is a chance node");
                }
                let actions = game.available_actions();
                // match semantically via Debug rendering (no PartialEq dependency on Action)
                let want = format!("{action:?}");
                let idx = actions
                    .iter()
                    .position(|a| format!("{a:?}") == want)
                    .ok_or_else(|| {
                        anyhow!("action {want} not available; options are {actions:?}")
                    })?;
                game.play(idx);
            }
        }
    }
    read_current_node(game)
}

/// Read the strategy / EV / equity at whatever node the game is currently positioned on.
fn read_current_node(game: &mut PostFlopGame) -> Result<NodeStrategy> {
    if game.is_chance_node() {
        bail!("cannot read a strategy at a chance node");
    }
    if game.is_terminal_node() {
        bail!("cannot read a strategy at a terminal node");
    }
    game.cache_normalized_weights();
    let player = game.current_player();
    let actions = game
        .available_actions()
        .iter()
        .map(|a| render_action(a))
        .collect::<Vec<_>>();
    let hands = holes_to_strings(game.private_cards(player))
        .map_err(|e| anyhow!("holes_to_strings: {e}"))?;
    let strategy = game.strategy();
    let ev = game.expected_values(player);
    let equity = game.equity(player);
    Ok(NodeStrategy {
        player,
        actions,
        hands,
        strategy,
        ev,
        equity,
    })
}

fn render_action(a: &Action) -> String {
    match a {
        Action::Fold => "Fold".to_string(),
        Action::Check => "Check".to_string(),
        Action::Call => "Call".to_string(),
        Action::Bet(n) => format!("Bet({n})"),
        Action::Raise(n) => format!("Raise({n})"),
        Action::AllIn(n) => format!("AllIn({n})"),
        Action::Chance(c) => format!("Chance({c})"),
        Action::None => "None".to_string(),
    }
}

fn build_card_config(spot: &Spot) -> Result<CardConfig> {
    let oop: Range = spot
        .oop_range
        .parse()
        .map_err(|e| anyhow!("OOP range parse: {e}"))?;
    let ip: Range = spot
        .ip_range
        .parse()
        .map_err(|e| anyhow!("IP range parse: {e}"))?;

    let flop_str = spot.board.flop.concat();
    let flop = flop_from_str(&flop_str).map_err(|e| anyhow!("flop '{flop_str}': {e}"))?;
    let turn = match &spot.board.turn {
        Some(s) => card_from_str(s).map_err(|e| anyhow!("turn '{s}': {e}"))?,
        None => NOT_DEALT,
    };
    let river = match &spot.board.river {
        Some(s) => card_from_str(s).map_err(|e| anyhow!("river '{s}': {e}"))?,
        None => NOT_DEALT,
    };

    Ok(CardConfig {
        range: [oop, ip],
        flop,
        turn,
        river,
    })
}

fn build_tree_config(spot: &Spot, cfg: &SolveConfig) -> Result<TreeConfig> {
    let scale = cfg.chip_per_bb as f64;
    let starting_pot = (spot.pot_bb * scale).round() as i32;
    let effective_stack = (spot.effective_stack_bb * scale).round() as i32;

    let bt = &cfg.bet_tree;
    let flop_bet_sizes = [
        bet_sizes(&bt.flop_bet_sizes[0], &bt.flop_raise_sizes[0])?,
        bet_sizes(&bt.flop_bet_sizes[1], &bt.flop_raise_sizes[1])?,
    ];
    let turn_bet_sizes = [
        bet_sizes(&bt.turn_bet_sizes[0], &bt.turn_raise_sizes[0])?,
        bet_sizes(&bt.turn_bet_sizes[1], &bt.turn_raise_sizes[1])?,
    ];
    let river_bet_sizes = [
        bet_sizes(&bt.river_bet_sizes[0], &bt.river_raise_sizes[0])?,
        bet_sizes(&bt.river_bet_sizes[1], &bt.river_raise_sizes[1])?,
    ];

    Ok(TreeConfig {
        initial_state: board_state(spot.initial_street),
        starting_pot,
        effective_stack,
        rake_rate: spot.rake.rate as f64,
        rake_cap: spot.rake.cap as f64,
        flop_bet_sizes,
        turn_bet_sizes,
        river_bet_sizes,
        turn_donk_sizes: donk_sizes(&bt.turn_donk_sizes)?,
        river_donk_sizes: donk_sizes(&bt.river_donk_sizes)?,
        add_allin_threshold: cfg.abstraction.add_allin_threshold as f64,
        force_allin_threshold: cfg.abstraction.force_allin_threshold as f64,
        merging_threshold: cfg.abstraction.merging_threshold as f64,
    })
}

fn bet_sizes(bet: &str, raise: &str) -> Result<BetSizeOptions> {
    BetSizeOptions::try_from((bet, raise))
        .map_err(|e| anyhow!("bet size spec ('{bet}' / '{raise}'): {e}"))
}

fn donk_sizes(spec: &Option<String>) -> Result<Option<DonkSizeOptions>> {
    match spec {
        Some(s) => Ok(Some(
            DonkSizeOptions::try_from(s.as_str()).map_err(|e| anyhow!("donk size '{s}': {e}"))?,
        )),
        None => Ok(None),
    }
}

fn board_state(s: Street) -> BoardState {
    match s {
        Street::Flop => BoardState::Flop,
        Street::Turn => BoardState::Turn,
        Street::River => BoardState::River,
    }
}
