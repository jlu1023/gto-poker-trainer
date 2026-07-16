// The explore-mode game-tree navigator: walk an ALREADY-CACHED solve down the action tree
// (POST /api/node, D55). It never solves; the server 409s on an uncached spot and the strip
// says so.
//
// Two ideas carry this feature, and they are load-bearing, not decoration:
//
// 1. The CONFIDENCE INDICATOR. Solve quality is GLOBAL (root exploitability <= 0.05% of pot),
//    not local. A low-reach subtree is trained far less than the root, so its numbers can carry
//    real local error even though the solve as a whole converged. The reach-gate admits nodes up
//    to `gate.display_gate_pct_pot` (5% of pot) rather than a purist 1% ONLY because every
//    displayed node shows its own dominance: a ~0.05% node and a ~4.5% node both render, but
//    they must never look equally authoritative. Four signals co-vary by tier (colour, tier
//    word, meter fill, and the literal % of pot) so the difference is legible at a glance and
//    means something concrete. If this indicator ever stops rendering, the honest gate is 1%,
//    not 5%: 5%-with-no-indicator is showing misleading nodes as gospel.
//
// 2. The SELF-EXPLAINING BOUNDARY. Where the navigator will not descend it says why, in plain
//    terms, always visible - never a silent greyed button. The limit becomes the lesson.

import type { GateContract, NodeContract } from "./types";
import { humanAction } from "./grid";
import { SERVER } from "./solve";

export type Tier = "high" | "fair" | "marginal";

/** Dominance (% of pot) -> confidence tier, against the server-shipped thresholds. Pure. */
export function tierOf(dominancePct: number, gate: GateContract): Tier {
  if (dominancePct <= gate.confidence_high_pct_pot) return "high";
  if (dominancePct <= gate.confidence_fair_pct_pot) return "fair";
  return "marginal";
}

const TIER_WORD: Record<Tier, string> = {
  high: "high confidence",
  fair: "fair confidence",
  marginal: "marginal confidence",
};

const TIER_GLOSS: Record<Tier, string> = {
  high: "this node is trained to about the same standard as the root",
  fair: "trained below the root: read small frequency differences with care",
  marginal: "near the trust limit: treat exact frequencies here as indicative only",
};

/** Legible at both ends of the range the gate admits (~0.02% to ~5%), never a bare "0%". */
export function formatPct(pct: number): string {
  if (pct < 0.01) return "<0.01%";
  return `${pct.toFixed(2)}%`;
}

/** Meter fill as a share of the gate, floored so a near-zero node still shows a sliver
 *  (a sliver reads as "almost no local error"; an empty bar reads as "no data"). */
function meterWidth(pct: number, gate: GateContract): number {
  const share = (pct / gate.display_gate_pct_pot) * 100;
  return Math.max(3, Math.min(100, share));
}

/** The per-node confidence indicator. Tier drives colour + word + meter; the literal
 *  dominance value is always spelled out so the signal stays concrete. */
function confidenceHtml(node: NodeContract): string {
  const tier = tierOf(node.dominance_pct, node.gate);
  return `
    <div class="nav-conf" id="navConf" data-tier="${tier}"
         data-dominance="${node.dominance_pct.toFixed(4)}">
      <div class="nc-head">
        <span class="nc-dot"></span>
        <span class="nc-tier" id="navConfTier">${TIER_WORD[tier]}</span>
        <span class="nc-val" id="navConfVal">${formatPct(node.dominance_pct)}<em>of pot local error</em></span>
      </div>
      <div class="nc-track"><div class="nc-fill" style="width:${meterWidth(node.dominance_pct, node.gate).toFixed(1)}%"></div></div>
      <p class="nc-gloss">${TIER_GLOSS[tier]}</p>
    </div>`;
}

/** Why the navigator stops at this child. Plain terms: the reason IS the lesson about
 *  global-vs-local convergence, so it never degrades to a greyed button. */
function boundaryReason(
  child: "decision" | "chance" | "terminal",
  dominancePct: number | null,
  gate: GateContract,
): string {
  if (child === "chance") {
    return "the turn card is dealt here: this solve covers the flop layer, so the walk stops.";
  }
  if (child === "terminal") {
    return "the hand ends here: there is nothing further to walk.";
  }
  if (dominancePct === null) {
    return "this line cannot be read back reliably, so the navigator will not show it.";
  }
  return (
    `beyond here the solve is not locally trained enough to trust: low-reach subtree. ` +
    `The solve converged globally (root under 0.05% of pot), but this line is reached so rarely ` +
    `that its own numbers still carry ${formatPct(dominancePct)} of pot of local error, past the ` +
    `${formatPct(gate.display_gate_pct_pot)} trust gate.`
  );
}

function actionsHtml(node: NodeContract): string {
  const rows = node.actions_meta.map((a, i) => {
    const label = humanAction(a.label);
    if (a.descendable) {
      const dom = a.child_dominance_pct ?? 0;
      const tier = tierOf(dom, node.gate);
      return `
        <button class="nav-act" type="button" data-idx="${i}" data-state="open" data-tier="${tier}">
          <span class="na-label">${label}</span>
          <span class="na-dom">${formatPct(dom)}</span>
          <span class="na-go">descend</span>
        </button>`;
    }
    const state = a.child === "decision" ? "blocked" : "leaf";
    const dom =
      a.child_dominance_pct !== null
        ? `<span class="na-dom na-dom--over">${formatPct(a.child_dominance_pct)}</span>`
        : "";
    return `
      <div class="nav-act nav-act--stop" data-idx="${i}" data-state="${state}">
        <span class="na-label">${label}</span>
        ${dom}
        <p class="na-why">${boundaryReason(a.child, a.child_dominance_pct, node.gate)}</p>
      </div>`;
  });
  return `<div class="nav-acts" id="navActs">${rows.join("")}</div>`;
}

function crumbsHtml(path: string[]): string {
  const items = ["Root", ...path.map(humanAction)];
  return `
    <nav class="nav-crumbs" id="navCrumbs" aria-label="tree path">
      ${items
        .map((label, i) =>
          i === items.length - 1
            ? `<span class="crumb crumb--cur" aria-current="true">${label}</span>`
            : `<button class="crumb" type="button" data-depth="${i}">${label}</button>`,
        )
        .join('<span class="crumb-sep">›</span>')}
    </nav>`;
}

export interface NavigatorDeps {
  host: HTMLElement;
  /** Render the stage (grid / table / detail) from this node. */
  onNode: (node: NodeContract) => void;
}

export interface NavigatorHandle {
  /** Enter the tree at the root of a cached spot. */
  open(board: string[], depth: number, potType: string): Promise<void>;
  /** Leave the navigator (mode switch / blank stage). */
  close(): void;
}

export function initNavigator(deps: NavigatorDeps): NavigatorHandle {
  const { host, onNode } = deps;
  let board: string[] = [];
  let depth = 100;
  let potType = "srp";
  let path: string[] = [];
  // guards an in-flight walk: a second click mid-fetch must not race a stale render onto stage
  let busy = false;

  const setStatus = (text: string, state: "error" | "info") => {
    host.hidden = false;
    host.dataset.state = state;
    host.innerHTML = `<p class="nav-msg" data-state="${state}">${text}</p>`;
  };

  async function walk(next: string[]): Promise<void> {
    if (busy) return;
    busy = true;
    host.dataset.busy = "1";
    try {
      const res = await fetch(`${SERVER}/api/node`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ board, depth, pot_type: potType, path: next }),
      });
      if (!res.ok) {
        const body = await res.json().catch(() => ({}) as { error?: string });
        const why =
          res.status === 409
            ? "this spot is not in the matrix, so there is no tree to walk (the navigator never solves)."
            : (body.error ?? `the server returned ${res.status}.`);
        setStatus(`Tree walk unavailable: ${why}`, "error");
        return;
      }
      const node: NodeContract = await res.json();
      path = node.path;
      render(node);
      onNode(node);
    } catch (e) {
      setStatus(`Tree walk unavailable: ${e instanceof Error ? e.message : String(e)}`, "error");
    } finally {
      busy = false;
      host.dataset.busy = "0";
    }
  }

  function render(node: NodeContract): void {
    host.hidden = false;
    host.dataset.state = "ok";
    host.innerHTML = `
      <div class="nav-head">
        ${crumbsHtml(node.path)}
        <span class="nav-actor">${node.hero.position} to act</span>
      </div>
      ${confidenceHtml(node)}
      ${actionsHtml(node)}`;

    host.querySelectorAll<HTMLButtonElement>(".crumb[data-depth]").forEach((btn) => {
      btn.addEventListener("click", () => {
        void walk(path.slice(0, Number(btn.dataset.depth)));
      });
    });
    host.querySelectorAll<HTMLButtonElement>('.nav-act[data-state="open"]').forEach((btn) => {
      btn.addEventListener("click", () => {
        const meta = node.actions_meta[Number(btn.dataset.idx)];
        if (meta) void walk([...path, meta.label]);
      });
    });
  }

  return {
    async open(b: string[], d: number, p: string) {
      board = b;
      depth = d;
      potType = p;
      path = [];
      await walk([]);
    },
    close() {
      host.hidden = true;
      host.innerHTML = "";
      path = [];
    },
  };
}
