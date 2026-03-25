//! PostgreSQL token analytics operations via Clorinde-generated queries.

use super::PgDb;
use crate::database::token_analytics::*;

impl PgDb {
    /// Get daily cost breakdown for the last N days.
    pub async fn get_daily_cost(&self, days: u32) -> Result<Vec<DailyCostRow>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
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

    /// Get cost breakdown by target page for the last N days.
    pub async fn get_cost_by_target_page(&self, days: u32) -> Result<Vec<TargetPageCostRow>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
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
}
