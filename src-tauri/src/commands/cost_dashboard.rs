//! Cost Dashboard Tauri Commands
//!
//! Provides a unified cost overview including token usage, cache efficiency,
//! per-phase breakdowns, and budget utilization.

use crate::commands::compartments::{ExecutionCompartment, StorageCompartment};
use serde::Serialize;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;

use crate::database::pg::token_analytics::PhaseCacheCostRow;

/// Unified cost dashboard summary.
#[derive(Debug, Clone, Serialize)]
pub struct CostDashboard {
    pub total_cost_usd: f64,
    /// Where `total_cost_usd` came from, counted in `phase_token_usage` rows.
    pub cost_provenance: CostProvenance,
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
    /// Where this phase's `cost_usd` came from, counted in rows.
    pub cost_provenance: CostProvenance,
}

/// How many `phase_token_usage` rows behind a cost figure were provider-
/// REPORTED (`cost_source = 'reported'`), price-table ESTIMATED
/// (`'estimated'`), or of UNKNOWN provenance (`NULL` — rows written before
/// the column existed). Lets the UI label a figure instead of presenting an
/// estimate as a bill.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct CostProvenance {
    pub reported_rows: u64,
    pub estimated_rows: u64,
    pub unknown_rows: u64,
}

impl CostProvenance {
    fn add(&mut self, other: CostProvenance) {
        self.reported_rows += other.reported_rows;
        self.estimated_rows += other.estimated_rows;
        self.unknown_rows += other.unknown_rows;
    }
}

/// A phase's cost in micro-USD, at the precision each row carries.
///
/// `precise_microusd` sums `cost_microusd` over the rows that have it;
/// `legacy_cents` sums `cost_cents` over the rows that do not. `cost_cents`
/// holds WHOLE cents (the writer stores `round(usd * 100)`), so a cent is
/// 10_000 micro-USD. Negative inputs cannot be written (the writer clamps)
/// and are read as zero rather than as a refund.
pub fn phase_cost_microusd(precise_microusd: i64, legacy_cents: i64) -> i64 {
    precise_microusd.max(0) + legacy_cents.max(0).saturating_mul(10_000)
}

/// Convert micro-USD to USD. Sum in micro-USD first (exact integer
/// addition), convert once.
///
/// This replaces a conversion that divided whole `cost_cents` by 100_000 on
/// the belief that the column held "hundredths of cents", which rendered
/// every dashboard dollar figure 1000x too low.
pub fn microusd_to_usd(microusd: i64) -> f64 {
    microusd.max(0) as f64 / 1_000_000.0
}

/// Aggregate the per-phase query rows into the dashboard figures. Pure, so
/// the arithmetic is testable without a database.
fn summarize(rows: &[PhaseCacheCostRow], budget_utilization: Option<f64>) -> CostDashboard {
    let mut per_phase: Vec<PhaseCostBreakdown> = Vec::new();
    let mut total_input: u64 = 0;
    let mut total_output: u64 = 0;
    let mut total_cache_creation: u64 = 0;
    let mut total_cache_read: u64 = 0;
    let mut total_microusd: i64 = 0;
    let mut total_provenance = CostProvenance::default();

    for row in rows {
        let microusd = phase_cost_microusd(row.precise_microusd, row.legacy_cents);
        let provenance = CostProvenance {
            reported_rows: row.reported_rows.max(0) as u64,
            estimated_rows: row.estimated_rows.max(0) as u64,
            unknown_rows: row.unknown_rows.max(0) as u64,
        };
        total_input += row.input_tokens.max(0) as u64;
        total_output += row.output_tokens.max(0) as u64;
        total_cache_creation += row.cache_creation_tokens.max(0) as u64;
        total_cache_read += row.cache_read_tokens.max(0) as u64;
        total_microusd = total_microusd.saturating_add(microusd);
        total_provenance.add(provenance);

        per_phase.push(PhaseCostBreakdown {
            phase: row.phase.clone(),
            input_tokens: row.input_tokens.max(0) as u64,
            output_tokens: row.output_tokens.max(0) as u64,
            cache_creation_tokens: row.cache_creation_tokens.max(0) as u64,
            cache_read_tokens: row.cache_read_tokens.max(0) as u64,
            cost_usd: microusd_to_usd(microusd),
            cost_provenance: provenance,
        });
    }
    // Most expensive phase first, compared in exact micro-USD.
    per_phase.sort_by(|a, b| b.cost_usd.total_cmp(&a.cost_usd));

    let total_tokens = total_input + total_output;

    // Cache hit rate = cache_read / (cache_read + cache_creation + regular_input)
    let cache_denominator =
        total_cache_read as f64 + total_cache_creation as f64 + total_input as f64;
    let cache_hit_rate = if cache_denominator > 0.0 {
        total_cache_read as f64 / cache_denominator
    } else {
        0.0
    };

    // Approximate savings: cache reads bill at 0.1x the input price, so each
    // one avoids 90% of it. Sonnet's $3/MTok input is the representative
    // baseline: $3/M * 0.9 = $2.70 saved per million cache-read tokens.
    const CACHE_READ_SAVINGS_PER_MTOK: f64 = 2.70;
    let cache_savings_usd = total_cache_read as f64 * CACHE_READ_SAVINGS_PER_MTOK / 1_000_000.0;

    CostDashboard {
        total_cost_usd: microusd_to_usd(total_microusd),
        cost_provenance: total_provenance,
        total_tokens,
        cache_hit_rate,
        cache_savings_usd,
        per_phase_breakdown: per_phase,
        budget_utilization,
    }
}

/// Fetch the cost dashboard summary for the last N days.
///
/// Queries phase_token_usage for token totals, cache metrics, and per-phase
/// breakdowns. Returns a unified view suitable for the frontend dashboard.
#[tauri::command]
pub async fn get_cost_dashboard(
    storage: tauri::State<'_, StorageCompartment>,
    execution: tauri::State<'_, ExecutionCompartment>,
    days: Option<u32>,
) -> Result<CostDashboard, String> {
    let d = days.unwrap_or(30);
    let pg_db = storage.pg_db().clone();

    let rows = pg_db.get_phase_cost_with_cache(d).await?;

    // Budget utilization from any active run
    let budget_utilization = {
        let trackers = execution.run_cost_trackers().lock().await;
        trackers
            .values()
            .next()
            .map(|t| 1.0 - t.budget.remaining_fraction())
    };

    Ok(summarize(&rows, budget_utilization))
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
    execution: tauri::State<'_, ExecutionCompartment>,
) -> Result<Option<ActiveBudgetStatus>, String> {
    let trackers = execution.run_cost_trackers().lock().await;

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

#[cfg(test)]
mod tests {
    use super::{microusd_to_usd, phase_cost_microusd, summarize, CostProvenance};
    use crate::database::pg::token_analytics::PhaseCacheCostRow;
    use crate::database::pg::token_usage::PhaseCost;

    fn row(phase: &str, precise_microusd: i64, legacy_cents: i64) -> PhaseCacheCostRow {
        PhaseCacheCostRow {
            phase: phase.to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            precise_microusd,
            legacy_cents,
            reported_rows: 0,
            estimated_rows: 0,
            unknown_rows: 0,
        }
    }

    #[test]
    fn whole_cents_and_microdollars_combine_in_microdollars() {
        assert_eq!(phase_cost_microusd(0, 0), 0);
        assert_eq!(phase_cost_microusd(0, 1), 10_000);
        assert_eq!(phase_cost_microusd(4_200, 1_234), 12_344_200);
        assert_eq!(phase_cost_microusd(-1, -5), 0);
        assert_eq!(microusd_to_usd(12_340_000), 12.34);
        assert_eq!(microusd_to_usd(-5), 0.0);
    }

    /// Pin a phase_token_usage row as the writer produces it to the dollar
    /// figure the dashboard shows: 1M input + 1M output tokens on Sonnet 4 is
    /// $3 + $15 = $18.00, stored as `cost_cents = 1800`. The old /100_000
    /// conversion rendered it as $0.018. A legacy row (no `cost_microusd`)
    /// reads back the same through the whole-cent arm.
    #[test]
    fn a_written_phase_token_usage_row_reads_back_as_its_dollar_cost() {
        let model = "claude-sonnet-4-20250514";
        let usd = crate::ai_pricing::calculate_cost_usd(1_000_000, 1_000_000, model);
        let written = PhaseCost::estimated(usd);
        assert_eq!(
            written.cents, 1_800,
            "fixture premise: Sonnet 4 at $3/$15 per MTok"
        );
        let precise = written.microusd.expect("the writer records microdollars");
        assert_eq!(microusd_to_usd(phase_cost_microusd(precise, 0)), 18.0);
        assert_eq!(
            microusd_to_usd(phase_cost_microusd(0, written.cents as i64)),
            18.0
        );
    }

    /// The reason the precision column exists: a reported $0.0042 call is
    /// `cost_cents = 0`. Summed in whole cents it would show $0.00; summed in
    /// micro-USD it shows what was billed, and is labelled reported.
    #[test]
    fn a_sub_cent_reported_row_is_not_rendered_as_free() {
        let written = PhaseCost::reported(0.0042);
        assert_eq!(written.cents, 0);
        let mut agentic = row("agentic", written.microusd.unwrap(), 0);
        agentic.reported_rows = 1;
        // A legacy row of unknown provenance in another phase.
        let mut setup = row("setup", 0, 7);
        setup.unknown_rows = 1;

        let dashboard = summarize(&[agentic, setup], None);
        assert!((dashboard.total_cost_usd - 0.0742).abs() < 1e-12);
        assert_eq!(
            dashboard.cost_provenance,
            CostProvenance {
                reported_rows: 1,
                estimated_rows: 0,
                unknown_rows: 1,
            }
        );
        // Most expensive phase first.
        assert_eq!(dashboard.per_phase_breakdown[0].phase, "setup");
        assert!((dashboard.per_phase_breakdown[0].cost_usd - 0.07).abs() < 1e-12);
        let agentic = &dashboard.per_phase_breakdown[1];
        assert!((agentic.cost_usd - 0.0042).abs() < 1e-12);
        assert_eq!(agentic.cost_provenance.reported_rows, 1);
    }
}
