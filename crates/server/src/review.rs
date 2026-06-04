//! Phase 5 review endpoints: hand-history ingestion (pass 1), batch grading against the
//! cached matrix, and the aggregate leak report.
//!
//! POST /api/review/ingest  - parse + classify only; returns coverage, the gradeable
//!                            decisions annotated cached/fresh, and the fresh-solve list
//!                            (capped). NOTHING is solved or graded here.
//! POST /api/review/grade   - grade gradeable decisions against ALREADY-SOLVED spots (the
//!                            client drives any fresh solves through /api/solve first);
//!                            appends to the JSONL store (idempotent); unsolved -> pending.
//! GET  /api/review/report  - aggregate the accumulated store into the leak report.
//!
//! Frequency, not sequence: grading reads rates and EV; no endpoint emits a recommendation.

use axum::extract::{Json, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use gto_engine::grade::find_hand;
use gto_engine::{hand_range_weight, query_node, NodeQuery, SolverAction, Step};
use gto_history::aggregate::{aggregate, assert_record_sane, texture_bucket, GradedRecord};
use gto_history::envelope::{Classified, GradeableDecision, NodeShape, SkipReason};
use gto_history::schema::HistoryFile;
use gto_history::{classify_history, pstext, store as hstore};
use gto_store::{load_manifest, load_solution};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::solve::{SRP_POT_BB, SRP_RAISE_BB};
use crate::state::AppState;

fn bad_request(message: String) -> Response {
    (StatusCode::BAD_REQUEST, axum::Json(json!({ "error": message }))).into_response()
}

fn store_path(state: &AppState) -> PathBuf {
    Path::new(&state.cfg.io.matrix_dir).join("history").join("graded.jsonl")
}

fn spot_key_for(d: &GradeableDecision, tree_id: &str) -> String {
    format!("{}bb_{}_flop_{}", d.depth_bb, d.board.concat(), tree_id)
}

#[derive(Debug, Deserialize)]
pub struct IngestRequest {
    /// "json" (the HistoryFile contract) or "pstext" (PokerStars text).
    pub format: String,
    pub content: String,
    #[serde(default)]
    pub source: String,
}

pub async fn ingest_handler(
    State(state): State<AppState>,
    Json(req): Json<IngestRequest>,
) -> Response {
    // ---- parse ----
    let (hands, parse_errors): (Vec<_>, Vec<(String, String)>) = match req.format.as_str() {
        "json" => match serde_json::from_str::<HistoryFile>(&req.content) {
            Ok(f) => (f.hands, Vec::new()),
            Err(e) => return bad_request(format!("invalid history JSON: {e}")),
        },
        "pstext" => pstext::parse_file(&req.content, &req.source),
        other => return bad_request(format!("unknown format '{other}' (json | pstext)")),
    };
    if hands.is_empty() && parse_errors.is_empty() {
        return bad_request("no hands found in the input".to_string());
    }

    // ---- classify ----
    let mut entries = classify_history(&hands, &state.cfg.review);
    // unparseable hands from the adapter join coverage as entries
    for (hand_id, detail) in &parse_errors {
        entries.push(Classified::Skipped {
            hand_id: hand_id.clone(),
            decision_index: None,
            reason: SkipReason::UnparseableHand,
            detail: detail.clone(),
        });
    }

    // ---- annotate gradeable decisions against the matrix (cache knowledge lives here) ----
    let manifest = match load_manifest(Path::new(&state.cfg.io.matrix_dir)) {
        Ok(m) => m,
        Err(e) => return bad_request(format!("reading matrix manifest: {e}")),
    };
    let tree_id = &state.cfg.solve.bet_tree.bet_tree_id;

    let mut coverage: BTreeMap<&'static str, u32> = BTreeMap::new();
    let mut gradeable = Vec::new();
    let mut fresh_keys: BTreeMap<String, (Vec<String>, u32)> = BTreeMap::new(); // key -> (board, depth)

    for entry in entries {
        match entry {
            Classified::Skipped { hand_id, decision_index, reason, detail } => {
                *coverage.entry(reason.key()).or_insert(0) += 1;
                // skipped entries are returned compactly in coverage only; details available
                // for debugging via the skipped list
                let _ = (hand_id, decision_index, detail);
            }
            Classified::Gradeable(d) => {
                let key = spot_key_for(&d, tree_id);
                let status = if manifest.spots.contains_key(&key) {
                    *coverage.entry("gradeable-cached").or_insert(0) += 1;
                    "cached"
                } else if manifest.spots.contains_key(&format!("{key}-shrunk")) {
                    // shrunk action set lacks Bet(75): excluded from the size-split report
                    *coverage.entry(SkipReason::SolveShrunkSizeUnavailable.key()).or_insert(0) += 1;
                    continue;
                } else {
                    *coverage.entry("gradeable-needs-solve").or_insert(0) += 1;
                    fresh_keys
                        .entry(key.clone())
                        .or_insert_with(|| (d.board.to_vec(), d.depth_bb));
                    "fresh"
                };
                gradeable.push(json!({ "decision": d, "key": key, "status": status }));
            }
        }
    }

    // per-run fresh-solve cap: spots beyond the cap stay pending this run
    let cap = state.cfg.review.max_fresh_solves_per_run as usize;
    let fresh: Vec<_> = fresh_keys
        .iter()
        .map(|(k, (board, depth))| json!({ "key": k, "board": board, "depth": depth }))
        .collect();
    let (solve_now, deferred) = if fresh.len() > cap {
        (fresh[..cap].to_vec(), fresh.len() - cap)
    } else {
        (fresh.clone(), 0)
    };

    axum::Json(json!({
        "hands": hands.len(),
        "parse_errors": parse_errors.len(),
        "coverage": coverage,
        "gradeable": gradeable,
        "fresh_spots": solve_now,
        "fresh_deferred": deferred,
        "max_fresh_solves_per_run": cap,
        // expectation-setting, surfaced verbatim by the UI: low coverage is the honest
        // envelope, and the skip breakdown is the roadmap
        "coverage_note": "A real history will usually show MOST decisions skipped: the \
            gradeable envelope is HU single-raised pots at 50/100bb (opens 2.3-2.7bb, stacks \
            within 7%), flop decisions reached through checks only. That is honesty, not \
            failure; the per-reason counts are the roadmap for what to build next.",
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct GradeBatchRequest {
    pub decisions: Vec<GradeableDecision>,
    #[serde(default)]
    pub source_file: String,
}

pub async fn grade_batch_handler(
    State(state): State<AppState>,
    Json(req): Json<GradeBatchRequest>,
) -> Response {
    let cfg = state.cfg.clone();
    let source_file = req.source_file.clone();
    let decisions = req.decisions;

    // grading loads solved trees (blocking IO + root recompute): off the async runtime
    let result = tokio::task::spawn_blocking(move || {
        grade_batch(&cfg, decisions, &source_file)
    })
    .await;

    match result {
        Ok(Ok(summary)) => axum::Json(summary).into_response(),
        Ok(Err(e)) => bad_request(format!("grading failed: {e}")),
        Err(e) => bad_request(format!("grade task panicked: {e}")),
    }
}

fn grade_batch(
    cfg: &gto_config::Config,
    decisions: Vec<GradeableDecision>,
    source_file: &str,
) -> anyhow::Result<serde_json::Value> {
    let manifest = load_manifest(Path::new(&cfg.io.matrix_dir))?;
    let tree_id = &cfg.solve.bet_tree.bet_tree_id;
    let pot_chips = (SRP_POT_BB * cfg.solve.chip_per_bb as f64) as f32;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default();

    // group by spot key so each tree loads exactly once
    let mut by_key: BTreeMap<String, Vec<GradeableDecision>> = BTreeMap::new();
    for d in decisions {
        by_key.entry(spot_key_for(&d, tree_id)).or_default().push(d);
    }

    let mut records: Vec<GradedRecord> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    let mut errors: Vec<serde_json::Value> = Vec::new();

    for (key, ds) in by_key {
        if !manifest.spots.contains_key(&key) {
            // not solved yet (pending-solve coverage state); never fabricate
            pending.extend(ds.iter().map(|d| format!("{}#{}", d.hand_id, d.decision_index)));
            continue;
        }
        let mut solution = load_solution(&key, cfg)?;

        // node reads per shape present in this spot's batch (root is already recomputed by
        // load_solution; the [Check] node goes through the same read_node primitive)
        let mut nodes: BTreeMap<&str, gto_engine::NodeStrategy> = BTreeMap::new();
        for d in &ds {
            let shape_key = d.node_shape.key();
            if nodes.contains_key(shape_key) {
                continue;
            }
            let node = match d.node_shape {
                NodeShape::BbRoot => solution.root.clone(),
                NodeShape::IpAfterCheck => query_node(
                    &mut solution,
                    &NodeQuery { path: vec![Step::Action(SolverAction::Check)] },
                )?,
            };
            nodes.insert(shape_key, node);
        }

        for d in ds {
            let node = &nodes[d.node_shape.key()];
            let n = node.hands.len();
            let n_act = node.actions.len();

            // the known fractional-weight tripwire stays guarded: this phase's math assumes
            // weight-1.0 ranges, so assert it per instance
            let hero_range = match d.node_shape {
                NodeShape::BbRoot => &solution.spot.oop_range,
                NodeShape::IpAfterCheck => &solution.spot.ip_range,
            };
            match hand_range_weight(hero_range, &d.combo) {
                Ok(w) if (w - 1.0).abs() < 1e-6 => {}
                Ok(w) => {
                    anyhow::bail!(
                        "range weight for {} is {w} (not 1.0); fractional-weight ranges \
                         require the aggregation weight fix first (audit tripwire)",
                        d.combo
                    );
                }
                Err(e) => {
                    errors.push(json!({ "decision": format!("{}#{}", d.hand_id, d.decision_index),
                        "error": format!("range check: {e}") }));
                    continue;
                }
            }

            let Some(h_idx) = find_hand(node, &d.combo) else {
                errors.push(json!({ "decision": format!("{}#{}", d.hand_id, d.decision_index),
                    "error": format!("combo {} not at this node (blocked or out of range)", d.combo) }));
                continue;
            };
            let Some(a_idx) = node.actions.iter().position(|a| a == &d.action_taken) else {
                errors.push(json!({ "decision": format!("{}#{}", d.hand_id, d.decision_index),
                    "error": format!("action '{}' not in node action set {:?}", d.action_taken, node.actions) }));
                continue;
            };
            if node.ev_per_action.len() != n * n_act {
                anyhow::bail!("node lacks per-action EV for {key} (read_node regression)");
            }

            let rec = GradedRecord {
                schema_version: 1,
                hand_id: d.hand_id.clone(),
                street: d.street.clone(),
                decision_index: d.decision_index,
                depth_bb: d.depth_bb,
                node_shape: d.node_shape.key().to_string(),
                texture: texture_bucket(&d.board).to_string(),
                board: d.board.clone(),
                combo: d.combo.clone(),
                action_taken: node.actions[a_idx].clone(),
                actions: node.actions.clone(),
                p: (0..n_act).map(|a| node.strategy[h_idx + a * n]).collect(),
                ev_mix: node.ev[h_idx],
                ev_actions: (0..n_act).map(|a| node.ev_per_action[h_idx + a * n]).collect(),
                source_file: source_file.to_string(),
                ingested_at: now.clone(),
            };
            // D45: the F5 tripwire runs per instance BEFORE the record can enter the store
            assert_record_sane(&rec, pot_chips, &cfg.review)?;
            records.push(rec);
        }
    }

    let path = Path::new(&cfg.io.matrix_dir).join("history").join("graded.jsonl");
    let (appended, duplicates) = hstore::append_records(&path, &records)?;

    Ok(json!({
        "graded": records.len(),
        "appended": appended,
        "duplicates": duplicates,
        "pending": pending,
        "errors": errors,
    }))
}

pub async fn report_handler(State(state): State<AppState>) -> Response {
    let path = store_path(&state);
    let (records, bad_lines) = match hstore::load_store(&path) {
        Ok(v) => v,
        Err(e) => return bad_request(format!("loading graded store: {e}")),
    };
    if records.is_empty() {
        return axum::Json(json!({
            "empty": true,
            "message": "no graded decisions stored yet; ingest a history first",
        }))
        .into_response();
    }
    let pot_chips = (SRP_POT_BB * state.cfg.solve.chip_per_bb as f64) as f32;
    let _ = SRP_RAISE_BB; // geometry constant shared with solve; pot is what matters here
    match aggregate(&records, pot_chips, state.cfg.solve.chip_per_bb as f64, &state.cfg.review) {
        Ok(report) => axum::Json(json!({
            "report": report,
            "store_bad_lines": bad_lines,
        }))
        .into_response(),
        Err(e) => bad_request(format!("aggregation failed: {e}")),
    }
}
