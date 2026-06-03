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
