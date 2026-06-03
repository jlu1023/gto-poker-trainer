// Original playing-card rendering. Every card face is drawn from CSS/SVG primitives here:
// rounded rect + rank glyph + suit symbol. No external or branded card-image assets.

const SUIT_GLYPH: Record<string, string> = { s: "♠", h: "♥", d: "♦", c: "♣" };
const RED_SUITS = new Set(["h", "d"]);

function rankLabel(r: string): string {
  return r === "T" ? "10" : r;
}

/** SVG markup for one face-up card, e.g. "Ks". */
export function cardSvg(card: string, width = 58): string {
  const rank = rankLabel(card[0] ?? "?");
  const suit = card[1] ?? "?";
  const glyph = SUIT_GLYPH[suit] ?? "?";
  const ink = RED_SUITS.has(suit) ? "var(--card-red)" : "var(--card-ink)";
  const height = Math.round(width * 1.4);
  const rankSize = rank.length > 1 ? 26 : 32;

  return `<svg class="card" width="${width}" height="${height}" viewBox="0 0 100 140"
      role="img" aria-label="${rank} of ${suit}">
    <rect x="2" y="2" width="96" height="136" rx="11" fill="var(--card-face)"
      stroke="rgba(20,24,28,0.25)" stroke-width="1.5"/>
    <text x="12" y="34" font-size="${rankSize}" font-weight="800"
      font-family="'JetBrains Mono', monospace" fill="${ink}">${rank}</text>
    <text x="14" y="60" font-size="22" fill="${ink}">${glyph}</text>
    <text x="50" y="98" font-size="56" text-anchor="middle" fill="${ink}" opacity="0.92">${glyph}</text>
  </svg>`;
}

/** SVG markup for a face-down card back (original crosshatch motif). */
export function cardBackSvg(width = 58): string {
  const height = Math.round(width * 1.4);
  return `<svg class="card card-back" width="${width}" height="${height}" viewBox="0 0 100 140"
      role="img" aria-label="face-down card">
    <defs>
      <pattern id="hatch" width="12" height="12" patternUnits="userSpaceOnUse"
        patternTransform="rotate(45)">
        <rect width="12" height="12" fill="var(--card-back)"/>
        <line x1="0" y1="0" x2="0" y2="12" stroke="var(--accent)" stroke-width="1.4" opacity="0.5"/>
      </pattern>
    </defs>
    <rect x="2" y="2" width="96" height="136" rx="11" fill="var(--card-back)"
      stroke="var(--accent)" stroke-width="1.5"/>
    <rect x="9" y="9" width="82" height="122" rx="7" fill="url(#hatch)" opacity="0.85"/>
    <circle cx="50" cy="70" r="13" fill="none" stroke="var(--accent)" stroke-width="2" opacity="0.75"/>
  </svg>`;
}
