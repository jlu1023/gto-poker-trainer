//! `gto-server`: the Phase 3 localhost solve-on-demand server.
//!
//! Wraps the engine behind two endpoints (no auth, binds 127.0.0.1 only):
//!   GET  /api/health          -> {"ok": true}
//!   POST /api/solve           -> SSE stream: progress / complete / error
//!
//! Run from the repo root (it loads `phase1.toml` and reads/writes `matrix/`):
//!   cargo run --release -p gto-server

mod solve;
mod state;
mod trainer;

use anyhow::Context;
use axum::routing::{get, post};
use axum::Router;
use gto_config::Config;
use gto_preflop::PreflopCharts;
use state::AppState;
use std::path::Path;
use tower_http::cors::{Any, CorsLayer};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // phase1.toml = the srp-v1 tree, the same config the batch runner uses, so on-demand spot
    // keys are interchangeable with the Phase 1 benchmark entries. Fail fast if missing: a
    // defaulted config would silently solve under the wrong tree id and split the cache.
    let cfg = Config::load("phase1.toml")
        .context("loading phase1.toml (run the server from the repo root)")?;
    let charts = PreflopCharts::load_embedded().context("loading embedded preflop charts")?;

    let matrix = Path::new(&cfg.io.matrix_dir);
    if !matrix.join("manifest.json").exists() {
        eprintln!(
            "note: no matrix manifest at {}/manifest.json yet; every solve will be fresh and \
             cached there",
            matrix.display()
        );
    }

    let timeout_secs = cfg.io.solve_timeout_secs;
    let app_state = AppState::new(cfg, charts);

    // the Vite dev server is a different origin; localhost-only, no credentials -> permissive CORS
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/solve", post(solve::solve_handler))
        .route("/api/setup", get(trainer::setup_handler))
        .route("/api/grade", post(trainer::grade_handler))
        .layer(cors)
        .with_state(app_state);

    let addr = "127.0.0.1:7474";
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr} (is another gto-server running?)"))?;
    println!(
        "gto-server on http://{addr}  (50bb SRP solve-on-demand, tree srp-v1, \
         solve timeout {timeout_secs}s)"
    );
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "ok": true }))
}
