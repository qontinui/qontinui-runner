//! PostgreSQL token usage operations via Clorinde-generated queries.

use super::PgDb;
use crate::database::types::PhaseTokenUsageRow;

impl PgDb {
    /// Record token usage for a single AI call within a workflow phase.
    pub async fn create_phase_token_usage(
        &self,
        task_run_id: &str,
        phase: &str,
        stage_index: Option<u32>,
        iteration: Option<u32>,
        model_used: Option<&str>,
        provider_used: Option<&str>,
        input_tokens: u64,
        output_tokens: u64,
        cost_cents: u64,
        duration_ms: Option<u64>,
    ) -> Result<(), String> {
        self.create_phase_token_usage_with_target(
            task_run_id, phase, stage_index, iteration, model_used, provider_used,
            input_tokens, output_tokens, cost_cents, duration_ms, None, None,
        )
        .await
    }

    /// Record token usage with optional target app/page context.
    pub async fn create_phase_token_usage_with_target(
        &self,
        task_run_id: &str,
        phase: &str,
        stage_index: Option<u32>,
        iteration: Option<u32>,
        model_used: Option<&str>,
        provider_used: Option<&str>,
        input_tokens: u64,
        output_tokens: u64,
        cost_cents: u64,
        duration_ms: Option<u64>,
        target_app: Option<&str>,
        target_page_url: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let stage_index_i = stage_index.map(|v| v as i32);
        let iteration_i = iteration.map(|v| v as i32);
        let model_owned = model_used.map(|s| s.to_string());
        let provider_owned = provider_used.map(|s| s.to_string());
        let input_i = input_tokens as i64;
        let output_i = output_tokens as i64;
        let cost_i = cost_cents as i64;
        let duration_i = duration_ms.map(|v| v as i64);
        let target_app_owned = target_app.map(|s| s.to_string());
        let target_page_owned = target_page_url.map(|s| s.to_string());

        qontinui_db::queries::token_usage::create_phase_token_usage()
            .bind(
                &conn,
                &task_run_id,
                &phase,
                &stage_index_i,
                &iteration_i,
                &model_owned,
                &provider_owned,
                &input_i,
                &output_i,
                &cost_i,
                &duration_i,
                &target_app_owned,
                &target_page_owned,
            )
            .await
            .map_err(|e| format!("PG insert phase_token_usage: {}", e))?;
        Ok(())
    }

    /// Get per-phase token usage breakdown for a task run.
    pub async fn get_phase_token_usage(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<PhaseTokenUsageRow>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::token_usage::get_phase_token_usage()
            .bind(&conn, &task_run_id)
            .all()
            .await
            .map_err(|e| format!("PG query phase_token_usage: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| PhaseTokenUsageRow {
                phase: r.phase,
                stage_index: r.stage_index.map(|v| v as u32),
                iteration: r.iteration.map(|v| v as u32),
                model_used: r.model_used,
                provider_used: r.provider_used,
                input_tokens: r.input_tokens as u64,
                output_tokens: r.output_tokens as u64,
                cost_cents: r.cost_cents as u64,
                duration_ms: r.duration_ms.map(|v| v as u64),
                created_at: r.created_at.to_rfc3339(),
            })
            .collect())
    }

    /// Get summed token usage for a specific iteration of a task run.
    pub async fn get_iteration_token_totals(
        &self,
        task_run_id: &str,
        iteration: u32,
    ) -> Result<(u64, u64), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let iteration_i = iteration as i32;
        let row = qontinui_db::queries::token_usage::get_iteration_token_totals()
            .bind(&conn, &task_run_id, &iteration_i)
            .one()
            .await
            .map_err(|e| format!("PG query iteration totals: {}", e))?;

        Ok((row.total_input as u64, row.total_output as u64))
    }

    /// Update the aggregate token totals on a task run.
    pub async fn update_task_run_token_totals(&self, task_run_id: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::token_usage::update_task_run_token_totals()
            .bind(&conn, &task_run_id)
            .await
            .map_err(|e| format!("PG update task_run token totals: {}", e))?;
        Ok(())
    }
}
