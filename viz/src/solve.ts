// Phase 3: the solve-on-demand panel. A flop input + Submit that POSTs to the local solve
// server, a progress strip driven by the STREAMED exploitability (never a fake spinner), and
// the completion hook that feeds the returned spot into the existing render pipeline.
//
// The server speaks SSE over a POST response. The browser EventSource API cannot POST (and its
// auto-reconnect would re-fire solves), so we parse the text/event-stream off a fetch body.

import type { SolveComplete, SolveProgressMsg } from "./types";

export const SERVER = "http://127.0.0.1:7474";
/// srp-v1 convergence target (fraction of pot); the bar fills as exploitability approaches it.
const TARGET = 0.005;

const RANKS = "AKQJT98765432";
const SUITS = "shdc";

/** Parse operator input like "Ks7h2c" / "ks 7H 2c" into 3 normalized cards, or an error. */
export function parseFlop(raw: string): { cards?: string[]; error?: string } {
  const s = raw.replace(/[\s,]+/g, "");
  if (s.length === 0) return { error: "" }; // empty input: invalid but no message
  if (s.length !== 6) return { error: "need exactly 3 cards, e.g. Ks7h2c" };
  const cards: string[] = [];
  for (let i = 0; i < 6; i += 2) {
    const r = s[i].toUpperCase();
    const su = s[i + 1].toLowerCase();
    if (!RANKS.includes(r)) return { error: `illegal rank '${s[i]}'` };
    if (!SUITS.includes(su)) return { error: `illegal suit '${s[i + 1]}'` };
    const card = r + su;
    if (cards.includes(card)) return { error: `duplicate card '${card}'` };
    cards.push(card);
  }
  return { cards };
}

export interface SolveCallbacks {
  onProgress(p: SolveProgressMsg): void;
  onComplete(payload: SolveComplete): void;
  onError(message: string): void;
}

/** POST the board and dispatch the SSE events. Exactly one terminal callback fires. */
export async function requestSolve(
  board: string[],
  cb: SolveCallbacks,
  depth = 50,
): Promise<void> {
  let done = false;
  const fail = (message: string) => {
    if (!done) {
      done = true;
      cb.onError(message);
    }
  };

  let res: Response;
  try {
    res = await fetch(`${SERVER}/api/solve`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ board, depth }),
    });
  } catch {
    fail("solve server unreachable; start it with: cargo run --release -p gto-server");
    return;
  }
  if (!res.ok || !res.body) {
    fail(`solve server error: HTTP ${res.status}`);
    return;
  }

  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buf = "";

  const dispatch = (evt: string, data: string) => {
    if (done) return; // first terminal event wins; ignore anything after it
    try {
      if (evt === "progress") {
        cb.onProgress(JSON.parse(data) as SolveProgressMsg);
      } else if (evt === "complete") {
        done = true;
        cb.onComplete(JSON.parse(data) as SolveComplete);
      } else if (evt === "error") {
        done = true;
        cb.onError((JSON.parse(data).message as string) ?? "unknown server error");
      }
    } catch {
      fail("malformed event from the solve server");
    }
  };

  try {
    for (;;) {
      const { value, done: eof } = await reader.read();
      if (eof) break;
      buf += decoder.decode(value, { stream: true });
      // SSE frames are separated by a blank line
      let idx: number;
      while ((idx = buf.indexOf("\n\n")) >= 0) {
        const frame = buf.slice(0, idx);
        buf = buf.slice(idx + 2);
        let evt = "message";
        const dataLines: string[] = [];
        for (const line of frame.split("\n")) {
          if (line.startsWith("event:")) evt = line.slice(6).trim();
          else if (line.startsWith("data:")) dataLines.push(line.slice(5).trim());
          // ":" comment lines (keep-alives) and other fields are ignored
        }
        if (dataLines.length > 0) dispatch(evt, dataLines.join("\n"));
      }
    }
  } catch {
    fail("connection to the solve server dropped");
    return;
  }
  if (!done) fail("solve stream closed before completion");
}

export interface SolvePanelRefs {
  input: HTMLInputElement;
  button: HTMLButtonElement;
  status: HTMLElement; // the strip; carries data-state / data-progress-count for verification
  label: HTMLElement;
  track: HTMLElement;
  fill: HTMLElement;
  value: HTMLElement;
}

/** Wire the solve panel. `onSolved` receives every successful completion (fresh or cached). */
export function initSolvePanel(refs: SolvePanelRefs, onSolved: (payload: SolveComplete) => void) {
  const { input, button, status, label, fill, value } = refs;
  let solving = false;

  const setState = (state: string) => {
    status.dataset.state = state;
  };

  const validate = (): string[] | null => {
    const { cards, error } = parseFlop(input.value);
    if (cards) {
      button.disabled = solving;
      if (status.dataset.state === "invalid") setState("idle");
      return cards;
    }
    button.disabled = true;
    if (error) {
      setState("invalid");
      label.textContent = error;
      value.textContent = "";
    } else if (status.dataset.state === "invalid") {
      setState("idle");
    }
    return null;
  };

  const start = () => {
    const cards = validate();
    if (!cards || solving) return;

    solving = true;
    input.disabled = true;
    button.disabled = true;
    status.dataset.progressCount = "0";
    setState("solving");
    label.textContent = `solving ${cards.join("")}`;
    fill.style.width = "0%";
    value.textContent = "waiting for the first convergence sample";

    let baseline: number | null = null; // first reported exploitability, for the log-scale fill

    const finish = () => {
      solving = false;
      input.disabled = false;
      button.disabled = validate() === null;
    };

    void requestSolve(cards, {
      onProgress(p) {
        status.dataset.progressCount = String(
          Number(status.dataset.progressCount ?? "0") + 1,
        );
        const e = p.exploitability_pct_pot;
        if (baseline === null) baseline = Math.max(e, TARGET * 1.0001);
        // log-scale convergence: CFR closes the gap multiplicatively
        const num = Math.log(baseline) - Math.log(Math.max(e, TARGET));
        const den = Math.log(baseline) - Math.log(TARGET);
        const frac = den > 0 ? Math.min(1, Math.max(0, num / den)) : 1;
        fill.style.width = `${(frac * 100).toFixed(1)}%`;
        value.textContent = `${(e * 100).toFixed(2)}% pot · iter ${p.iteration} · target ${(TARGET * 100).toFixed(2)}%`;
      },
      onComplete(payload) {
        finish();
        setState(payload.cached ? "cached" : "done");
        const spot = payload.spot;
        const exploit = (spot.exploitability_pct_pot * 100).toFixed(2);
        label.textContent = payload.cached
          ? `${spot.board.join("")} loaded from cache`
          : `solved ${spot.board.join("")}`;
        value.textContent = payload.cached
          ? `${exploit}% pot · ${spot.iterations} iters · no re-solve`
          : `${exploit}% pot · ${spot.iterations} iters${payload.shrunk ? " · reduced tree (RAM ceiling)" : ""}`;
        if (!payload.cached) fill.style.width = "100%";
        onSolved(payload);
      },
      onError(message) {
        finish();
        setState("error");
        label.textContent = "solve failed";
        value.textContent = message;
      },
    });
  };

  input.addEventListener("input", () => void validate());
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") start();
  });
  button.addEventListener("click", start);
  button.disabled = true;
}
