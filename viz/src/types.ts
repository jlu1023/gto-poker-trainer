// The read-only data contract emitted by the Rust `export_viz` exporter.

export interface HandEntry {
  hand: string;
  strategy: number[];
  ev: number;
  equity: number;
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
