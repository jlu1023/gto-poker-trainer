// The table view: an original felt ellipse with hero (bottom), villain (top), board (center), pot.
// Real poker tables are referenced for layout proportions only; every element here is drawn from
// CSS/SVG primitives, no branded assets.

import type { SpotContract } from "./types";
import { cardSvg, cardBackSvg } from "./cards";

function feltSvg(): string {
  return `<svg class="felt" viewBox="0 0 800 500" preserveAspectRatio="xMidYMid meet" aria-hidden="true">
    <defs>
      <radialGradient id="feltg" cx="50%" cy="42%" r="70%">
        <stop offset="0%" stop-color="var(--felt-hi)"/>
        <stop offset="70%" stop-color="var(--felt)"/>
        <stop offset="100%" stop-color="var(--felt-lo)"/>
      </radialGradient>
    </defs>
    <ellipse cx="400" cy="250" rx="372" ry="224" fill="url(#feltg)"
      stroke="var(--felt-rim)" stroke-width="12"/>
    <ellipse cx="400" cy="250" rx="350" ry="206" fill="none"
      stroke="rgba(255,255,255,0.06)" stroke-width="2"/>
  </svg>`;
}

function handRow(cardsHtml: string, extra = ""): string {
  return `<div class="hand-row ${extra}">${cardsHtml}</div>`;
}

/** Build the table element for `spot`. `heroCards` is the selected hand's two cards, or null. */
export function renderTable(spot: SpotContract, heroCards: string[] | null): HTMLElement {
  const el = document.createElement("div");
  el.className = "table";

  const villainBacks = cardBackSvg(46) + cardBackSvg(46);
  const boardCards = spot.board.map((c) => cardSvg(c, 60)).join("");
  const heroHtml = heroCards
    ? heroCards.map((c) => cardSvg(c, 64)).join("")
    : cardBackSvg(64) + cardBackSvg(64);

  el.innerHTML = `
    ${feltSvg()}
    <div class="seat seat--villain">
      <div class="seat-label">${spot.villain.seat} · ${spot.villain.position}</div>
      ${handRow(villainBacks, "villain")}
    </div>
    <div class="board-area">
      <div class="pot">Pot ${spot.pot_bb.toFixed(1)} bb</div>
      ${handRow(boardCards, "board")}
      <div class="spr">eff ${spot.eff_stack_bb.toFixed(1)} bb · SPR ${(spot.eff_stack_bb / spot.pot_bb).toFixed(1)}</div>
    </div>
    <div class="seat seat--hero">
      ${handRow(heroHtml, "hero")}
      <div class="seat-label hero-label">${spot.hero.seat} · ${spot.hero.position} <span class="tag-hero">hero</span></div>
    </div>
  `;
  return el;
}
