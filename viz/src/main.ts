import "./style.css";
import type { SolveComplete, SpotContract } from "./types";
import type { ClassAgg } from "./grid";
import { renderTable } from "./table";
import { renderGrid, actionColors } from "./grid";
import { initSolvePanel } from "./solve";
import { initTrainer } from "./trainer";
import { initReview } from "./review";

const app = document.querySelector<HTMLDivElement>("#app");
if (!app) throw new Error("missing #app");

function humanAction(label: string): string {
  const m = label.match(/^([A-Za-z]+)\((\d+)\)$/);
  if (!m) return label;
  if (m[1] === "AllIn") return "All-in";
  return `${m[1]} ${m[2]}`;
}

// The topbar hosts the SINGLE solve control: depth + board (+ hand in train mode) + one
// action button. The depth select is shared by both modes; the board input and the button
// exist once per mode (.sw-explore / .sw-train, CSS-swapped) because the two flows keep
// deliberately separate state machines: the explore path disables its input/button around a
// live solve, while the train path (commit-then-reveal, D35) must never couple to that. The
// ids are pinned by the gate verifiers (verify_viz + verify_solve: #tDepth/#flop/#solveBtn;
// verify_trainer: #tDepth/#tBoard/#tHand/#tSetup) - do not rename or merge them.
app.innerHTML = `
  <header class="topbar">
    <div class="brand"><span class="brand-mark">◈</span>GTO&nbsp;<em>Lab</em></div>
    <div class="mode-toggle" role="tablist" aria-label="mode">
      <button id="modeExplore" class="mode-btn mode-btn--active" type="button">explore</button>
      <button id="modeTrain" class="mode-btn" type="button">train</button>
      <button id="modeReview" class="mode-btn" type="button">review</button>
    </div>
    <div class="solve-wrap">
      <label for="tDepth"><span class="sw-explore">solve · srp</span><span class="sw-train">train · srp</span></label>
      <select id="tDepth" aria-label="stack depth">
        <option value="50">50bb</option>
        <option value="100">100bb</option>
      </select>
      <input id="flop" class="sw-explore" type="text" placeholder="Ks7h2c" maxlength="11"
        spellcheck="false" autocomplete="off" aria-label="flop to solve" />
      <input id="tBoard" class="sw-train" type="text" placeholder="Ks7h2c" maxlength="11"
        spellcheck="false" autocomplete="off" aria-label="board to train" />
      <input id="tHand" class="sw-train" type="text" placeholder="AhKh" maxlength="7"
        spellcheck="false" autocomplete="off" aria-label="your hand" />
      <button id="solveBtn" class="sw-explore" type="button">Solve</button>
      <button id="tSetup" class="sw-train" type="button">Set up</button>
    </div>
    <div class="meta" id="meta"></div>
  </header>
  <div id="solveStatus" class="solve-status" data-state="idle" data-progress-count="0">
    <span class="ss-label" id="ssLabel"></span>
    <div class="ss-track" id="ssTrack"><div class="ss-fill" id="ssFill"></div></div>
    <span class="ss-value" id="ssValue"></span>
  </div>
  <div id="trainerHost" class="trainer" data-phase="entry" hidden></div>
  <div id="reviewHost" class="review" hidden></div>
  <main class="stage">
    <section class="table-panel" id="tableHost"></section>
    <aside class="data-panel">
      <div class="panel-head"><span class="ph-title">OOP flop strategy</span><span class="legend" id="legend"></span></div>
      <div class="grid-wrap" id="gridHost"></div>
      <div class="detail" id="detail"></div>
    </aside>
  </main>
`;

const meta = app.querySelector<HTMLDivElement>("#meta")!;
const depthSel = app.querySelector<HTMLSelectElement>("#tDepth")!;
const flopIn = app.querySelector<HTMLInputElement>("#flop")!;
const boardIn = app.querySelector<HTMLInputElement>("#tBoard")!;
const tableHost = app.querySelector<HTMLElement>("#tableHost")!;
const gridHost = app.querySelector<HTMLElement>("#gridHost")!;
const detailHost = app.querySelector<HTMLElement>("#detail")!;
const legendHost = app.querySelector<HTMLElement>("#legend")!;

// the last spot rendered from a live solve or trainer reveal (REAL solver output from the
// server, never mocked); re-entering explore restores it. Recall of any other cached board
// is re-typing it: the solve path is cache-first, so presets load instantly.
let lastSpot: SpotContract | null = null;

const TRAINER_PROMPT =
  "Set up a spot in the top bar (depth, board, your hand) and commit to an action. " +
  "The solver's answer reveals only after you commit.";

const EXPLORE_IDLE_TITLE = "No spot on stage";
const EXPLORE_IDLE_BODY =
  "Type a flop in the top bar, pick a depth, and Solve. " +
  "Cached boards load instantly; new boards stream a live solve.";

function showStageMessage(title: string, body: string) {
  tableHost.innerHTML = `<div class="empty"><h2>${title}</h2><p>${body}</p></div>`;
  gridHost.innerHTML = "";
  detailHost.innerHTML = "";
  legendHost.innerHTML = "";
}

function renderMeta(spot: SpotContract) {
  meta.innerHTML = `
    <span class="m-board">${spot.board.join(" ")}</span>
    <span class="m-chip">${spot.stack_depth_bb}bb</span>
    <span class="m-chip">${(spot.exploitability_pct_pot * 100).toFixed(3)}% exploit</span>
    <span class="m-chip">${spot.iterations} iters</span>`;
}

function renderLegend(spot: SpotContract, colors: string[]) {
  legendHost.innerHTML = spot.actions
    .map(
      (a, i) =>
        `<span class="legend-item"><span class="swatch" style="background:${colors[i]}"></span>${humanAction(a)}</span>`,
    )
    .join("");
}

function renderDetail(spot: SpotContract, colors: string[], agg: ClassAgg) {
  const hasEv = agg.evActions.length === spot.actions.length;
  const bars = spot.actions
    .map(
      (a, i) => `
      <div class="dbar ${hasEv ? "dbar--ev" : ""}">
        <div class="dbar-label">${humanAction(a)}</div>
        <div class="dbar-track"><div class="dbar-fill" style="width:${(agg.strategy[i] * 100).toFixed(0)}%;background:${colors[i]}"></div></div>
        <div class="dbar-val">${(agg.strategy[i] * 100).toFixed(0)}%</div>
        ${hasEv ? `<div class="dbar-ev">${agg.evActions[i].toFixed(1)}</div>` : ""}
      </div>`,
    )
    .join("");
  detailHost.innerHTML = `
    <div class="detail-head">
      <span class="detail-class">${agg.label}</span>
      <span class="detail-combos">${agg.count} combo${agg.count > 1 ? "s" : ""} · sample ${agg.rep}</span>
    </div>
    <div class="detail-bars">${bars}</div>
    <div class="detail-stats">
      <div><span class="stat-k">EV</span><span class="stat-v">${agg.ev.toFixed(1)}<em>chips</em></span></div>
      <div><span class="stat-k">Equity</span><span class="stat-v">${(agg.equity * 100).toFixed(1)}<em>%</em></span></div>
    </div>`;
}

/** Render a spot (from a live solve or a trainer reveal). When `selectClassLabel` is given
 *  (trainer reveal), that hand class is selected instead of the default first in-range cell. */
function renderSpot(spot: SpotContract, selectClassLabel?: string) {
  const colors = actionColors(spot.actions);
  renderMeta(spot);
  renderLegend(spot, colors);

  const drawTable = (heroCards: string[] | null) => {
    tableHost.replaceChildren(renderTable(spot, heroCards));
  };
  drawTable(null);

  const { el, selectDefault, selectClass } = renderGrid(spot, (agg) => {
    const hero = [agg.rep.slice(0, 2), agg.rep.slice(2, 4)];
    drawTable(hero);
    renderDetail(spot, colors, agg);
  });
  gridHost.replaceChildren(el);
  if (!selectClassLabel || !selectClass(selectClassLabel)) {
    selectDefault();
  }
}

/** A live solve (fresh or cached) finished: remember it, render it. */
function onSolved(payload: SolveComplete, selectClassLabel?: string) {
  const spot = payload.spot;
  lastSpot = spot;
  renderSpot(spot, selectClassLabel);
}

// ---- mode toggle (explore / train / review) ----
function initModeToggle() {
  const buttons: Record<string, HTMLButtonElement> = {
    explore: app!.querySelector<HTMLButtonElement>("#modeExplore")!,
    train: app!.querySelector<HTMLButtonElement>("#modeTrain")!,
    review: app!.querySelector<HTMLButtonElement>("#modeReview")!,
  };
  const trainerHost = document.querySelector<HTMLElement>("#trainerHost")!;
  const reviewHost = document.querySelector<HTMLElement>("#reviewHost")!;
  let current: "explore" | "train" | "review" = "explore";

  const setMode = (mode: "explore" | "train" | "review") => {
    // one control, two wirings: the board you typed follows you across modes. Sync on
    // LEAVING a mode (not entering) so a review detour cannot resurrect a stale value.
    // The dispatched input event re-runs each side's own validation.
    if (mode !== current) {
      if (current === "explore") {
        boardIn.value = flopIn.value;
        boardIn.dispatchEvent(new Event("input"));
      } else if (current === "train") {
        flopIn.value = boardIn.value;
        flopIn.dispatchEvent(new Event("input"));
      }
      current = mode;
    }
    document.body.classList.toggle("mode-train", mode === "train");
    document.body.classList.toggle("mode-review", mode === "review");
    for (const [m, btn] of Object.entries(buttons)) {
      btn.classList.toggle("mode-btn--active", m === mode);
    }
    trainerHost.hidden = mode !== "train";
    reviewHost.hidden = mode !== "review";
    if (mode === "train") {
      // entering (or re-clicking) train mode blanks the stage: nothing solver-derived
      // pre-commit (D35)
      showStageMessage("Trainer", TRAINER_PROMPT);
    } else if (mode === "explore") {
      if (lastSpot) {
        renderSpot(lastSpot);
      } else {
        showStageMessage(EXPLORE_IDLE_TITLE, EXPLORE_IDLE_BODY);
      }
    }
  };

  buttons.explore.addEventListener("click", () => setMode("explore"));
  buttons.train.addEventListener("click", () => setMode("train"));
  buttons.review.addEventListener("click", () => setMode("review"));
}

function main() {
  initSolvePanel(
    {
      input: flopIn,
      button: app!.querySelector<HTMLButtonElement>("#solveBtn")!,
      depth: depthSel,
      status: document.querySelector<HTMLElement>("#solveStatus")!,
      label: document.querySelector<HTMLElement>("#ssLabel")!,
      track: document.querySelector<HTMLElement>("#ssTrack")!,
      fill: document.querySelector<HTMLElement>("#ssFill")!,
      value: document.querySelector<HTMLElement>("#ssValue")!,
    },
    onSolved,
  );

  initModeToggle();
  initReview(document.querySelector<HTMLElement>("#reviewHost")!);
  initTrainer({
    host: document.querySelector<HTMLElement>("#trainerHost")!,
    depthSel,
    boardIn,
    handIn: app!.querySelector<HTMLInputElement>("#tHand")!,
    setupBtn: app!.querySelector<HTMLButtonElement>("#tSetup")!,
    onReveal: (payload, handClass) => onSolved(payload, handClass),
    blankStage: () => showStageMessage("Trainer", TRAINER_PROMPT),
  });

  // nothing on stage until the operator solves something; the solve path is cache-first,
  // so any stored board is one Solve away
  showStageMessage(EXPLORE_IDLE_TITLE, EXPLORE_IDLE_BODY);
}

main();
