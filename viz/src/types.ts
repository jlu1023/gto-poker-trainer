// The read-only data contract emitted by the Rust `export_viz` exporter.

export interface HandEntry {
  hand: string;
  strategy: number[];
  ev: number;
  equity: number;
  /// Per-action EV (chips), aligned with `actions`. May be absent/empty on data exported
  /// before Phase 4.
  ev_actions?: number[];
}

export interface Seat {
  position: string;
  seat: string;
}

export interface HeroSeat extends Seat {
  player: number;
}

export interface SpotContract {
  key: string;
  board: string[];
  street: string;
  stack_depth_bb: number;
  pot_bb: number;
  eff_stack_bb: number;
  exploitability_pct_pot: number;
  iterations: number;
  hero: HeroSeat;
  villain: Seat;
  actions: string[];
  hands: HandEntry[];
}

export interface IndexEntry {
  key: string;
  board: string[];
  stack_depth_bb: number;
  exploitability_pct_pot: number;
}

export interface IndexFile {
  spots: IndexEntry[];
}

// ---- Phase 3 solve server (SSE) ----

/// `event: progress` payload: one convergence sample (every 10 solver iterations).
export interface SolveProgressMsg {
  iteration: number;
  exploitability_pct_pot: number;
}

/// `event: complete` payload. `cached` = served from the matrix with no solve;
/// `shrunk` = solved with a reduced flop tree after a RAM-ceiling breach.
export interface SolveComplete {
  cached: boolean;
  shrunk: boolean;
  spot: SpotContract;
}

// ---- Phase 4 trainer ----

/// GET /api/setup response: pre-commit data ONLY (labels and booleans, no strategy).
export interface SetupResponse {
  key: string;
  depth: number;
  board: string[];
  hand: string;
  hand_in_range: boolean;
  actions: string[];
  cached: boolean;
}

export type Verdict = "correct" | "fine_mix" | "minor_leak" | "clear_leak";

export interface GradeDistributionRow {
  action: string;
  frequency: number;
  ev: number;
}

/// POST /api/grade response: the verdict for the committed action.
export interface GradeResponse {
  key: string;
  depth: number;
  hand: string;
  chosen_action: string;
  mapped_from: string | null;
  frequency: number;
  ev_chosen: number;
  ev_best: number;
  ev_loss_chips: number;
  ev_loss_pct_pot: number;
  best_action: string;
  verdict: Verdict;
  pot_chips: number;
  distribution: GradeDistributionRow[];
}
