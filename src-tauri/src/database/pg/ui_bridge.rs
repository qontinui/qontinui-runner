//! PostgreSQL UI Bridge analytics operations via Clorinde-generated queries.
//!
//! Covers: element events, stall events, and all analytics queries.

use super::PgDb;
use crate::database::ui_bridge_ops::*;

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

fn now_epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Map an event row (with Option fields from named row type) to UiBridgeEvent.
macro_rules! event_row {
    ($r:expr) => {{
        UiBridgeEvent {
            id: $r.id,
            task_run_id: $r.task_run_id,
            timestamp: $r.timestamp,
            sequence: $r.sequence,
            event_type: $r.event_type,
            element_id: $r.element_id,
            state_id: $r.state_id,
            transition_id: $r.transition_id,
            action: $r.action,
            params: $r.params,
            result: $r.result,
            duration_ms: $r.duration_ms,
            success: $r.success,
            error_message: $r.error_message,
            metadata: $r.metadata,
        }
    }};
}

impl PgDb {
    // ========================================================================
    // Write Operations
    // ========================================================================

    /// Insert a UI Bridge event.
    pub async fn insert_ui_bridge_event(
        &self,
        task_run_id: Option<i64>,
        sequence: i64,
        event_type: &str,
        element_id: Option<&str>,
        state_id: Option<&str>,
        transition_id: Option<&str>,
        action: Option<&str>,
        params: Option<&str>,
        result: Option<&str>,
        duration_ms: Option<f64>,
        success: bool,
        error_message: Option<&str>,
        metadata: Option<&str>,
    ) -> Result<i64, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let ts = now_epoch_ms();
        let id = qontinui_db::queries::ui_bridge::insert_ui_bridge_event()
            .bind(
                &conn, &task_run_id, &ts, &sequence, &event_type, &element_id,
                &state_id, &transition_id, &action, &params, &result,
                &duration_ms, &success, &error_message, &metadata,
            )
            .one()
            .await
            .map_err(|e| format!("PG insert_ui_bridge_event: {}", e))?;
        Ok(id)
    }

    /// Insert a stall event.
    pub async fn insert_stall_event(
        &self,
        id: &str,
        task_run_id: &str,
        iteration: i32,
        pattern_type: &str,
        pattern_details: Option<&str>,
        action_count: Option<i32>,
        intervention_action: Option<&str>,
        intervention_result: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        qontinui_db::queries::ui_bridge::insert_stall_event()
            .bind(
                &conn, &id, &task_run_id, &iteration, &pattern_type,
                &pattern_details, &action_count, &intervention_action, &intervention_result,
            )
            .one()
            .await
            .map_err(|e| format!("PG insert_stall_event: {}", e))?;
        Ok(())
    }

    // ========================================================================
    // Read Operations
    // ========================================================================

    /// Get all events for a task run.
    pub async fn get_element_interactions(&self, task_run_id: i64) -> Result<Vec<UiBridgeEvent>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::ui_bridge::get_element_interactions()
            .bind(&conn, &task_run_id)
            .all()
            .await
            .map_err(|e| format!("PG get_element_interactions: {}", e))?;

        Ok(rows.into_iter().map(|r| event_row!(r)).collect())
    }

    /// Get cross-run element history.
    pub async fn get_element_history(&self, element_id: &str, limit: i64) -> Result<Vec<UiBridgeEvent>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::ui_bridge::get_element_history()
            .bind(&conn, &element_id, &limit)
            .all()
            .await
            .map_err(|e| format!("PG get_element_history: {}", e))?;

        Ok(rows.into_iter().map(|r| event_row!(r)).collect())
    }

    /// Get elements with high failure rates.
    pub async fn get_flaky_elements(&self, min_interactions: i64, max_success_rate: f64) -> Result<Vec<ElementReliability>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::ui_bridge::get_flaky_elements()
            .bind(&conn, &min_interactions, &max_success_rate)
            .all()
            .await
            .map_err(|e| format!("PG get_flaky_elements: {}", e))?;

        let mut results: Vec<ElementReliability> = rows.into_iter().map(|r| ElementReliability {
            element_id: r.element_id,
            total_interactions: r.total,
            successful_interactions: r.successes,
            success_rate: r.rate,
            last_failure_reason: None,
            flaky: r.rate < 0.95,
            recommended_confidence: r.rate.max(0.1),
        }).collect();

        // Enrich with last failure reason
        for elem in &mut results {
            if let Ok(reason) = qontinui_db::queries::ui_bridge::get_last_failure_reason()
                .bind(&conn, &elem.element_id.as_str())
                .opt()
                .await
            {
                if let Some(r) = reason {
                    elem.last_failure_reason = Some(r);
                }
            }
        }

        Ok(results)
    }

    /// Get reliability for a single element.
    pub async fn get_element_reliability(&self, element_id: &str) -> Result<Option<ElementReliability>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let row = qontinui_db::queries::ui_bridge::get_element_reliability()
            .bind(&conn, &element_id)
            .opt()
            .await
            .map_err(|e| format!("PG get_element_reliability: {}", e))?;

        match row {
            Some(r) => {
                let mut er = ElementReliability {
                    element_id: r.element_id,
                    total_interactions: r.total,
                    successful_interactions: r.successes,
                    success_rate: r.rate,
                    last_failure_reason: None,
                    flaky: r.rate < 0.95,
                    recommended_confidence: r.rate.max(0.1),
                };
                if let Ok(Some(reason)) = qontinui_db::queries::ui_bridge::get_last_failure_reason()
                    .bind(&conn, &element_id)
                    .opt()
                    .await
                {
                    er.last_failure_reason = Some(reason);
                }
                Ok(Some(er))
            }
            None => Ok(None),
        }
    }

    /// Get stall events for a task run.
    pub async fn get_stall_events(&self, task_run_id: &str) -> Result<Vec<StallEvent>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::ui_bridge::get_stall_events()
            .bind(&conn, &task_run_id)
            .all()
            .await
            .map_err(|e| format!("PG get_stall_events: {}", e))?;

        Ok(rows.into_iter().map(|r| StallEvent {
            id: r.id,
            task_run_id: r.task_run_id,
            iteration: r.iteration as i64,
            pattern_type: r.pattern_type,
            pattern_details: non_empty(r.pattern_details),
            action_count: if r.action_count == 0 { None } else { Some(r.action_count as i64) },
            intervention_action: non_empty(r.intervention_action),
            intervention_result: non_empty(r.intervention_result),
            created_at: r.created_at.to_rfc3339(),
        }).collect())
    }

    // ========================================================================
    // Analytics Queries
    // ========================================================================

    /// Element decay curve (success rate over time windows).
    pub async fn get_element_decay_curve(&self, element_id: &str, window_ms: i64, num_windows: i64) -> Result<Vec<DecayCurveBucket>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::ui_bridge::get_element_decay_curve()
            .bind(&conn, &window_ms, &element_id, &num_windows)
            .all()
            .await
            .map_err(|e| format!("PG get_element_decay_curve: {}", e))?;

        Ok(rows.into_iter().map(|r| DecayCurveBucket {
            bucket: r.bucket,
            total: r.total,
            successes: r.successes,
            success_rate: r.rate,
        }).collect())
    }

    /// Per-action latency baselines.
    pub async fn get_action_latency_baselines(&self, since_epoch_ms: i64) -> Result<Vec<ActionBaseline>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::ui_bridge::get_action_latency_baselines()
            .bind(&conn, &since_epoch_ms)
            .all()
            .await
            .map_err(|e| format!("PG get_action_latency_baselines: {}", e))?;

        Ok(rows.into_iter().map(|r| ActionBaseline {
            action: r.action,
            count: r.cnt,
            avg_duration_ms: Some(r.avg_dur),
            min_duration_ms: Some(r.min_dur),
            max_duration_ms: Some(r.max_dur),
            success_rate: r.rate,
        }).collect())
    }

    /// Failure taxonomy (error clusters).
    pub async fn get_failure_taxonomy(&self, since_epoch_ms: i64, limit: i64) -> Result<Vec<FailureCluster>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::ui_bridge::get_failure_taxonomy()
            .bind(&conn, &since_epoch_ms, &limit)
            .all()
            .await
            .map_err(|e| format!("PG get_failure_taxonomy: {}", e))?;

        Ok(rows.into_iter().map(|r| FailureCluster {
            error_message: r.error_message,
            count: r.freq,
            affected_elements: r.elements,
        }).collect())
    }

    /// Element fragility with bounds (for heatmap).
    pub async fn get_element_fragility_by_region(&self, since_epoch_ms: i64) -> Result<Vec<ElementFragility>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::ui_bridge::get_element_fragility_by_region()
            .bind(&conn, &since_epoch_ms)
            .all()
            .await
            .map_err(|e| format!("PG get_element_fragility_by_region: {}", e))?;

        Ok(rows.into_iter().map(|r| ElementFragility {
            element_id: r.element_id,
            bounds: non_empty(r.bounds),
            interaction_count: r.cnt,
            success_rate: r.rate,
        }).collect())
    }

    /// Automation regressions.
    pub async fn get_automation_regressions(&self, since_epoch_ms: i64, limit: i64) -> Result<Vec<AutomationRegression>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::ui_bridge::get_automation_regressions()
            .bind(&conn, &since_epoch_ms, &limit)
            .all()
            .await
            .map_err(|e| format!("PG get_automation_regressions: {}", e))?;

        Ok(rows.into_iter().map(|r| AutomationRegression {
            element_id: r.element_id,
            action: r.action,
            prior_success_rate: r.prior_rate,
            recent_success_rate: r.recent_rate,
            delta: r.recent_rate - r.prior_rate,
        }).collect())
    }

    /// Stall frequency by pattern type.
    pub async fn get_stall_frequency(&self, since_timestamp: &str) -> Result<Vec<StallFrequency>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let ts: chrono::DateTime<chrono::FixedOffset> = since_timestamp.parse()
            .unwrap_or_else(|_| chrono::Utc::now().fixed_offset());
        let rows = qontinui_db::queries::ui_bridge::get_stall_frequency()
            .bind(&conn, &ts)
            .all()
            .await
            .map_err(|e| format!("PG get_stall_frequency: {}", e))?;

        Ok(rows.into_iter().map(|r| StallFrequency {
            pattern_type: r.pattern_type,
            count: r.cnt,
        }).collect())
    }

    /// Intervention effectiveness.
    pub async fn get_intervention_effectiveness(&self, since_timestamp: &str) -> Result<Vec<InterventionStats>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let ts: chrono::DateTime<chrono::FixedOffset> = since_timestamp.parse()
            .unwrap_or_else(|_| chrono::Utc::now().fixed_offset());
        let rows = qontinui_db::queries::ui_bridge::get_intervention_effectiveness()
            .bind(&conn, &ts)
            .all()
            .await
            .map_err(|e| format!("PG get_intervention_effectiveness: {}", e))?;

        Ok(rows.into_iter().map(|r| InterventionStats {
            intervention_action: r.intervention_action,
            total: r.total,
            successful: r.successes,
            success_rate: r.rate,
        }).collect())
    }

    /// State coverage for a task run.
    pub async fn get_state_coverage(&self, task_run_id: i64) -> Result<Vec<String>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::ui_bridge::get_state_coverage()
            .bind(&conn, &task_run_id)
            .all()
            .await
            .map_err(|e| format!("PG get_state_coverage: {}", e))?;
        Ok(rows)
    }

    /// Unannotated high-interaction elements (annotation gaps).
    pub async fn get_unannotated_high_interaction_elements(&self, min_interactions: i64) -> Result<Vec<AnnotationGap>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = qontinui_db::queries::ui_bridge::get_unannotated_high_interaction_elements()
            .bind(&conn, &min_interactions)
            .all()
            .await
            .map_err(|e| format!("PG get_unannotated_elements: {}", e))?;

        Ok(rows.into_iter().map(|r| AnnotationGap {
            element_id: r.element_id,
            interaction_count: r.interaction_count,
            success_rate: r.success_rate,
            element_type: None, // Not queried in the aggregate
            label: None,
        }).collect())
    }
}
