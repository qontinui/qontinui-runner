//! Cost Dashboard Tauri Commands
//!
//! Provides a unified cost overview including token usage, cache efficiency,
//! per-phase breakdowns, and budget utilization.

use serde::Serialize;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;

/// Unified cost dashboard summary.
#[derive(Debug, Clone, Serialize)]
pub struct CostDashboard {
    pub total_cost_usd: f64,
    pub total_tokens: u64,
    pub cache_hit_rate: f64,
    pub cache_savings_usd: f64,
    pub per_phase_breakdown: Vec<PhaseCostBreakdown>,
    pub budget_utilization: Option<f64>,
}

/// Per-phase cost breakdown with cache metrics.
#[derive(Debug, Clone, Serialize)]
pub struct PhaseCostBreakdown {
    pub phase: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub cost_usd: f64,
}

/// Fetch the cost dashboard summary for the last N days.
///
/// Queries phase_token_usage for token totals, cache metrics, and per-phase
/// breakdowns. Returns a unified view suitable for the frontend dashboard.
#[tauri::command]
pub async fn get_cost_dashboard(
    state: tauri::State<'_, std::sync::Arc<crate::commands::AppState>>,
    days: Option<u32>,
) -> Result<CostDashboard, String> {
    let d = days.unwrap_or(30);
    let pg_db = state.pg_db.clone();

    let rows = pg_db.get_phase_cost_with_cache(d).await?;

    let mut per_phase: Vec<PhaseCostBreakdown> = Vec::new();
    let mut total_input: u64 = 0;
    let mut total_output: u64 = 0;
    let mut total_cache_creation: u64 = 0;
    let mut total_cache_read: u64 = 0;
    let mut total_cost_cents: u64 = 0;

    for (phase, input, output, creation, read, cost) in &rows {
        total_input += *input as u64;
        total_output += *output as u64;
        total_cache_creation += *creation as u64;
        total_cache_read += *read as u64;
        total_cost_cents += *cost as u64;

        per_phase.push(PhaseCostBreakdown {
            phase: phase.clone(),
            input_tokens: *input as u64,
            output_tokens: *output as u64,
            cache_creation_tokens: *creation as u64,
            cache_read_tokens: *read as u64,
            // cost_cents is in hundredths-of-cents (microdollars * 100)
            cost_usd: *cost as f64 / 100_000.0,
        });
    }

    let total_tokens = total_input + total_output;
    // cost_cents is in hundredths-of-cents
    let total_cost_usd = total_cost_cents as f64 / 100_000.0;

    // Cache hit rate = cache_read / (cache_read + cache_creation + regular_input)
    let cache_denominator =
        total_cache_read as f64 + total_cache_creation as f64 + total_input as f64;
    let cache_hit_rate = if cache_denominator > 0.0 {
        total_cache_read as f64 / cache_denominator
    } else {
        0.0
    };

    // Estimate cache savings: cached reads are ~90% cheaper than regular input.
    // At current Sonnet pricing ($3/MTok input, $0.30/MTok cached read),
    // savings = cache_read_tokens * (input_price - cache_price) / 1M tokens.
    // We approximate at $2.70 savings per million cached tokens.
    // Approximate savings: cache reads avoid 90% of input cost.
    // Using Sonnet pricing ($3/MTok input) as representative baseline.
    // Savings per cache-read token = $3/M * 0.9 = $2.70/M
    const CACHE_READ_SAVINGS_PER_MTOK: f64 = 2.70;
    let cache_savings_usd = total_cache_read as f64 * CACHE_READ_SAVINGS_PER_MTOK / 1_000_000.0;

    Ok(CostDashboard {
        total_cost_usd,
        total_tokens,
        cache_hit_rate,
        cache_savings_usd,
        per_phase_breakdown: per_phase,
        // Get budget utilization from any active run
        budget_utilization: {
            let trackers = state.run_cost_trackers.lock().await;
            trackers
                .values()
                .next()
                .map(|t| 1.0 - t.budget.remaining_fraction())
        },
    })
}

/// Active budget status for the Cost Control panel.
#[derive(Debug, Clone, Serialize)]
pub struct ActiveBudgetStatus {
    pub execution_id: String,
    pub total_cost_usd: f64,
    pub max_cost_usd: f64,
    pub remaining_fraction: f64,
    pub total_tokens: u64,
    pub max_tokens: u64,
    pub circuit_breaker_tripped: bool,
    pub anomaly_detector_sample_count: u32,
    pub anomaly_detector_mean: f64,
}

/// Get the active budget status for the Cost Control panel.
///
/// Returns the budget snapshot, circuit breaker state, and anomaly detector
/// stats for any currently active run. Returns None if no run is active.
#[tauri::command]
pub async fn get_active_budget_status(
    state: tauri::State<'_, std::sync::Arc<crate::commands::AppState>>,
) -> Result<Option<ActiveBudgetStatus>, String> {
    let trackers = state.run_cost_trackers.lock().await;

    // Return the first active run's status (most common case: single run)
    if let Some((execution_id, t)) = trackers.iter().next() {
        let snapshot = t.budget.snapshot();
        let remaining = t.budget.remaining_fraction();
        let (anomaly_count, anomaly_mean) = match t.anomaly_detector.lock() {
            Ok(d) => (d.sample_count(), d.mean()),
            Err(_) => (0, 0.0),
        };

        Ok(Some(ActiveBudgetStatus {
            execution_id: execution_id.clone(),
            total_cost_usd: snapshot.total_cost_usd,
            max_cost_usd: snapshot.max_cost_usd,
            remaining_fraction: remaining,
            total_tokens: snapshot.total_tokens,
            max_tokens: snapshot.max_tokens,
            circuit_breaker_tripped: t.circuit_breaker.is_tripped(),
            anomaly_detector_sample_count: anomaly_count,
            anomaly_detector_mean: anomaly_mean,
        }))
    } else {
        Ok(None)
    }
}

pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_cost_dashboard")
        .invoke_handler(tauri::generate_handler![
            get_cost_dashboard,
            get_active_budget_status,
        ])
        .build()
}
