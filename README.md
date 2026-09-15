# GTO Poker Trainer

An offline game-theory-optimal (GTO) study and training tool for heads-up No-Limit Hold'em,
written in Rust. It solves poker spots to near-unexploitability, renders the resulting strategy,
quizzes you on it, and analyses your own hand histories for frequency leaks.

The desktop application is branded **GTO Lab**, which is the name you will see in screenshots.

## What this is, and what it is not

This is a **study tool, used away from the table**. That distinction is the whole design, so it
is worth stating plainly before anything else:

- **It is not real-time assistance during live play.** It is for reviewing spots before and after
  a session, not during one.
- **It is not connected to any poker site.** There is no scraper, no screen reader, no overlay,
  no HUD, no site integration of any kind. All solving is local: every API call the interface
  makes goes to a server it spawns itself on `127.0.0.1:7474`. The only traffic that leaves the
  machine is for web fonts, namely a stylesheet from `fonts.googleapis.com` and the font files it
  references from `fonts.gstatic.com`. There is no analytics, no telemetry and no update check.
  Nothing poker-related leaves the machine, and the interface falls back to system fonts offline.
- **Hand-history analysis is post-hoc only.** You paste in hands you have already played. The
  reviewer reports *aggregate frequency* leaks by node ("you fold too often here across 40
  hands"), and by deliberate design no surface in it can emit a next-move recommendation. GTO is
  a frequency, not a sequence.
- **It is a personal project**, built to understand solvers by writing one, not a product.

If you are looking for something to run while you play, this is not that, and it is not built in
a way that could become that.

## What it does

Four pieces, built in phases, each gated before the next began:

- **Solver and strategy viz.** Solve a flop spot with Discounted CFR, then explore the result as
  a 13x13 hand grid with per-action frequencies and EVs, a table view, and per-combo detail.
- **Trainer.** Commit-then-reveal drilling. You are shown a spot, you commit to an action, and
  only then is the solver's answer revealed and your choice graded against it (Correct / FineMix
  / MinorLeak / ClearLeak, with EV loss in % of pot).
- **Hand-history review.** Ingest PokerStars hand-history text (or a documented JSON contract) and
  get aggregate, range-conditioned frequency leaks by node, with statistical significance.
- **Game-tree navigator.** Walk a cached solve down the action tree, node by node, with a
  confidence indicator on every node.

## How it works

**Discounted CFR to a hard convergence target.** Counterfactual regret minimisation is run on a
single spot until exploitability falls to **0.05% of the starting pot**. This is not machine
learning and there is no training corpus or neural network; it is iterative self-play on an
explicit game tree, and the convergence target is a measurable property of the output, not a
hyperparameter you hope was enough.

**A "spot" is a subgame.** The solver works from a chosen street onwards, so preflop history is
not replayed. It is encoded in three inputs: both players' ranges, the starting pot, and the
effective stack. Those come from the authored preflop provider per pot type, never hardcoded at
call sites.

**Precompute and cache.** One solved flop tree is hundreds of megabytes, so solving everything
ahead of time is infeasible. Instead a curated set of spots is solved once into a `matrix/`
directory (tree file, JSON summary sidecar, manifest index), and lookups read from it with zero
solving. Anything not in the cache can be solved on demand and is then cached alongside. The
curated stored set **is** the validation benchmark set: one list, not two, chosen as much for
"can I verify this is right" as for "will I hit it often".

**Seven crates** under `crates/`: `config` (every tunable, single-sourced), `engine` (spot model,
solve, node queries), `preflop` (authored ranges and geometry), `validate` (invariant gate plus an
independent equity cross-check), `store` (serialisation and the viz contract), `server` (the
localhost API), `history` (hand-history parsing and leak aggregation). The frontend is TypeScript
and Vite; the desktop shell is Tauri.

## Build and run

**Read this section before cloning.** A fresh clone compiles, but it will not run until you build
the solve cache yourself. This is by design, not an oversight.

Requirements: **Rust stable** (pinned by `rust-toolchain.toml`; the workspace declares a 1.75
minimum), **Node 18, 20 or 22+** (Vite 6 requires `^18 || ^20 || >=22`), and **Windows** with
PowerShell plus the WebView2 runtime the Tauri shell renders in. The desktop shell and the build
script are Windows-specific; the engine and server crates are not.

```powershell
git clone https://github.com/jlu1023/gto-poker-trainer.git
cd gto-poker-trainer

# 1. Build the solve cache. THIS IS THE LONG STEP.
cargo run --release -p gto-store --example build_matrix

# 2. Build the desktop app (viz bundle, server, sidecar provisioning, app).
.\desktop\build-desktop.ps1

# 3. Run it.
.\desktop\src-tauri\target\release\gto-lab-desktop.exe
```

### Why step 1 is unavoidable

The `matrix/` cache is **not** in this repository and never will be. It is roughly **3.4 GB** for
the 19-spot curated benchmark, which is far past what a git repository should carry. Building it
solves 19 spots at a measured **66 to 249 seconds each**, so budget **hours**, not minutes.

The application deliberately **refuses to start without it** rather than silently cold-starting an
empty cache and re-solving the whole benchmark behind your back. If it cannot find
`phase1.toml` and `matrix/manifest.json`, it shows an error dialog naming the path it looked in
and exits. That is working as intended.

You can point it at a cache elsewhere with the `GTO_LAB_ROOT` environment variable. Otherwise the
app finds the repository root by walking up from its own location.

### Two build foot-guns

- **Use `build-desktop.ps1`.** A bare `cargo build --release` inside `desktop/src-tauri` produces a
  *dev-mode* binary whose webview points at `http://localhost:5173` and shows
  `ERR_CONNECTION_REFUSED` when you double-click it. The `--features custom-protocol` flag that
  the script passes is what embeds the frontend bundle, and it is the only thing distinguishing a
  real artefact from a dev build.
- **The desktop shell is outside the Cargo workspace** (it has its own dependency tree and
  lockfile, so the pinned-solver engine workspace stays byte-identical for the gates). Building
  the workspace does not build the app, and vice versa.

You can also run the pieces separately: `cargo run --release -p gto-server` from the repository
root, then `npm run dev` inside `viz/`.

## Honest limitations

These are real and worth knowing before you trust a number this tool shows you.

**Convergence is global, not local.** The 0.05%-of-pot target is measured at the root. A subtree
that is reached rarely gets far less training than the root does, so its local numbers can be
meaningfully wrong even though the solve as a whole converged. This does not shrink by converging
the root harder. The navigator handles it rather than hiding it: a node is only descendable when
its own worst unplayed-action dominance is within **5% of the pot**, and **every displayed node
carries its own dominance as a visible confidence indicator** (high within 0.5%, fair within 2%,
marginal beyond that up to the gate). The 5% gate is a deliberate trade: a stricter 1% would be
purer, but it gates off exactly the bet, raise and check-raise exploration on dry, monotone and
broadway boards that the navigator exists to show. The looser gate is only defensible because a
marginal node cannot look as authoritative as a root-quality one.

**The navigator is slow.** Roughly **13 to 18 seconds per node step**. The server decodes the
whole tree from disk on every request (about 7 seconds, the dominant fixed cost, nothing cached
between calls) and then reads each child again to compute its dominance. For a click-to-descend
interface that is poor. The fix (caching decoded games in memory, keyed by spot) has real memory
implications against the 8 GB solve ceiling, so it is deliberately left as a measured piece of
work rather than a quick patch. This is a known open item, not a mystery.

**The scope is narrow.**
- Heads-up only. Specifically big blind out of position against button in position.
- **Flop-rooted spots only.** The solver plays them through to the river, but you cannot start
  from a turn or river node.
- Single-raised, 3-bet and 4-bet pots, at 100bb and 50bb (4-bet pots at 100bb only).
- No turn or river raise sizes in any bet tree. Flop raises are a single size.
- **Preflop ranges are theory-authored approximations.** See below.

**No commercial solver was used as an oracle.** Validation is built on internal invariants
(exploitability against target, strategy normalisation across the full tree, per-action EV
consistency, root zero-sum, best-response recheck) plus an independent equity cross-check. Those
catch a great deal, but they cannot tell you that the authored preflop ranges match what a
commercial solver would produce.

## Provenance and licensing

**The preflop ranges are ours.** They are authored in-repo from established heads-up GTO theory,
not lifted from any licensed product. That was a deliberate choice after no clean, freely
importable heads-up range set could be confirmed, and after the leading candidate was refuted
during verification (its range data turned out not to exist). Documented as D18.

**A cross-check tool was dropped on licence grounds.** opensolver was evaluated and rejected: no
licence file, which defaults to all rights reserved. Documented as D19. The project is therefore
licence-clean throughout, which for a poker tool is worth stating explicitly.

**Licence: AGPL-3.0.** See [LICENSE](LICENSE). The solver engine is
[postflop-solver](https://github.com/b-inary/postflop-solver) by b-inary, which is AGPL-3.0 and
git-pinned here to commit `9d1509fe` (upstream development was suspended in October 2023 and made
breaking changes without version bumps). Because this project links that engine, AGPL-3.0 is the
correct and required licence for the whole of it, including the network-use provision if you ever
host the viz.

## Screenshots

<!-- TODO: add screenshots here. Suggested: the 13x13 strategy grid, the trainer's
     reveal-and-grade view, and a navigator node showing its confidence indicator. -->

## Further reading

[DECISIONS.md](DECISIONS.md) is the detailed engineering record: decisions numbered through D55,
with the evidence behind them, including the ones that were refuted and reversed. The `PLAN*.md` files are
the phase plans each stage was gated against. Between them they are a more honest account of how
this was built than this README can be.
