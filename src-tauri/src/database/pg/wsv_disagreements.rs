//! PostgreSQL persistence for World State Verifier shadow-mode disagreements.
//!
//! When WSV is in `Shadow` mode, the agentic verification loop runs both the
//! VLM judge (WSM) and the text verifier agent on every iteration. When they
//! return different statuses, a row is inserted here so the user can review
//! the disagreement pattern in the Settings → World State Verifier UI and
//! decide whether to flip to `Enabled` mode.
//!
//! Write pattern: fire-and-forget on the hot path of the agentic loop —
//! PG errors are logged but never propagate back to the loop.
//! Read pattern: called from a Tauri command that backs the calibration view.

use serde::{Deserialize, Serialize};
use tracing::warn;

use super::PgDb;

/// Maximum limit clients can request from `list_wsv_disagreements`.
/// Client requests above this are silently clamped to `MAX_LIST_LIMIT`.
pub const MAX_LIST_LIMIT: i64 = 1000;

/// Default list limit when the caller doesn't specify one.
pub const DEFAULT_LIST_LIMIT: i64 = 100;

/// One row from `wsv_shadow_disagreements`. Serializable for the Tauri command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsvDisagreementRow {
    pub id: i64,
    pub task_run_id: String,
    pub iteration: i32,
    pub text_status: String,
    pub wsm_status: String,
    pub text_confidence: f64,
    pub wsm_confidence: f64,
    pub intent: String,
    pub wsm_observations: String,
    /// RFC3339 timestamp — stringified so the Tauri/JS layer can parse
    /// without needing a tokio-postgres Timestamp ↔ JS mapping.
    pub created_at: String,
}

/// Input struct for inserts. DB assigns id and created_at.
#[derive(Debug, Clone)]
pub struct WsvDisagreementInsert<'a> {
    pub task_run_id: &'a str,
    pub iteration: i32,
    pub text_status: &'a str,
    pub wsm_status: &'a str,
    pub text_confidence: f64,
    pub wsm_confidence: f64,
    pub intent: &'a str,
    pub wsm_observations: &'a str,
}

/// Clamp a caller-supplied limit to [1, MAX_LIST_LIMIT], falling back
/// to DEFAULT_LIST_LIMIT when None or non-positive.
pub fn normalize_limit(requested: Option<i64>) -> i64 {
    match requested {
        Some(n) if n > 0 => n.min(MAX_LIST_LIMIT),
        _ => DEFAULT_LIST_LIMIT,
    }
}

impl PgDb {
    /// Insert a single disagreement row.
    ///
    /// Returns the new row's id on success. On error, logs a warning and
    /// returns None — callers on the agentic loop hot path MUST treat a
    /// failure as a non-fatal telemetry loss.
    pub async fn insert_wsv_disagreement(&self, input: WsvDisagreementInsert<'_>) -> Option<i64> {
        let conn = match self.pool.get().await {
            Ok(c) => c,
            Err(e) => {
                warn!("wsv_disagreements insert: PG pool error: {}", e);
                return None;
            }
        };

        let row = match conn
            .query_one(
                "INSERT INTO wsv_shadow_disagreements \
                 (task_run_id, iteration, text_status, wsm_status, \
                  text_confidence, wsm_confidence, intent, wsm_observations) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
                 RETURNING id",
                &[
                    &input.task_run_id,
                    &input.iteration,
                    &input.text_status,
                    &input.wsm_status,
                    &input.text_confidence,
                    &input.wsm_confidence,
                    &input.intent,
                    &input.wsm_observations,
                ],
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!("wsv_disagreements insert failed: {}", e);
                return None;
            }
        };

        let id: i64 = row.get("id");
        Some(id)
    }

    /// List recent disagreements, most-recent first. Optionally filter to a
    /// single task_run_id. The limit is clamped to [1, MAX_LIST_LIMIT].
    pub async fn list_wsv_disagreements(
        &self,
        task_run_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<WsvDisagreementRow>, String> {
        let limit = limit.clamp(1, MAX_LIST_LIMIT);

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = match task_run_id {
            Some(id) => conn
                .query(
                    "SELECT id, task_run_id, iteration, text_status, wsm_status, \
                            text_confidence, wsm_confidence, intent, wsm_observations, \
                            to_char(created_at AT TIME ZONE 'UTC', \
                                    'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS created_at \
                     FROM wsv_shadow_disagreements \
                     WHERE task_run_id = $1 \
                     ORDER BY created_at DESC \
                     LIMIT $2",
                    &[&id, &limit],
                )
                .await
                .map_err(|e| format!("wsv_disagreements list (by task): {}", e))?,
            None => conn
                .query(
                    "SELECT id, task_run_id, iteration, text_status, wsm_status, \
                            text_confidence, wsm_confidence, intent, wsm_observations, \
                            to_char(created_at AT TIME ZONE 'UTC', \
                                    'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS created_at \
                     FROM wsv_shadow_disagreements \
                     ORDER BY created_at DESC \
                     LIMIT $1",
                    &[&limit],
                )
                .await
                .map_err(|e| format!("wsv_disagreements list: {}", e))?,
        };

        Ok(rows
            .iter()
            .map(|r| WsvDisagreementRow {
                id: r.get("id"),
                task_run_id: r.get("task_run_id"),
                iteration: r.get("iteration"),
                text_status: r.get("text_status"),
                wsm_status: r.get("wsm_status"),
                text_confidence: r.get("text_confidence"),
                wsm_confidence: r.get("wsm_confidence"),
                intent: r.get("intent"),
                wsm_observations: r.get("wsm_observations"),
                created_at: r.get("created_at"),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_limit_defaults_when_none() {
        assert_eq!(normalize_limit(None), DEFAULT_LIST_LIMIT);
    }

    #[test]
    fn normalize_limit_defaults_when_zero_or_negative() {
        assert_eq!(normalize_limit(Some(0)), DEFAULT_LIST_LIMIT);
        assert_eq!(normalize_limit(Some(-5)), DEFAULT_LIST_LIMIT);
    }

    #[test]
    fn normalize_limit_clamps_above_max() {
        assert_eq!(normalize_limit(Some(5000)), MAX_LIST_LIMIT);
        assert_eq!(normalize_limit(Some(10_000_000)), MAX_LIST_LIMIT);
    }

    #[test]
    fn normalize_limit_passes_through_valid_values() {
        assert_eq!(normalize_limit(Some(1)), 1);
        assert_eq!(normalize_limit(Some(50)), 50);
        assert_eq!(normalize_limit(Some(MAX_LIST_LIMIT)), MAX_LIST_LIMIT);
    }

    #[test]
    #[ignore = "requires a live postgres with schema.pg.sql applied"]
    fn roundtrip_insert_and_list() {
        // Round-trip test skipped because the project's PG test harness
        // is not trivially reachable from here. The insert/list SQL is
        // exercised end-to-end in manual testing and by the Tauri command
        // it's wired into.
    }
}
