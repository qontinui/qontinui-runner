//! Storage layer for the Trace API (Section 5b).
//!
//! All access goes through raw `tokio_postgres` queries via `PgDb::pool`
//! (see `database/pg/recordings.rs` for the established pattern). We avoid
//! the Clorinde-generated bindings here on purpose: the queries we depend on
//! reference the `recording_session_id` and `caused_by_event_id` columns
//! added by the `section_5b_01_ui_bridge_causal_columns` migration, and that
//! migration is applied on a different machine ("spaceship"). Until that
//! schema regen lands locally, Clorinde would refuse to compile against the
//! cached `schema.pg.sql.generated`. Raw SQL sidesteps the validation gate.
//!
//! After the migration applies and the cached schema catches up, the
//! Clorinde queries declared in `queries/ui_bridge.sql` (under the
//! `SECTION 5B` block) can replace the bodies here.

use std::sync::Arc;

use tokio_postgres::types::ToSql;

use crate::database::pg::PgDb;

use super::types::{InboundRecorderEvent, RecordingSessionMeta, TraceEvent};

/// Insert a single causal event row into `ui_bridge_events`. Returns the new
/// row id assigned by Postgres. Errors carry a short human-readable string so
/// handlers can wrap them in the standard `TraceError` envelope.
pub async fn insert_causal_event(
    pg: &Arc<PgDb>,
    event: &InboundRecorderEvent,
    recording_session_id: &str,
    sequence: i64,
    timestamp_ms: i64,
    caused_by_event_id: Option<i64>,
) -> Result<i64, String> {
    let conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {}", e))?;

    let event_type: String = event
        .event_type
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let success: bool = event.success.unwrap_or(true);

    let params_json: Option<String> = event
        .params
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "null".to_string()));
    let result_json: Option<String> = event
        .result
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "null".to_string()));
    let metadata_json: Option<String> = event
        .metadata
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "null".to_string()));

    let row = conn
        .query_one(
            r#"
            INSERT INTO ui_bridge_events
                (task_run_id, timestamp, sequence, event_type, element_id,
                 state_id, transition_id, action, params, result,
                 duration_ms, success, error_message, metadata,
                 recording_session_id, caused_by_event_id)
            VALUES ($1, $2, $3, $4, $5,
                    $6, $7, $8, $9, $10,
                    $11, $12, $13, $14,
                    $15, $16)
            RETURNING id
            "#,
            &[
                &event.task_run_id as &(dyn ToSql + Sync),
                &timestamp_ms as &(dyn ToSql + Sync),
                &sequence as &(dyn ToSql + Sync),
                &event_type as &(dyn ToSql + Sync),
                &event.element_id as &(dyn ToSql + Sync),
                &event.state_id as &(dyn ToSql + Sync),
                &event.transition_id as &(dyn ToSql + Sync),
                &event.action as &(dyn ToSql + Sync),
                &params_json as &(dyn ToSql + Sync),
                &result_json as &(dyn ToSql + Sync),
                &event.duration_ms as &(dyn ToSql + Sync),
                &success as &(dyn ToSql + Sync),
                &event.error_message as &(dyn ToSql + Sync),
                &metadata_json as &(dyn ToSql + Sync),
                &recording_session_id as &(dyn ToSql + Sync),
                &caused_by_event_id as &(dyn ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG insert_causal_event: {}", e))?;

    let id: i64 = row.get(0);
    Ok(id)
}

/// List recording sessions, newest first, capped at `limit`. Sessions with
/// no events are not returned (the GROUP BY excludes them).
pub async fn list_recording_sessions(
    pg: &Arc<PgDb>,
    limit: i64,
) -> Result<Vec<RecordingSessionMeta>, String> {
    let conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {}", e))?;

    let rows = conn
        .query(
            r#"
            SELECT recording_session_id,
                   COUNT(*)::bigint AS event_count,
                   MIN(timestamp)::bigint AS started_at,
                   MAX(timestamp)::bigint AS ended_at
            FROM ui_bridge_events
            WHERE recording_session_id IS NOT NULL
            GROUP BY recording_session_id
            ORDER BY MIN(timestamp) DESC
            LIMIT $1
            "#,
            &[&limit],
        )
        .await
        .map_err(|e| format!("PG list_recording_sessions: {}", e))?;

    Ok(rows
        .iter()
        .map(|r| RecordingSessionMeta {
            recording_session_id: r.get(0),
            event_count: r.get(1),
            started_at: r.get(2),
            ended_at: r.get(3),
        })
        .collect())
}

/// Read every event belonging to a recording session, ordered by sequence.
pub async fn get_recording_session(
    pg: &Arc<PgDb>,
    recording_session_id: &str,
) -> Result<Vec<TraceEvent>, String> {
    let conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {}", e))?;

    let rows = conn
        .query(
            r#"
            SELECT id, task_run_id, timestamp, sequence, event_type,
                   element_id, state_id, transition_id, action, params,
                   result, duration_ms, success, error_message, metadata,
                   recording_session_id, caused_by_event_id
            FROM ui_bridge_events
            WHERE recording_session_id = $1
            ORDER BY sequence ASC
            "#,
            &[&recording_session_id],
        )
        .await
        .map_err(|e| format!("PG get_recording_session: {}", e))?;

    Ok(rows.iter().map(row_to_trace_event).collect())
}

/// Walk the `caused_by_event_id` chain backwards starting from `event_id`.
/// Returns rows in root-first order — i.e. the requested event is the last
/// entry. Returns an empty vector if `event_id` is not present.
pub async fn get_causal_chain(
    pg: &Arc<PgDb>,
    event_id: i64,
) -> Result<Vec<TraceEvent>, String> {
    let conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {}", e))?;

    let rows = conn
        .query(
            r#"
            WITH RECURSIVE chain AS (
                SELECT id, task_run_id, timestamp, sequence, event_type,
                       element_id, state_id, transition_id, action, params,
                       result, duration_ms, success, error_message, metadata,
                       recording_session_id, caused_by_event_id, 0 AS depth
                FROM ui_bridge_events
                WHERE id = $1
                UNION ALL
                SELECT p.id, p.task_run_id, p.timestamp, p.sequence, p.event_type,
                       p.element_id, p.state_id, p.transition_id, p.action, p.params,
                       p.result, p.duration_ms, p.success, p.error_message, p.metadata,
                       p.recording_session_id, p.caused_by_event_id, c.depth + 1
                FROM ui_bridge_events p
                JOIN chain c ON p.id = c.caused_by_event_id
            )
            SELECT id, task_run_id, timestamp, sequence, event_type,
                   element_id, state_id, transition_id, action, params,
                   result, duration_ms, success, error_message, metadata,
                   recording_session_id, caused_by_event_id
            FROM chain
            ORDER BY depth DESC
            "#,
            &[&event_id],
        )
        .await
        .map_err(|e| format!("PG get_causal_chain: {}", e))?;

    Ok(rows.iter().map(row_to_trace_event).collect())
}

/// Map a `tokio_postgres::Row` (selected with the column order the helpers
/// above use) into a `TraceEvent`.
fn row_to_trace_event(row: &tokio_postgres::Row) -> TraceEvent {
    TraceEvent {
        id: row.get(0),
        task_run_id: row.get(1),
        timestamp: row.get(2),
        sequence: row.get(3),
        event_type: row.get(4),
        element_id: row.get(5),
        state_id: row.get(6),
        transition_id: row.get(7),
        action: row.get(8),
        params: row.get(9),
        result: row.get(10),
        duration_ms: row.get(11),
        success: row.get(12),
        error_message: row.get(13),
        metadata: row.get(14),
        recording_session_id: row.get(15),
        caused_by_event_id: row.get(16),
    }
}
