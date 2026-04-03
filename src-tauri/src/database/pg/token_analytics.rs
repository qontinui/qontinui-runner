//! PostgreSQL token analytics operations via Clorinde-generated queries.

use super::PgDb;
use crate::database::token_analytics::*;

impl PgDb {
    /// Get daily cost breakdown for the last N days.
    pub async fn get_daily_cost(&self, days: u32) -> Result<Vec<DailyCostRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let days_i = days as i32;
        let rows = qontinui_db::queries::token_analytics::get_daily_cost()
            .bind(&conn, &days_i)
            .all()
            .await
            .map_err(|e| format!("PG query daily cost: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| DailyCostRow {
                date: r.day,
                total_cost_cents: r.total_cost_cents as u64,
                total_input_tokens: r.total_input_tokens as u64,
                total_output_tokens: r.total_output_tokens as u64,
                call_count: r.call_count as u64,
            })
            .collect())
    }

    /// Get cost breakdown by model for the last N days.
    pub async fn get_cost_by_model(&self, days: u32) -> Result<Vec<ModelCostRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let days_i = days as i32;
        let rows = qontinui_db::queries::token_analytics::get_cost_by_model()
            .bind(&conn, &days_i)
            .all()
            .await
            .map_err(|e| format!("PG query cost by model: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| ModelCostRow {
                model_used: r.model_used,
                total_cost_cents: r.total_cost_cents as u64,
                total_input_tokens: r.total_input_tokens as u64,
                total_output_tokens: r.total_output_tokens as u64,
                call_count: r.call_count as u64,
                avg_duration_ms: Some(r.avg_duration_ms as u64),
            })
            .collect())
    }

    /// Get cost breakdown by workflow phase for the last N days.
    pub async fn get_cost_by_phase(&self, days: u32) -> Result<Vec<PhaseCostRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let days_i = days as i32;
        let rows = qontinui_db::queries::token_analytics::get_cost_by_phase()
            .bind(&conn, &days_i)
            .all()
            .await
            .map_err(|e| format!("PG query cost by phase: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| PhaseCostRow {
                phase: r.phase,
                total_cost_cents: r.total_cost_cents as u64,
                total_input_tokens: r.total_input_tokens as u64,
                total_output_tokens: r.total_output_tokens as u64,
            })
            .collect())
    }

    /// Get latency stats by provider for the last N days.
    pub async fn get_provider_latency(&self, days: u32) -> Result<Vec<ProviderLatencyRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let days_i = days as i32;
        let rows = qontinui_db::queries::token_analytics::get_provider_latency()
            .bind(&conn, &days_i)
            .all()
            .await
            .map_err(|e| format!("PG query provider latency: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| ProviderLatencyRow {
                provider_used: r.provider_used,
                avg_duration_ms: r.avg_duration_ms as u64,
                min_duration_ms: r.min_duration_ms as u64,
                max_duration_ms: r.max_duration_ms as u64,
                call_count: r.call_count as u64,
            })
            .collect())
    }

    /// Get per-task-run cost breakdown for the last N days.
    pub async fn get_task_run_costs(
        &self,
        days: u32,
        limit: u32,
    ) -> Result<Vec<TaskRunCostRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let days_i = days as i32;
        let limit_i = limit as i64;
        let rows = qontinui_db::queries::token_analytics::get_task_run_costs()
            .bind(&conn, &days_i, &limit_i)
            .all()
            .await
            .map_err(|e| format!("PG query task run costs: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| TaskRunCostRow {
                task_run_id: r.task_run_id,
                total_cost_cents: r.total_cost_cents as u64,
                total_input_tokens: r.total_input_tokens as u64,
                total_output_tokens: r.total_output_tokens as u64,
                call_count: r.call_count as u64,
                started_at: r.started_at,
            })
            .collect())
    }

    /// Get an aggregate summary of token usage for the last N days.
    pub async fn get_token_usage_summary(&self, days: u32) -> Result<TokenUsageSummary, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let days_i = days as i32;

        let totals = qontinui_db::queries::token_analytics::get_token_usage_totals()
            .bind(&conn, &days_i)
            .one()
            .await
            .map_err(|e| format!("PG query token usage totals: {}", e))?;

        let unique_models = qontinui_db::queries::token_analytics::get_unique_models_count()
            .bind(&conn, &days_i)
            .one()
            .await
            .map_err(|e| format!("PG query unique models: {}", e))?;

        let unique_providers = qontinui_db::queries::token_analytics::get_unique_providers_count()
            .bind(&conn, &days_i)
            .one()
            .await
            .map_err(|e| format!("PG query unique providers: {}", e))?;

        let total_calls = totals.total_calls as u64;
        let total_cost = totals.total_cost_cents as u64;
        let avg_cost_per_call = if total_calls > 0 {
            total_cost as f64 / total_calls as f64
        } else {
            0.0
        };

        Ok(TokenUsageSummary {
            total_cost_cents: total_cost,
            total_input_tokens: totals.total_input_tokens as u64,
            total_output_tokens: totals.total_output_tokens as u64,
            total_calls,
            unique_models: unique_models as u64,
            unique_providers: unique_providers as u64,
            avg_cost_per_call_cents: avg_cost_per_call,
            avg_duration_ms: Some(totals.avg_duration_ms as f64),
        })
    }

    /// Get cost breakdown by target app for the last N days.
    pub async fn get_cost_by_target_app(&self, days: u32) -> Result<Vec<TargetAppCostRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let days_i = days as i32;
        let rows = qontinui_db::queries::token_analytics::get_cost_by_target_app()
            .bind(&conn, &days_i)
            .all()
            .await
            .map_err(|e| format!("PG query cost by target app: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| TargetAppCostRow {
                target_app: r.target_app,
                total_cost_cents: r.total_cost_cents as u64,
                total_input_tokens: r.total_input_tokens as u64,
                total_output_tokens: r.total_output_tokens as u64,
                call_count: r.call_count as u64,
                avg_duration_ms: Some(r.avg_duration_ms as u64),
            })
            .collect())
    }

    /// Get per-phase cost breakdown with cache metrics for the cost dashboard.
    ///
    /// Returns (phase, input_tokens, output_tokens, cache_creation, cache_read, cost_cents).
    pub async fn get_phase_cost_with_cache(
        &self,
        days: u32,
    ) -> Result<Vec<(String, i64, i64, i64, i64, i64)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let since = (chrono::Utc::now() - chrono::Duration::days(days as i64)).to_rfc3339();
        let rows = conn
            .query(
                r#"SELECT
                    phase,
                    COALESCE(SUM(input_tokens), 0)::bigint as input_tokens,
                    COALESCE(SUM(output_tokens), 0)::bigint as output_tokens,
                    COALESCE(SUM(cache_creation_tokens), 0)::bigint as cache_creation,
                    COALESCE(SUM(cache_read_tokens), 0)::bigint as cache_read,
                    COALESCE(SUM(cost_cents), 0)::bigint as cost_cents
                FROM phase_token_usage
                WHERE created_at > $1::timestamptz
                GROUP BY phase
                ORDER BY cost_cents DESC"#,
                &[&since],
            )
            .await
            .map_err(|e| format!("PG query phase cost with cache: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<_, String>(0),
                    r.get::<_, i64>(1),
                    r.get::<_, i64>(2),
                    r.get::<_, i64>(3),
                    r.get::<_, i64>(4),
                    r.get::<_, i64>(5),
                )
            })
            .collect())
    }

    /// Get cost breakdown by target page for the last N days.
    pub async fn get_cost_by_target_page(
        &self,
        days: u32,
    ) -> Result<Vec<TargetPageCostRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let days_i = days as i32;
        let rows = qontinui_db::queries::token_analytics::get_cost_by_target_page()
            .bind(&conn, &days_i)
            .all()
            .await
            .map_err(|e| format!("PG query cost by target page: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| TargetPageCostRow {
                target_page_url: r.target_page_url,
                total_cost_cents: r.total_cost_cents as u64,
                total_input_tokens: r.total_input_tokens as u64,
                total_output_tokens: r.total_output_tokens as u64,
                call_count: r.call_count as u64,
            })
            .collect())
    }

    /// Get cache hit rate and estimated savings for a specific model since a given date.
    ///
    /// Returns `(avg_cache_hit_rate, avg_cache_savings_usd)`.
    pub async fn get_model_cache_stats(
        &self,
        model_id: &str,
        period_start: &str,
    ) -> Result<(f64, f64), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_opt(
                "SELECT
                    COALESCE(SUM(cache_read_tokens), 0)::float8 as total_cache_read,
                    COALESCE(SUM(cache_creation_tokens), 0)::float8 as total_cache_create,
                    COALESCE(SUM(input_tokens), 0)::float8 as total_input
                FROM phase_token_usage
                WHERE model_used = $1 AND created_at >= $2::timestamptz",
                &[&model_id, &period_start],
            )
            .await
            .map_err(|e| format!("Failed to query model cache stats: {}", e))?;

        let (cache_read, cache_create, input): (f64, f64, f64) = match row {
            Some(r) => (r.get(0), r.get(1), r.get(2)),
            None => return Ok((0.0, 0.0)),
        };

        let total = cache_read + cache_create + input;
        let hit_rate = if total > 0.0 { cache_read / total } else { 0.0 };
        // Savings estimate: cache reads avoid ~90% of input cost.
        // Using ~$3/MTok Sonnet baseline => cache read saves $2.70/MTok.
        let savings = cache_read * 2.70 / 1_000_000.0;

        Ok((hit_rate, savings))
    }
}
