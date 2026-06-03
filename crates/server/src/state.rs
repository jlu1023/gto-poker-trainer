//! Shared server state: the loaded config, the preflop charts, and the single-solve slot.

use gto_config::Config;
use gto_preflop::PreflopCharts;
use std::sync::Arc;
use tokio::sync::Semaphore;

/// One permit = one solve at a time (D30). A second submit while a solve runs gets an immediate
/// error event. The permit is moved into the blocking solve thread, so it releases only when the
/// CPU work actually finishes (a timed-out orphan still holds the slot until it completes).
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub charts: Arc<PreflopCharts>,
    pub solve_slot: Arc<Semaphore>,
}

impl AppState {
    pub fn new(cfg: Config, charts: PreflopCharts) -> Self {
        Self {
            cfg: Arc::new(cfg),
            charts: Arc::new(charts),
            solve_slot: Arc::new(Semaphore::new(1)),
        }
    }
}
