// Phase 4: the manual GTO trainer. Enter a spot (depth + board + exact hand), COMMIT to an
// action before any solver data exists client-side, then solve (cache-first via the Phase 3
// server), grade the committed action, and reveal the full strategy.
//
// No-peeking is enforced by data absence (D35): pre-commit the client holds only the setup
// payload (action labels + booleans). The solve and grade requests fire on the commit click,
// never before. host.dataset.phase tracks the state machine for the verification gate.

import type { GradeResponse, SetupResponse, SolveComplete, Verdict } from "./types";
import { parseFlop, requestSolve, SERVER } from "./solve";
import { classOf } from "./grid";

const RANKS = "AKQJT98765432";
const SUITS = "shdc";
/// srp-v1 convergence target, for the progress fill (same as the explore strip).
const TARGET = 0.005;

const VERDICT_LABEL: Record<Verdict, string> = {
  correct: "Correct",
  fine_mix: "Fine mix",
  minor_leak: "Minor leak",
  clear_leak: "Clear leak",
};

interface HistoryRow {
  depth: number;
  board: string;
  hand: string;
  action: string;
  verdict: Verdict;
  evLossPctPot: number;
}

export interface TrainerDeps {
  host: HTMLElement;
  /** Render the solved spot into the stage with the operator's hand class selected. */
  onReveal(payload: SolveComplete, handClass: string): void;
  /** Blank the stage (entry/reset states must show no solver output). */
  blankStage(): void;
}

function normalizeCard(raw: string): string | null {
  if (raw.length !== 2) return null;
  const r = raw[0].toUpperCase();
  const s = raw[1].toLowerCase();
  if (!RANKS.includes(r) || !SUITS.includes(s)) return null;
  return r + s;
}

/** Parse a hand like "ahkh" into two normalized cards, or an error string. */
function parseHand(raw: string): { cards?: [string, string]; error?: string } {
  const s = raw.replace(/[\s,]+/g, "");
  if (s.length === 0) return { error: "" };
  if (s.length !== 4) return { error: "hand needs exactly 2 cards, e.g. AhKh" };
  const c1 = normalizeCard(s.slice(0, 2));
  const c2 = normalizeCard(s.slice(2, 4));
  if (!c1 || !c2) return { error: "illegal card in hand" };
  if (c1 === c2) return { error: `duplicate card '${c1}' in hand` };
  return { cards: [c1, c2] };
}

export function initTrainer(deps: TrainerDeps) {
  const { host } = deps;

  host.innerHTML = `
    <div class="t-row t-entry">
      <label for="tDepth">depth</label>
      <select id="tDepth" aria-label="stack depth">
        <option value="50">50bb</option>
        <option value="100">100bb</option>
      </select>
      <label for="tBoard">board</label>
      <input id="tBoard" type="text" placeholder="Ks7h2c" maxlength="11" spellcheck="false" autocomplete="off" />
      <label for="tHand">your hand</label>
      <input id="tHand" type="text" placeholder="AhKh" maxlength="7" spellcheck="false" autocomplete="off" />
      <button id="tSetup" type="button">Set up</button>
      <span class="t-err" id="tErr"></span>
    </div>
    <div class="t-row t-commit" id="tCommit" hidden>
      <span class="t-q" id="tQuestion"></span>
      <div class="t-actions" id="tActions"></div>
    </div>
    <div class="t-row t-progress" id="tProgress" hidden>
      <span class="ss-label" id="tProgLabel"></span>
      <div class="ss-track"><div class="ss-fill" id="tProgFill"></div></div>
      <span class="ss-value" id="tProgVal"></span>
    </div>
    <div class="t-grade" id="tGrade" hidden></div>
    <div class="t-history" id="tHistory" hidden></div>
  `;

  const depthSel = host.querySelector<HTMLSelectElement>("#tDepth")!;
  const boardIn = host.querySelector<HTMLInputElement>("#tBoard")!;
  const handIn = host.querySelector<HTMLInputElement>("#tHand")!;
  const setupBtn = host.querySelector<HTMLButtonElement>("#tSetup")!;
  const errEl = host.querySelector<HTMLElement>("#tErr")!;
  const commitRow = host.querySelector<HTMLElement>("#tCommit")!;
  const questionEl = host.querySelector<HTMLElement>("#tQuestion")!;
  const actionsEl = host.querySelector<HTMLElement>("#tActions")!;
  const progRow = host.querySelector<HTMLElement>("#tProgress")!;
  const progLabel = host.querySelector<HTMLElement>("#tProgLabel")!;
  const progFill = host.querySelector<HTMLElement>("#tProgFill")!;
  const progVal = host.querySelector<HTMLElement>("#tProgVal")!;
  const gradeEl = host.querySelector<HTMLElement>("#tGrade")!;
  const historyEl = host.querySelector<HTMLElement>("#tHistory")!;

  const history: HistoryRow[] = [];
  let setup: SetupResponse | null = null;
  let busy = false;

  const setPhase = (p: "entry" | "commit" | "solving" | "reveal") => {
    host.dataset.phase = p;
  };
  const setError = (msg: string) => {
    errEl.textContent = msg;
  };

  const resetToEntry = () => {
    setup = null;
    busy = false;
    commitRow.hidden = true;
    progRow.hidden = true;
    gradeEl.hidden = true;
    actionsEl.innerHTML = "";
    gradeEl.innerHTML = "";
    setError("");
    setPhase("entry");
    deps.blankStage();
  };

  const renderHistory = () => {
    if (history.length === 0) {
      historyEl.hidden = true;
      return;
    }
    historyEl.hidden = false;
    const total = history.reduce((s, r) => s + r.evLossPctPot, 0);
    const avg = total / history.length;
    const rows = history
      .map(
        (r) => `
        <div class="th-row">
          <span class="th-spot">${r.depth}bb ${r.board}</span>
          <span class="th-hand">${r.hand}</span>
          <span class="th-action">${r.action}</span>
          <span class="th-verdict v-${r.verdict}">${VERDICT_LABEL[r.verdict]}</span>
          <span class="th-loss">-${(r.evLossPctPot * 100).toFixed(2)}% pot</span>
        </div>`,
      )
      .join("");
    historyEl.innerHTML = `
      <div class="th-head">
        <span>session · ${history.length} spot${history.length > 1 ? "s" : ""}</span>
        <span>total EV loss ${(total * 100).toFixed(2)}% pot · avg ${(avg * 100).toFixed(2)}%</span>
      </div>
      ${rows}`;
  };

  // ---- ENTRY -> setup ----
  const doSetup = async () => {
    if (busy) return;
    const flop = parseFlop(boardIn.value);
    if (!flop.cards) {
      setError(flop.error || "enter a 3-card board");
      return;
    }
    const hand = parseHand(handIn.value);
    if (!hand.cards) {
      setError(hand.error || "enter your 2-card hand");
      return;
    }
    if (hand.cards.some((c) => flop.cards!.includes(c))) {
      setError("hand collides with the board");
      return;
    }
    setError("");
    busy = true;
    setupBtn.disabled = true;

    let res: Response;
    try {
      res = await fetch(
        `${SERVER}/api/setup?depth=${depthSel.value}&board=${flop.cards.join("")}&hand=${hand.cards.join("")}`,
      );
    } catch {
      setError("solve server unreachable; start it with: cargo run --release -p gto-server");
      busy = false;
      setupBtn.disabled = false;
      return;
    }
    const body = await res.json();
    busy = false;
    setupBtn.disabled = false;
    if (!res.ok) {
      setError(body.error ?? `setup failed: HTTP ${res.status}`);
      return;
    }
    const s = body as SetupResponse;
    if (!s.hand_in_range) {
      setError(`${s.hand} is not in the BB's preflop range at ${s.depth}bb; pick a hand the BB actually holds here`);
      return;
    }

    setup = s;
    questionEl.textContent = `${s.hand} on ${s.board.join(" ")} (${s.depth}bb SRP, BB to act)${s.cached ? " · cached" : ""} — your action?`;
    actionsEl.innerHTML = "";
    for (const a of s.actions) {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "t-action-btn";
      btn.textContent = a;
      btn.addEventListener("click", () => void commit(a));
      actionsEl.appendChild(btn);
    }
    commitRow.hidden = false;
    gradeEl.hidden = true;
    gradeEl.innerHTML = "";
    setPhase("commit");
    deps.blankStage(); // nothing solver-derived on screen until commit
  };

  // ---- COMMIT -> solve -> grade -> reveal ----
  const commit = async (action: string) => {
    if (busy || !setup) return;
    const s = setup;
    busy = true;
    setPhase("solving");
    commitRow.hidden = true;
    progRow.hidden = false;
    progLabel.textContent = s.cached ? `loading ${s.board.join("")}` : `solving ${s.board.join("")}`;
    progFill.style.width = "0%";
    progVal.textContent = s.cached ? "cached; no solve needed" : "waiting for the first convergence sample";
    host.dataset.progressCount = "0";

    let baseline: number | null = null;

    void requestSolve(
      s.board,
      {
        onProgress(p) {
          host.dataset.progressCount = String(Number(host.dataset.progressCount ?? "0") + 1);
          const e = p.exploitability_pct_pot;
          if (baseline === null) baseline = Math.max(e, TARGET * 1.0001);
          const num = Math.log(baseline) - Math.log(Math.max(e, TARGET));
          const den = Math.log(baseline) - Math.log(TARGET);
          progFill.style.width = `${(Math.min(1, Math.max(0, num / den)) * 100).toFixed(1)}%`;
          progVal.textContent = `${(e * 100).toFixed(2)}% pot · iter ${p.iteration}`;
        },
        async onComplete(payload) {
          progRow.hidden = true;
          // grade the committed action against the now-stored solution
          let res: Response;
          try {
            res = await fetch(`${SERVER}/api/grade`, {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({
                board: s.board,
                depth: s.depth,
                hand: s.hand,
                action,
              }),
            });
          } catch {
            busy = false;
            setError("grading failed: server unreachable");
            setPhase("commit");
            commitRow.hidden = false;
            return;
          }
          const body = await res.json();
          if (!res.ok) {
            busy = false;
            setError(body.error ?? `grading failed: HTTP ${res.status}`);
            setPhase("commit");
            commitRow.hidden = false;
            return;
          }
          const g = body as GradeResponse;
          renderGrade(g, payload);
          history.push({
            depth: g.depth,
            board: s.board.join(""),
            hand: g.hand,
            action: g.chosen_action,
            verdict: g.verdict,
            evLossPctPot: g.ev_loss_pct_pot,
          });
          renderHistory();
          deps.onReveal(payload, classOf(g.hand));
          busy = false;
          setPhase("reveal");
        },
        onError(message) {
          busy = false;
          progRow.hidden = true;
          setError(`solve failed: ${message}`);
          setPhase("commit");
          commitRow.hidden = false;
        },
      },
      s.depth,
    );
  };

  const renderGrade = (g: GradeResponse, solve: SolveComplete) => {
    const lossPct = (g.ev_loss_pct_pot * 100).toFixed(2);
    const freqPct = (g.frequency * 100).toFixed(0);
    const maxFreq = Math.max(...g.distribution.map((d) => d.frequency), 1e-9);
    const dist = g.distribution
      .map((d) => {
        const isBest = d.action === g.best_action;
        const isChosen = d.action === g.chosen_action;
        return `
        <div class="gd-row ${isChosen ? "gd-row--chosen" : ""}">
          <span class="gd-action">${d.action}${isBest ? " ★" : ""}</span>
          <div class="gd-track"><div class="gd-fill" style="width:${((d.frequency / maxFreq) * 100).toFixed(1)}%"></div></div>
          <span class="gd-freq">${(d.frequency * 100).toFixed(0)}%</span>
          <span class="gd-ev">${d.ev.toFixed(1)}</span>
        </div>`;
      })
      .join("");
    gradeEl.innerHTML = `
      <div class="g-head">
        <span class="g-verdict v-${g.verdict}" data-verdict="${g.verdict}">${VERDICT_LABEL[g.verdict]}</span>
        <span class="g-summary">
          you: <strong>${g.chosen_action}</strong> · solver plays it ${freqPct}% ·
          EV loss ${g.ev_loss_chips.toFixed(2)} chips (${lossPct}% pot)
        </span>
        ${g.mapped_from ? `<span class="g-mapped">mapped from ${g.mapped_from} (not in the tree)</span>` : ""}
        ${solve.shrunk ? `<span class="g-mapped">solved with a reduced tree (RAM ceiling)</span>` : ""}
      </div>
      <div class="g-dist">
        <div class="gd-row gd-row--head">
          <span class="gd-action">action</span><span></span>
          <span class="gd-freq">freq</span><span class="gd-ev">EV</span>
        </div>
        ${dist}
      </div>
      <button id="tNext" type="button">next spot</button>`;
    gradeEl.hidden = false;
    gradeEl.querySelector<HTMLButtonElement>("#tNext")!.addEventListener("click", resetToEntry);
  };

  setupBtn.addEventListener("click", () => void doSetup());
  for (const el of [boardIn, handIn]) {
    el.addEventListener("keydown", (e) => {
      if (e.key === "Enter") void doSetup();
    });
    el.addEventListener("input", () => setError(""));
  }

  setPhase("entry");
}
