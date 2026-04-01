//! Online/Continual Learning System
//!
//! Provides incremental learning capabilities that operate after every workflow
//! run, augmenting the batch-oriented meta-optimizer pipeline.
//!
//! ## Architecture
//!
//! All subsystems feed into a single async coordinator spawned after each run:
//!
//! 1. **Bandit Framework** — Generic contextual bandit with pluggable policies
//! 2. **Model Router** — Bandit-based LLM model selection (replaces static heuristics)
//! 3. **Credit Assignment** — ADCA step-level attribution for causal scoring
//! 4. **Reflection** — Post-step/post-run experience extraction
//! 5. **Strategy Bank** — Evolvable optimization strategies with Thompson Sampling
//! 6. **Coordinator** — Non-blocking post-run pipeline orchestrator
//!
//! The drift detection engine lives in `meta_optimizer::drift_detection` since
//! it's tightly coupled to the meta-optimizer's trigger system.

pub mod bandit;
pub mod context;
pub mod coordinator;
pub mod credit_assignment;
pub mod model_router;
pub mod policies;
pub mod reflection;
pub mod strategy_bank;
pub mod strategy_evolution;

use std::sync::{Mutex, OnceLock};
use tracing::warn;

// =============================================================================
// Global singletons
// =============================================================================

/// Global model router bandit, loaded from PG at startup.
static GLOBAL_MODEL_ROUTER: OnceLock<Mutex<model_router::ModelRouterBandit>> = OnceLock::new();

/// Global drift monitor, loaded from PG at startup.
static GLOBAL_DRIFT_MONITOR: OnceLock<Mutex<crate::meta_optimizer::drift_detection::DriftMonitor>> =
    OnceLock::new();

/// Initialize the global model router. Call once during app startup.
pub fn init_model_router(router: model_router::ModelRouterBandit) {
    GLOBAL_MODEL_ROUTER
        .set(Mutex::new(router))
        .unwrap_or_else(|_| warn!("Model router already initialized (ignored)"));
}

/// Initialize the global drift monitor. Call once during app startup.
pub fn init_drift_monitor(monitor: crate::meta_optimizer::drift_detection::DriftMonitor) {
    GLOBAL_DRIFT_MONITOR
        .set(Mutex::new(monitor))
        .unwrap_or_else(|_| warn!("Drift monitor already initialized (ignored)"));
}

/// Get the global model router. Returns None if not initialized.
pub fn model_router() -> Option<&'static Mutex<model_router::ModelRouterBandit>> {
    GLOBAL_MODEL_ROUTER.get()
}

/// Get the global drift monitor. Returns None if not initialized.
pub fn drift_monitor() -> Option<&'static Mutex<crate::meta_optimizer::drift_detection::DriftMonitor>> {
    GLOBAL_DRIFT_MONITOR.get()
}

/// Initialize all online learning singletons with defaults.
/// Called during app startup. Loads persisted state from PG if available.
pub async fn initialize(pg_db: &std::sync::Arc<crate::database::pg::PgDb>) {
    // Initialize model router with persisted Q-table
    let mut router = model_router::ModelRouterBandit::new();
    match pg_db.load_model_routing_table().await {
        Ok(rows) if !rows.is_empty() => {
            tracing::info!("Loading {} model routing entries from PG", rows.len());
            router.load_from_rows(rows);
        }
        Ok(_) => tracing::info!("No persisted model routing data — starting fresh"),
        Err(e) => tracing::warn!("Failed to load model routing table: {}", e),
    }
    match pg_db.load_model_routing_overrides().await {
        Ok(overrides) if !overrides.is_empty() => {
            tracing::info!("Loading {} model routing overrides from PG", overrides.len());
            router.load_overrides(overrides);
        }
        _ => {}
    }
    init_model_router(router);

    // Initialize drift monitor with persisted state
    let monitor = match pg_db.load_drift_detector_state("global_monitor").await {
        Ok(Some(json)) => {
            match crate::meta_optimizer::drift_detection::DriftMonitor::deserialize_state(&json) {
                Ok(m) => {
                    tracing::info!("Loaded drift monitor state from PG ({} detectors)", m.detector_count());
                    m
                }
                Err(e) => {
                    tracing::warn!("Failed to deserialize drift monitor state: {} — starting fresh", e);
                    crate::meta_optimizer::drift_detection::DriftMonitor::new()
                }
            }
        }
        _ => {
            tracing::info!("No persisted drift monitor state — starting fresh");
            crate::meta_optimizer::drift_detection::DriftMonitor::new()
        }
    };
    init_drift_monitor(monitor);

    tracing::info!("Online learning system initialized");
}
