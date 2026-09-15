# PLAN-phase5.md - GTO Poker Bot Phase 5 (hand-history leak analysis)

Status: APPROVED with operator additions D44/D45 + gate addition (2026-06-04). Builds on
Phase 4 (commit cd31e5b). No em dashes. Authoritative master plan: the author's private project notes.

## Goal

Ingest COMPLETED hand histories (post-hoc study only, never real-time), grade every decision
inside the supported envelope using the existing grader/solve/cache, and report AGGREGATE
frequency leaks by node: "you bet this node 40% over 50 occurrences; GTO bets 67% for the
hands you held; cost ~X bb". Out-of-envelope decisions are skipped with precise reasons; the
coverage breakdown doubles as the build-next roadmap.

## Core principle (shapes everything)

GTO is a frequency, not a sequence. A leak is an aggregate rate deviation across many
instances of a node, never a per-instance verdict and never a "do X next time". The mixed
strategy is unpredictable by design. Report enforcement: fixed sentence templates
(frequency-framed), no per-instance verdicts in the leak report, ranking by EV cost only
(never by prescription), and a gate tripwire that greps the built bundle for prescriptive
phrasing. NOTE (operator addition): the tripwire validates templates, not framing safety;
the framing is safe because of the per-instance-rate model itself - a passing grep must not
be read as proof of safe framing.

## Operator decisions (settled)

1. Input = minimal JSON schema (engine contract) + PokerStars-text adapter in this phase.
2. Envelope = BB-root AND IP-after-check nodes (check-only prefixes).
3. Aggregation = ~10 texture buckets (A-high and paired splits) x depth x node shape, with
   exact-board drilldown.
4. Persistence = accumulating gitignored JSONL store, idempotent re-ingestion.

## 1. Input: `crates/history` (gto-history), JSON contract + PS adapter

- `schema.rs`: HandRecord {hand_id, source, table_size_dealt, hero_seat (BTN|BB), stacks_bb,
  blinds_bb, ante_bb, hero_cards, preflop: [ActionEvent], flop: Option<{cards, actions}>,
  turn/river: Option (classification only)}. ActionEvent {seat, kind (Fold|Check|Call|Bet|
  Raise|Post|AllInMarker), amount_bb}. Hero hole cards REQUIRED to grade (the grader needs
  the exact combo); absent -> skip `hero-cards-unknown`.
- `pstext.rs`: PokerStars text -> Vec<HandRecord>. Quarantined landmines: posts/dead blinds
  (-> `non-standard-blinds`), "and is all-in" markers, "Uncalled bet returned" (not an
  action), currency/locale headers, sit-outs/timeouts, hero-only "Dealt to", rake/summary
  lines, dealt-count vs table-max (true HU = exactly 2 DEALT).
- Unparseable or partial hands -> skip `unparseable-hand` with a per-hand error string.

## 2. Envelope classifier (decision-level; the crux)

IN-ENVELOPE iff ALL:
- Exactly 2 players dealt; standard blinds, no antes (else `non-standard-blinds`/`ante-pot`).
- SRP: preflop exactly one raise (BTN open) then BB call (limp -> `limped-pot`; re-raise ->
  `3-bet-pot`).
- Geometry guards (tight; a graded instance must approximate the solved 5bb-pot geometry to
  sub-noise, disclosure does not fix a wrong-SPR mix): open in [2.3, 2.7]bb, effective stack
  within +-7% of a depth bucket {50, 100} (else `open-size-out-of-band`/`depth-out-of-band`).
- FLOP decision (else `preflop-decision`/`turn-decision`/`river-decision`).
- Flop action prefix before hero's decision is ONLY checks. Two gradeable node shapes:
  (a) hero=BB first to act (root, path []); (b) hero=BTN after BB check (path [Check]).
  Hero facing any bet, either seat (incl. check-raise spots) -> `facing-bet-on-flop`.
  NO action translation, ever.
- Hero's own action maps onto the tree: Check exact; a bet within +-8% of pot of a tree size
  (33%/75%) maps silently (client rounding); else `off-tree-hero-size`. NEVER nearest-map
  into the aggregate; never split-count.
- Full tree only: instances served by a `-shrunk` solve -> `solve-shrunk-size-unavailable`
  (their action set lacks Bet(75); folding them in corrupts the size split).
Other reasons/states: `hero-folded-preflop`, `no-postflop-decision` (incl. all-in preflop),
`pending-solve` (gradeable, not yet solved under the per-run cap - a distinct coverage STATE,
not a skip; reporting it as ungradeable would make the roadmap lie).

Expectation-setting (operator addition): the Pass-1 coverage panel states UP FRONT that a
real history will likely show a large majority of decisions skipped (wrong depth, open size,
pot type, street) - this is the honest envelope, not a failure, and the per-reason counts are
the roadmap for what to add next.

## 3. Engine extension: `read_node` (small, F5-guarded)

`gto_engine::read_node(game, &NodeQuery) -> NodeStrategy` walks the semantic path (reuses
query_node's walker) and fills BOTH `ev` (mix) and `ev_per_action` via
`expected_values_detail(player)` AT the destination node (hero is current player there; one
shared money-reference offset per action row). Unit test: read_node([]) == read_root.
GATE ADDITION (operator): F5 was found and fixed AT THE ROOT; the [Check] node has its own
money reference, so the gate verifies AT THAT NODE: for a near-pure IP hand on a known spot,
the per-action EV spread among played actions is tiny - the same sanity check that caught F5.
`/api/grade` gains optional `node_path` (default [] = root).

## 4. Aggregation model (the heart)

- Node key = (depth bucket, node shape, texture bucket). Texture taxonomy (~10): paired-high,
  paired-mid/low, monotone, two-tone-A-high, two-tone-broadway-no-A, two-tone-mid/low,
  rainbow-A-high, rainbow-broadway-no-A, rainbow-mid-connected, rainbow-low. Exact-board
  drilldown beneath each bucket.
- Per instance i: solver mix p_i(a) for hero's EXACT combo on that EXACT board (from the
  cached solve via read_node), hero's taken tree-action a_i.
- Operator rate r(a) = count(a_i = a)/N. GTO comparator g(a) = mean_i p_i(a).
- **D44 (operator addition) - the comparator is RANGE-CONDITIONED, a stated scope boundary:**
  g(a) is the solver's mix conditioned on the hands the operator ACTUALLY HELD at this node,
  NOT the node's unconditional GTO frequency. This report measures action-given-range leaks
  only; it CANNOT detect range-construction leaks. If the operator's arriving range is skewed
  (e.g. over-folding earlier makes the continuing range too strong), g(a) moves with that
  selection bias and the operator can look LESS leaky than they are. Report templates must
  say "for the hands you held"; the review UI carries a one-line caveat near the leak table;
  g(a) must never be readable as "the GTO frequency for this node".
- Significance: Poisson-binomial under the null "you played GTO on these exact spots":
  mu = sum_i p_i(a), sigma^2 = sum_i p_i(a)(1-p_i(a)), z = (count - mu)/sigma. Ranked leak
  iff N >= 10 AND |z| > 2. Below threshold -> "low sample - not evidence" section, N visible,
  no leak language. (Plain binomial SE is the wrong null for heterogeneous p_i and mishandles
  near-pure nodes.)
- EV cost: primary = sum_i (node.ev[h_i] - node.ev_per_action[h_i + a_i*n]) - mix-EV loss,
  computed in the aggregator from the node read (node.ev IS dot(p_i, ev_actions_i)). NOT
  sourced from grade() (clamped vs-best). Secondary, smaller: sum of vs-best losses =
  "max exploitable cost". Units: bb and bb/100 instances.
- **D45 (operator addition) - per-instance EV sanity assertion BEFORE clipping (F5 regression
  guard through read_node):** for any instance where the solver plays a_i at meaningful
  frequency (p_i(a_i) >= mix floor), assert node.ev[h_i] >= node.ev_per_action[h_i + a_i*n]
  - eps (eps = the solve target, 0.5% pot). You cannot beat the equilibrium mix by taking an
  action the solver also mixes; a violation means an F5-style reference error re-entered via
  read_node, and ingestion FAILS LOUDLY for that spot. The display-time negative clip on node
  totals stays, but is NOT the only guard - clipping alone would mask exactly this
  regression. Companion direction (audit follow-up priority 1): the same identity goes into
  check_invariants so fresh solves inherit the guard.
- Ingest assertion: all range weights == 1.0, fail loudly (the known fractional-weight
  tripwire stays guarded, not silently leaned on).

## 5. Ingestion flow (two-pass) + solve budget

Pass 1 (instant): parse + classify -> full coverage report (counts by reason/state, with the
high-skip expectation note) + distinct needed spots split cache-hit vs fresh.
Pass 2: drive fresh solves SEQUENTIALLY through the existing `/api/solve` (SSE progress,
single-solve semaphore; resilient to "solve already in progress" via retry/backoff, never
aborts the run). Per-run fresh-solve cap (config `[review] max_fresh_solves_per_run`, default
25); capped spots -> `pending-solve`; re-runs are incremental via the cache. Strict order:
solve-all -> re-read manifest -> grade-all (grade 409s if unsolved). Worst case ~40-60
distinct boards per 100 HU hands; the cap and cache bound it. ram_ceiling_gb + shrink
fallback inherited (shrunk -> excluded state, above).

## 6. Persistence (JSONL store)

`matrix/history/graded.jsonl` (gitignored with the matrix): one record per graded decision
{schema_version, hand_id, street, decision_index, node-key fields, board, depth, combo,
action_taken, p_i vector, ev_mix, ev_actions, source_file, ingested_at}. Dedupe key
(hand_id, street, decision_index); re-ingesting a file is idempotent (existing keys loaded
into a set at ingest). Report computed over the accumulated store; per-ingestion filter.

## 7. Report surface (viz "review" mode)

New mode tab alongside explore/train: paste/upload (JSON or PS text) -> Pass-1 coverage panel
-> Pass-2 progress (existing convergence strip per spot) -> ranked leak table (node, N,
per-action your% vs GTO% with delta and z, EV cost primary + max-exploitable secondary) +
low-sample section + coverage summary + the D44 caveat line near the leak table. Fixed
templates: "Over N instances at <node>, for the hands you held, you <action> X% vs GTO Y%;
cost ~Z bb". No per-instance verdicts in the leak report. Ranking by EV cost only; no
template sorts or ranks by "what to change". Existing modes untouched.

## 8. DECISIONS.md additions

D38 input contract + PS adapter; D39 envelope criteria + skip taxonomy (as section 2);
D40 aggregation (per-instance-mean comparator + Poisson-binomial + mix-EV primary from
node.ev); D41 two-pass ingestion + pending-solve state; D42 JSONL persistence; D43 report
framing enforcement (+ tripwire caveat); D44 range-conditioned comparator boundary (operator);
D45 per-instance EV sanity assertion (operator).

## 9. Gate (5b)

- cargo check + workspace tests (parser/classifier/aggregator units incl. one fixture per
  skip reason + a hand-computed Poisson-binomial case) + npm build clean.
- **read_node F5 check at the [Check] node** (operator addition): near-pure IP hand on a
  known spot shows tiny played-action EV spread at that node; read_node([]) == read_root.
- Sample multi-hand history: in-envelope graded; every skip reason exercised and counted;
  coverage reconciles with input hand count exactly.
- Reuse proof: one decision's review-path grade == direct trainer /api/grade of the same
  spot+combo+node.
- Aggregation correctness: hand-computed fixture reproduces r, g, z, EV cost exactly; the
  min-n and z guards suppress a 2-instance "leak"; D45 assertion fires on a synthetically
  corrupted ev_per_action (negative test).
- Language tripwire passes (with its caveat recorded); D44 caveat present in the UI; existing
  Phase 2/3/4 verifiers still green.
- Out-of-envelope and unparseable inputs skip/error cleanly, never fabricate a grade.

## 10. Vault (on completion)

Phase 5 - findings.md (input contract, envelope + taxonomy, aggregation model incl. D44
boundary + D45 guard, persistence, observed coverage; links [[GTO Poker Bot]] and
[[Phase 4 - findings]] both ways). Master plan -> phase-5 + pointer. [[Phase 4 - audit
follow-ups]]: note the weight==1.0 ingest assertion + D45 as partial delivery of priority 1.
Clod.md row updated.

## 11. Out of scope

Turn/river grading, 3-bet/limped pots, action translation, multiway, randomized spots,
spaced repetition, cache eviction, site formats beyond PS text, per-instance verdicts in the
leak report, range-construction leak detection (explicitly out per D44).
