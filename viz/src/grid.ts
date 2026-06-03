// The 13x13 starting-hand matrix. Aggregates the solver's per-combo root strategy to the 169 hand
// classes (e.g. "AsKs" -> "AKs"), renders each cell as a stacked action-frequency bar, and reports a
// representative combo on selection so the table can show concrete hero cards.

import type { SpotContract } from "./types";

const RANKS = ["A", "K", "Q", "J", "T", "9", "8", "7", "6", "5", "4", "3", "2"];
const RANK_INDEX: Record<string, number> = Object.fromEntries(RANKS.map((r, i) => [r, i]));

// action hue palette, applied by action index (Check, Bet33, Bet75, ...)
const ACTION_PALETTE = ["#5b8def", "#46b27a", "#e0a33e", "#d2553f", "#9b5de5", "#3fb0b0"];

export function actionColors(actions: string[]): string[] {
  return actions.map((label, i) => {
    if (/^fold/i.test(label)) return "#6b7280";
    if (/^(check|call)/i.test(label)) return ACTION_PALETTE[0];
    return ACTION_PALETTE[i] ?? ACTION_PALETTE[ACTION_PALETTE.length - 1];
  });
}

export interface ClassAgg {
  label: string;
  strategy: number[];
  ev: number;
  equity: number;
  count: number;
  rep: string; // a representative concrete combo, e.g. "AsKs"
}

function classOf(hand: string): string {
  const r0 = hand[0];
  const s0 = hand[1];
  const r1 = hand[2];
  const s1 = hand[3];
  if (r0 === r1) return r0 + r1;
  const [hi, lo] = RANK_INDEX[r0] < RANK_INDEX[r1] ? [r0, r1] : [r1, r0];
  return hi + lo + (s0 === s1 ? "s" : "o");
}

function aggregate(spot: SpotContract): Map<string, ClassAgg> {
  const map = new Map<string, ClassAgg>();
  for (const h of spot.hands) {
    const label = classOf(h.hand);
    let agg = map.get(label);
    if (!agg) {
      agg = { label, strategy: spot.actions.map(() => 0), ev: 0, equity: 0, count: 0, rep: h.hand };
      map.set(label, agg);
    }
    for (let a = 0; a < h.strategy.length; a++) agg.strategy[a] += h.strategy[a];
    agg.ev += h.ev;
    agg.equity += h.equity;
    agg.count += 1;
  }
  // convert sums to averages
  for (const agg of map.values()) {
    agg.strategy = agg.strategy.map((s) => s / agg.count);
    agg.ev /= agg.count;
    agg.equity /= agg.count;
  }
  return map;
}

function cellLabel(i: number, j: number): string {
  if (i === j) return RANKS[i] + RANKS[j];
  if (i < j) return RANKS[i] + RANKS[j] + "s"; // upper-right triangle = suited
  return RANKS[j] + RANKS[i] + "o"; // lower-left = offsuit
}

export function renderGrid(
  spot: SpotContract,
  onSelect: (agg: ClassAgg) => void,
): { el: HTMLElement; selectDefault: () => void } {
  const colors = actionColors(spot.actions);
  const map = aggregate(spot);

  const grid = document.createElement("div");
  grid.className = "grid";

  let firstAgg: ClassAgg | null = null;
  let selectedEl: HTMLElement | null = null;

  const select = (cell: HTMLElement, agg: ClassAgg) => {
    if (selectedEl) selectedEl.classList.remove("cell--selected");
    cell.classList.add("cell--selected");
    selectedEl = cell;
    onSelect(agg);
  };

  for (let i = 0; i < 13; i++) {
    for (let j = 0; j < 13; j++) {
      const label = cellLabel(i, j);
      const agg = map.get(label);
      const cell = document.createElement("div");
      cell.className = "cell";
      if (i === j) cell.classList.add("cell--pair");

      if (agg && agg.count > 0) {
        const bars = document.createElement("div");
        bars.className = "bars";
        agg.strategy.forEach((freq, a) => {
          if (freq <= 0.001) return;
          const seg = document.createElement("div");
          seg.className = "seg";
          seg.style.height = `${freq * 100}%`;
          seg.style.background = colors[a];
          bars.appendChild(seg);
        });
        cell.appendChild(bars);
        cell.addEventListener("click", () => select(cell, agg));
        if (!firstAgg) {
          firstAgg = agg;
        }
      } else {
        cell.classList.add("cell--empty");
      }

      const tag = document.createElement("span");
      tag.className = "cell-label";
      tag.textContent = label;
      cell.appendChild(tag);

      // remember the first in-range cell so we can auto-select it
      if (agg && agg.count > 0 && firstAgg === agg && !selectedEl) {
        cell.dataset.first = "1";
      }

      grid.appendChild(cell);
    }
  }

  const selectDefault = () => {
    const first = grid.querySelector<HTMLElement>('.cell[data-first="1"]');
    if (first && firstAgg) select(first, firstAgg);
  };

  return { el: grid, selectDefault };
}
