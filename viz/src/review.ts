// Phase 5: hand-history review mode. Paste a history (PokerStars text or the JSON contract),
// see coverage (what is gradeable vs skipped and WHY - the skip breakdown is the roadmap),
// drive any needed solves through the existing solve path, then render the aggregate
// frequency-leak report.
//
// Frequency, not sequence: this surface reports rate gaps and EV cost over many instances.
// It never shows per-instance verdicts and never emits a next-move recommendation. The GTO%
// column is range-conditioned (D44): the solver's mix for the hands actually held, NOT the
// node's unconditional frequency - the caveat ships with the table.

import { requestSolve, SERVER } from "./solve";

interface FreshSpot {
  key: string;
  board: string[];
  depth: number;
}

interface IngestResponse {
  hands: number;
  parse_errors: number;
  coverage: Record<string, number>;
  gradeable: { decision: unknown; key: string; status: string }[];
  fresh_spots: FreshSpot[];
  fresh_deferred: number;
  max_fresh_solves_per_run: number;
  coverage_note: string;
}

interface NodeReport {
  depth_bb: number;
  node_shape: string;
  texture: string;
  n: number;
  actions: string[];
  taken_counts: number[];
  r: number[];
  g: number[];
  delta: number[];
  z: number[];
  ev_cost_mix_bb: number;
  displayed_cost_bb: number;
  ev_cost_best_bb: number;
  ranked: boolean;
  boards: { board: string; n: number }[];
}

interface ReviewReport {
  total_records: number;
  distinct_hands: number;
  nodes: NodeReport[];
  low_sample_nodes: number;
  total_cost_mix_bb: number;
  total_cost_best_bb: number;
  comparator_caveat: string;
}

export function initReview(host: HTMLElement) {
  host.innerHTML = `
    <div class="rv-input">
      <div class="rv-row">
        <label for="rvFormat">format</label>
        <select id="rvFormat"><option value="pstext">PokerStars text</option><option value="json">JSON</option></select>
        <label for="rvSource">source</label>
        <input id="rvSource" type="text" placeholder="session name" maxlength="40" />
        <button id="rvAnalyze" type="button">Analyze</button>
        <span class="t-err" id="rvErr"></span>
      </div>
      <textarea id="rvText" rows="6" spellcheck="false"
        placeholder="Paste completed hand histories here (post-hoc study only)"></textarea>
    </div>
    <div class="rv-coverage" id="rvCoverage" hidden></div>
    <div class="rv-solve" id="rvSolve" hidden>
      <span class="ss-label" id="rvSolveLabel"></span>
      <div class="ss-track"><div class="ss-fill" id="rvSolveFill"></div></div>
      <span class="ss-value" id="rvSolveVal"></span>
    </div>
    <div class="rv-report" id="rvReport" hidden></div>
  `;

  const formatSel = host.querySelector<HTMLSelectElement>("#rvFormat")!;
  const sourceIn = host.querySelector<HTMLInputElement>("#rvSource")!;
  const textIn = host.querySelector<HTMLTextAreaElement>("#rvText")!;
  const analyzeBtn = host.querySelector<HTMLButtonElement>("#rvAnalyze")!;
  const errEl = host.querySelector<HTMLElement>("#rvErr")!;
  const coverageEl = host.querySelector<HTMLElement>("#rvCoverage")!;
  const solveRow = host.querySelector<HTMLElement>("#rvSolve")!;
  const solveLabel = host.querySelector<HTMLElement>("#rvSolveLabel")!;
  const solveFill = host.querySelector<HTMLElement>("#rvSolveFill")!;
  const solveVal = host.querySelector<HTMLElement>("#rvSolveVal")!;
  const reportEl = host.querySelector<HTMLElement>("#rvReport")!;

  let busy = false;

  const setError = (m: string) => {
    errEl.textContent = m;
  };

  const renderCoverage = (r: IngestResponse) => {
    const rows = Object.entries(r.coverage)
      .sort((a, b) => b[1] - a[1])
      .map(
        ([reason, count]) =>
          `<div class="rv-cov-row"><span class="rv-cov-reason">${reason}</span><span class="rv-cov-count">${count}</span></div>`,
      )
      .join("");
    const gradeableN = r.gradeable.length;
    coverageEl.innerHTML = `
      <div class="rv-head">coverage · ${r.hands} hands · ${gradeableN} gradeable decision${gradeableN === 1 ? "" : "s"}</div>
      <p class="rv-note">${r.coverage_note}</p>
      <div class="rv-cov">${rows}</div>
      ${r.fresh_deferred > 0 ? `<p class="rv-note">${r.fresh_deferred} additional fresh spots deferred by the per-run cap (${r.max_fresh_solves_per_run}); re-run after this pass to continue.</p>` : ""}
    `;
    coverageEl.hidden = false;
  };

  const renderReport = (rep: ReviewReport) => {
    const fmtPct = (x: number) => `${(x * 100).toFixed(0)}%`;
    const nodeHtml = (nd: NodeReport) => {
      const rows = nd.actions
        .map(
          (a, i) => `
          <div class="rv-act-row">
            <span class="rv-act">${a}</span>
            <span>you ${fmtPct(nd.r[i])} (${nd.taken_counts[i]})</span>
            <span>gto ${fmtPct(nd.g[i])}</span>
            <span class="${Math.abs(nd.z[i]) > 2 ? "rv-sig" : ""}">d ${(nd.delta[i] * 100).toFixed(0)} · z ${Number.isFinite(nd.z[i]) ? nd.z[i].toFixed(1) : "inf"}</span>
          </div>`,
        )
        .join("");
      const cost =
        nd.ev_cost_mix_bb < 0
          ? "no measurable cost (within solve noise)"
          : `cost ${nd.displayed_cost_bb.toFixed(2)} bb · max exploitable ${nd.ev_cost_best_bb.toFixed(2)} bb`;
      return `
        <div class="rv-node ${nd.ranked ? "rv-node--ranked" : ""}">
          <div class="rv-node-head">${nd.depth_bb}bb · ${nd.node_shape} · ${nd.texture} · N=${nd.n}
            <span class="rv-boards">${nd.boards.length} board${nd.boards.length === 1 ? "" : "s"}</span></div>
          ${rows}
          <div class="rv-cost">${cost}</div>
        </div>`;
    };

    const ranked = rep.nodes.filter((n) => n.ranked);
    const lowSample = rep.nodes.filter((n) => !n.ranked);
    reportEl.innerHTML = `
      <div class="rv-head">frequency report · ${rep.total_records} graded decisions over ${rep.distinct_hands} hands</div>
      <p class="rv-caveat">${rep.comparator_caveat}</p>
      ${ranked.length ? ranked.map(nodeHtml).join("") : `<p class="rv-note">No statistically supported rate gaps yet. Keep ingesting sessions; leaks need sample size before they are evidence.</p>`}
      ${lowSample.length ? `<div class="rv-low-head">low sample - not evidence (need N >= 10 and |z| > 2)</div>${lowSample.map(nodeHtml).join("")}` : ""}
    `;
    reportEl.hidden = false;
  };

  const fetchReport = async () => {
    const res = await fetch(`${SERVER}/api/review/report`);
    const body = await res.json();
    if (!res.ok) {
      setError(body.error ?? `report failed: HTTP ${res.status}`);
      return;
    }
    if (body.empty) {
      reportEl.innerHTML = `<p class="rv-note">${body.message}</p>`;
      reportEl.hidden = false;
      return;
    }
    renderReport(body.report as ReviewReport);
  };

  const solveFresh = (spots: FreshSpot[]) =>
    new Promise<void>((resolve) => {
      if (spots.length === 0) {
        resolve();
        return;
      }
      solveRow.hidden = false;
      let i = 0;
      const next = () => {
        if (i >= spots.length) {
          solveRow.hidden = true;
          resolve();
          return;
        }
        const spot = spots[i];
        solveLabel.textContent = `solving ${spot.board.join("")} (${spot.depth}bb) · ${i + 1}/${spots.length}`;
        solveFill.style.width = "0%";
        let baseline: number | null = null;
        const TARGET = 0.005;
        void requestSolve(
          spot.board,
          {
            onProgress(p) {
              const e = p.exploitability_pct_pot;
              if (baseline === null) baseline = Math.max(e, TARGET * 1.0001);
              const num = Math.log(baseline) - Math.log(Math.max(e, TARGET));
              const den = Math.log(baseline) - Math.log(TARGET);
              solveFill.style.width = `${(Math.min(1, Math.max(0, num / den)) * 100).toFixed(1)}%`;
              solveVal.textContent = `${(e * 100).toFixed(2)}% pot · iter ${p.iteration}`;
            },
            onComplete() {
              i += 1;
              next();
            },
            onError(message) {
              // "solve already in progress" and friends: brief backoff, retry this spot
              solveVal.textContent = `${message}; retrying in 3s`;
              setTimeout(next, 3000);
            },
          },
          spot.depth,
        );
      };
      next();
    });

  const analyze = async () => {
    if (busy) return;
    const content = textIn.value.trim();
    if (!content) {
      setError("paste a hand history first");
      return;
    }
    setError("");
    busy = true;
    analyzeBtn.disabled = true;
    coverageEl.hidden = true;
    reportEl.hidden = true;

    try {
      const res = await fetch(`${SERVER}/api/review/ingest`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          format: formatSel.value,
          content,
          source: sourceIn.value || "pasted",
        }),
      });
      const body = await res.json();
      if (!res.ok) {
        setError(body.error ?? `ingest failed: HTTP ${res.status}`);
        return;
      }
      const ingest = body as IngestResponse;
      renderCoverage(ingest);

      // pass 2: fresh solves (sequential, capped server-side), then grade, then report
      await solveFresh(ingest.fresh_spots);

      if (ingest.gradeable.length > 0) {
        const gradeRes = await fetch(`${SERVER}/api/review/grade`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            decisions: ingest.gradeable.map((g) => g.decision),
            source_file: sourceIn.value || "pasted",
          }),
        });
        const gradeBody = await gradeRes.json();
        if (!gradeRes.ok) {
          setError(gradeBody.error ?? `grading failed: HTTP ${gradeRes.status}`);
          return;
        }
        if ((gradeBody.pending?.length ?? 0) > 0) {
          coverageEl.innerHTML += `<p class="rv-note">${gradeBody.pending.length} decisions pending solve (re-run to continue).</p>`;
        }
      }
      await fetchReport();
    } catch {
      setError("solve server unreachable; start it with: cargo run --release -p gto-server");
    } finally {
      busy = false;
      analyzeBtn.disabled = false;
    }
  };

  analyzeBtn.addEventListener("click", () => void analyze());
}
