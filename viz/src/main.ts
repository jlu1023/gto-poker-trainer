import "./style.css";
import type { SolveComplete, SpotContract } from "./types";
import type { ClassAgg } from "./grid";
import { renderTable } from "./table";
import { renderGrid, actionColors, classOf, humanAction } from "./grid";
import { initSolvePanel } from "./solve";
import { initTrainer, parseHand } from "./trainer";
import { initReview } from "./review";
import { initNavigator } from "./navigator";

const app = document.querySelector<HTMLDivElement>("#app");
if (!app) throw new Error("missing #app");

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
      <label for="tDepth"><span class="sw-explore">solve</span><span class="sw-train">train</span></label>
      <select id="tPot" aria-label="pot type">
        <option value="srp">srp</option>
        <option value="3bp">3bp</option>
        <option value="4bp">4bp</option>
      </select>
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
    <div class="hand-wrap">
      <label for="exploreHand">your hand</label>
      <input id="exploreHand" type="text" placeholder="AdAc" maxlength="7"
        spellcheck="false" autocomplete="off" disabled
        aria-label="your hand (highlights its grid cell)" />
      <span class="hand-hint" id="handHint"></span>
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
      <div class="panel-head"><span class="ph-title" id="phTitle">OOP flop strategy</span><span class="legend" id="legend"></span></div>
      <div class="nav" id="navHost" hidden></div>
      <div class="grid-wrap" id="gridHost"></div>
      <div class="detail" id="detail"></div>
    </aside>
  </main>
`;

const meta = app.querySelector<HTMLDivElement>("#meta")!;
const depthSel = app.querySelector<HTMLSelectElement>("#tDepth")!;
const potSel = app.querySelector<HTMLSelectElement>("#tPot")!;

// 4-bet pots exist at 100bb only (50bb is jam-or-fold, out of scope by decision): the
// depth options follow the pot type so an unsupported pair cannot be submitted.
function syncDepthToPot(): void {
  const opt50 = depthSel.querySelector<HTMLOptionElement>('option[value="50"]')!;
  const lock100 = potSel.value === "4bp";
  opt50.disabled = lock100;
  if (lock100 && depthSel.value === "50") depthSel.value = "100";
}
potSel.addEventListener("change", syncDepthToPot);
syncDepthToPot();
const flopIn = app.querySelector<HTMLInputElement>("#flop")!;
const boardIn = app.querySelector<HTMLInputElement>("#tBoard")!;
const handHl = app.querySelector<HTMLInputElement>("#exploreHand")!;
const handHint = app.querySelector<HTMLElement>("#handHint")!;
const tableHost = app.querySelector<HTMLElement>("#tableHost")!;
const gridHost = app.querySelector<HTMLElement>("#gridHost")!;
const detailHost = app.querySelector<HTMLElement>("#detail")!;
const legendHost = app.querySelector<HTMLElement>("#legend")!;
const navHost = app.querySelector<HTMLElement>("#navHost")!;
const phTitle = app.querySelector<HTMLElement>("#phTitle")!;

// Descending re-renders the stage from the node the navigator walked to. A NodeContract IS a
// SpotContract, so the grid / table / detail render it unchanged: they cannot tell a node from
// a root, which is exactly why the confidence indicator has to carry the difference.
const treeNav = initNavigator({
  host: navHost,
  onNode: (node) => renderSpot(node),
});

// the last spot rendered from a live solve or trainer reveal (REAL solver output from the
// server, never mocked); re-entering explore restores it. Recall of any other cached board
// is re-typing it: the solve path is cache-first, so presets load instantly. Depth + pot are
// captured AT SOLVE TIME, not read back off the selects: the operator can re-point those
// without re-solving, and the navigator must walk the tree that is actually on stage.
let lastCtx: { spot: SpotContract; depth: number; potType: string } | null = null;

// the navigator walks the tree of the spot on stage, and only in explore mode: a trainer
// reveal renders through this same path, and pre-commit the trainer must stay uncoupled (D35).
let currentMode: "explore" | "train" | "review" = "explore";

// the live grid's selection handles + the spot it renders, for the explore hand-highlight
// (null whenever no grid is on stage; renderGrid's closures are otherwise ephemeral)
let currentGrid: {
  spot: SpotContract;
  selectClass: (label: string) => boolean;
  selectDefault: () => void;
} | null = null;

const TRAINER_PROMPT =
  "Set up a spot in the top bar (depth, board, your hand) and commit to an action. " +
  "The solver's answer reveals only after you commit.";

const EXPLORE_IDLE_TITLE = "No spot on stage";
const EXPLORE_IDLE_BODY =
  "Type a flop in the top bar, pick a depth, and Solve. " +
  "Cached boards load instantly; new boards stream a live solve.";

function setHandHint(text: string, isError = false) {
  handHint.textContent = text;
  handHint.dataset.state = isError ? "error" : "info";
}

/** The explore hand-highlight: map typed hole cards to their grid cell and select it.
 *  Claims CELL MEMBERSHIP only - selection shows the class aggregate exactly as a cell
 *  click does, never the typed combo's own strategy (blocker effects make individual
 *  combos diverge from the class mean; a per-combo readout is a separate feature).
 *  Returns true only when a valid, in-range hand was highlighted. */
function applyHandHighlight(): boolean {
  if (!currentGrid) return false;
  const stripped = handHl.value.replace(/[\s,]+/g, "");
  if (stripped.length < 4) {
    setHandHint(""); // empty or still typing: no claim, no error noise
    return false;
  }
  const parsed = parseHand(handHl.value);
  if (!parsed.cards) {
    setHandHint(parsed.error || "enter your 2-card hand", true);
    return false;
  }
  const onBoard = parsed.cards.find((c) => currentGrid!.spot.board.includes(c));
  if (onBoard) {
    setHandHint(`${onBoard} is on the board`, true);
    return false;
  }
  const label = classOf(parsed.cards.join(""));
  if (!currentGrid.selectClass(label)) {
    setHandHint(`${label} · not in range here`);
    return false;
  }
  setHandHint(`cell ${label} · class average`);
  return true;
}

function showStageMessage(title: string, body: string) {
  tableHost.innerHTML = `<div class="empty"><h2>${title}</h2><p>${body}</p></div>`;
  gridHost.innerHTML = "";
  detailHost.innerHTML = "";
  legendHost.innerHTML = "";
  currentGrid = null;
  treeNav.close(); // no tree strip without a spot on stage
  handHl.disabled = true;
  setHandHint("");
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
      <span class="detail-combos">${agg.count} combo${agg.count > 1 ? "s" : ""}${
        Math.abs(agg.mass - agg.count) > 0.01 ? ` · mass ${agg.mass.toFixed(2)}` : ""
      } · sample ${agg.rep}</span>
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
  // whose strategy this grid is: the actor changes as the navigator descends, so the panel
  // head follows the node rather than claiming "OOP" forever
  phTitle.textContent = `${spot.hero.position} ${spot.street} strategy`;

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
  currentGrid = { spot, selectClass, selectDefault };
  handHl.disabled = false;
  // selection precedence: trainer-reveal class > valid in-range typed hand > default.
  // The hand highlight re-validates against THIS board, so a hand that collides on the
  // new board shows its error and the default selection stands.
  if (!(selectClassLabel && selectClass(selectClassLabel)) && !applyHandHighlight()) {
    selectDefault();
  }
}

/** A live solve (fresh or cached) finished: remember it, render it, and (explore only) enter
 *  the tree at its root so the navigator's confidence signal covers the root too. */
function onSolved(payload: SolveComplete, selectClassLabel?: string) {
  const spot = payload.spot;
  lastCtx = { spot, depth: Number(depthSel.value), potType: potSel.value };
  renderSpot(spot, selectClassLabel);
  if (currentMode === "explore") {
    void treeNav.open(spot.board, lastCtx.depth, lastCtx.potType);
  }
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
      currentMode = mode;
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
      if (lastCtx) {
        renderSpot(lastCtx.spot);
        void treeNav.open(lastCtx.spot.board, lastCtx.depth, lastCtx.potType);
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
      pot: potSel,
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
    potSel,
    boardIn,
    handIn: app!.querySelector<HTMLInputElement>("#tHand")!,
    setupBtn: app!.querySelector<HTMLButtonElement>("#tSetup")!,
    onReveal: (payload, handClass) => onSolved(payload, handClass),
    blankStage: () => showStageMessage("Trainer", TRAINER_PROMPT),
  });

  // explore hand-highlight: live as you type; clearing the input restores the default cell
  handHl.addEventListener("input", () => {
    if (!currentGrid) return;
    if (!applyHandHighlight() && handHl.value.replace(/[\s,]+/g, "").length === 0) {
      currentGrid.selectDefault();
    }
  });

  // nothing on stage until the operator solves something; the solve path is cache-first,
  // so any stored board is one Solve away
  showStageMessage(EXPLORE_IDLE_TITLE, EXPLORE_IDLE_BODY);
}

main();
