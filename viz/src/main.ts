import "./style.css";
import type { IndexFile, SolveComplete, SpotContract } from "./types";
import type { ClassAgg } from "./grid";
import { renderTable } from "./table";
import { renderGrid, actionColors } from "./grid";
import { initSolvePanel } from "./solve";
import { initTrainer } from "./trainer";
import { initReview } from "./review";

const app = document.querySelector<HTMLDivElement>("#app");
if (!app) throw new Error("missing #app");

async function fetchJson<T>(file: string): Promise<T> {
  const res = await fetch(`data/${file}`);
  if (!res.ok) throw new Error(`${file}: ${res.status}`);
  return (await res.json()) as T;
}

function humanAction(label: string): string {
  const m = label.match(/^([A-Za-z]+)\((\d+)\)$/);
  if (!m) return label;
  if (m[1] === "AllIn") return "All-in";
  return `${m[1]} ${m[2]}`;
}

app.innerHTML = `
  <header class="topbar">
    <div class="brand"><span class="brand-mark">◈</span>GTO&nbsp;<em>Lab</em></div>
    <div class="mode-toggle" role="tablist" aria-label="mode">
      <button id="modeExplore" class="mode-btn mode-btn--active" type="button">explore</button>
      <button id="modeTrain" class="mode-btn" type="button">train</button>
      <button id="modeReview" class="mode-btn" type="button">review</button>
    </div>
    <div class="selector-wrap">
      <label for="spot">spot</label>
      <select id="spot" aria-label="stored spot"></select>
    </div>
    <div class="solve-wrap">
      <label for="flop">solve · 50bb srp</label>
      <input id="flop" type="text" placeholder="Ks7h2c" maxlength="11"
        spellcheck="false" autocomplete="off" aria-label="flop to solve" />
      <button id="solveBtn" type="button">Solve</button>
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

const selector = app.querySelector<HTMLSelectElement>("#spot")!;
const meta = app.querySelector<HTMLDivElement>("#meta")!;
const tableHost = app.querySelector<HTMLElement>("#tableHost")!;
const gridHost = app.querySelector<HTMLElement>("#gridHost")!;
const detailHost = app.querySelector<HTMLElement>("#detail")!;
const legendHost = app.querySelector<HTMLElement>("#legend")!;

// spots solved live this session (REAL solver output from the server, never mocked); lets the
// selector flip back to an on-demand spot without a static data/<key>.json file
const sessionSpots = new Map<string, SpotContract>();

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

/** Render a spot (from a static file, the session cache, or a live solve). When
 *  `selectClassLabel` is given (trainer reveal), that hand class is selected instead of the
 *  default first in-range cell. */
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

async function loadSpot(key: string) {
  const live = sessionSpots.get(key);
  if (live) {
    renderSpot(live);
    return;
  }
  let spot: SpotContract;
  try {
    spot = await fetchJson<SpotContract>(`${key}.json`);
  } catch {
    showStageMessage("No solved data for this spot", `Run the matrix build and exporter to populate it: cargo run --release -p gto-store --example build_matrix, then export_viz.`);
    return;
  }
  renderSpot(spot);
}

/** A live solve (fresh or cached) finished: remember it, list it, render it. */
function onSolved(payload: SolveComplete, selectClassLabel?: string) {
  const spot = payload.spot;
  sessionSpots.set(spot.key, spot);
  if (![...selector.options].some((o) => o.value === spot.key)) {
    const opt = document.createElement("option");
    opt.value = spot.key;
    opt.textContent = `${spot.board.join("")} · ${spot.stack_depth_bb}bb · live`;
    selector.appendChild(opt);
  }
  selector.value = spot.key;
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

  const setMode = (mode: "explore" | "train" | "review") => {
    document.body.classList.toggle("mode-train", mode === "train");
    document.body.classList.toggle("mode-review", mode === "review");
    for (const [m, btn] of Object.entries(buttons)) {
      btn.classList.toggle("mode-btn--active", m === mode);
    }
    trainerHost.hidden = mode !== "train";
    reviewHost.hidden = mode !== "review";
    if (mode === "train") {
      // entering train mode blanks the stage: nothing solver-derived pre-commit (D35)
      showStageMessage(
        "Trainer",
        "Set up a spot (depth, board, your hand) and commit to an action. The solver's answer reveals only after you commit.",
      );
    } else if (mode === "explore" && selector.value) {
      void loadSpot(selector.value);
    }
  };

  buttons.explore.addEventListener("click", () => setMode("explore"));
  buttons.train.addEventListener("click", () => setMode("train"));
  buttons.review.addEventListener("click", () => setMode("review"));
}

async function main() {
  initSolvePanel(
    {
      input: app!.querySelector<HTMLInputElement>("#flop")!,
      button: app!.querySelector<HTMLButtonElement>("#solveBtn")!,
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
    onReveal: (payload, handClass) => onSolved(payload, handClass),
    blankStage: () =>
      showStageMessage(
        "Trainer",
        "Set up a spot (depth, board, your hand) and commit to an action. The solver's answer reveals only after you commit.",
      ),
  });

  let index: IndexFile;
  try {
    index = await fetchJson<IndexFile>("index.json");
  } catch {
    showStageMessage("Matrix not found", "Could not load data/index.json. Build the matrix and run the export_viz exporter first.");
    return;
  }
  if (index.spots.length === 0) {
    showStageMessage("Matrix is empty", "No stored spots in the manifest.");
    return;
  }

  selector.innerHTML = index.spots
    .map((s) => `<option value="${s.key}">${s.board.join("")} · ${s.stack_depth_bb}bb</option>`)
    .join("");
  selector.addEventListener("change", () => loadSpot(selector.value));

  await loadSpot(index.spots[0].key);
}

void main();
