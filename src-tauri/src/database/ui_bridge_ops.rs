//! CRUD operations for UI Bridge persistence tables.
//!
//! Covers: ui_bridge_elements, ui_bridge_events, ui_bridge_navigation_history,
//! and stall_events.

use rusqlite::{params, Connection};
use std::time::{SystemTime, UNIX_EPOCH};

// ============================================================================
// Struct definitions
// ============================================================================

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UiBridgeElement {
    pub id: i64,
    pub task_run_id: Option<i64>,
    pub timestamp: i64,
    pub element_id: String,
    pub tag_name: Option<String>,
    pub element_type: Option<String>,
    pub bounds: Option<String>,
    pub visible: bool,
    pub enabled: bool,
    pub focused: bool,
    pub value: Option<String>,
    pub text_content: Option<String>,
    pub label: Option<String>,
    pub parent_id: Option<String>,
    pub children: Option<String>,
    pub actions: Option<String>,
    pub metadata: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UiBridgeEvent {
    pub id: i64,
    pub task_run_id: Option<i64>,
    pub timestamp: i64,
    pub sequence: i64,
    pub event_type: String,
    pub element_id: Option<String>,
    pub state_id: Option<String>,
    pub transition_id: Option<String>,
    pub action: Option<String>,
    pub params: Option<String>,
    pub result: Option<String>,
    pub duration_ms: Option<f64>,
    pub success: bool,
    pub error_message: Option<String>,
    pub metadata: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UiBridgeNavigationEvent {
    pub id: i64,
    pub task_run_id: Option<i64>,
    pub timestamp: i64,
    pub target_states: String,
    pub path_found: bool,
    pub transitions_planned: Option<String>,
    pub transitions_executed: Option<String>,
    pub total_cost: Option<f64>,
    pub duration_ms: Option<f64>,
    pub success: bool,
    pub final_active_states: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StallEvent {
    pub id: String,
    pub task_run_id: String,
    pub iteration: i64,
    pub pattern_type: String,
    pub pattern_details: Option<String>,
    pub action_count: Option<i64>,
    pub intervention_action: Option<String>,
    pub intervention_result: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ElementReliability {
    pub element_id: String,
    pub total_interactions: i64,
    pub successful_interactions: i64,
    pub success_rate: f64,
    pub last_failure_reason: Option<String>,
    pub flaky: bool,
    pub recommended_confidence: f64,
}

fn now_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

// ============================================================================
// ui_bridge_elements
// ============================================================================

/// Insert an element snapshot captured during a UI Bridge interaction.
///
/// Note: `parent_id`, `children`, and `focused` columns from the schema are
/// intentionally omitted — element hierarchy data is not available at
/// individual action capture time. These fields are populated during
/// full snapshot captures via the exploration engine instead.
pub fn insert_element_snapshot(
    conn: &Connection,
    task_run_id: Option<i64>,
    element_id: &str,
    tag_name: Option<&str>,
    element_type: Option<&str>,
    bounds: Option<&str>,
    visible: bool,
    enabled: bool,
    value: Option<&str>,
    text_content: Option<&str>,
    label: Option<&str>,
    actions: Option<&str>,
    metadata: Option<&str>,
) -> Result<i64, String> {
    let ts = now_epoch_ms();

    conn.execute(
        r#"INSERT INTO ui_bridge_elements
           (task_run_id, timestamp, element_id, tag_name, element_type,
            bounds, visible, enabled, focused, value, text_content,
            label, actions, metadata)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9, ?10, ?11, ?12, ?13)"#,
        params![
            task_run_id,
            ts,
            element_id,
            tag_name,
            element_type,
            bounds,
            visible as i32,
            enabled as i32,
            value,
            text_content,
            label,
            actions,
            metadata,
        ],
    )
    .map_err(|e| format!("Failed to insert element snapshot: {e}"))?;

    Ok(conn.last_insert_rowid())
}

// ============================================================================
// ui_bridge_events
// ============================================================================

/// Insert a UI Bridge event (action executed, state changed, etc.).
pub fn insert_ui_bridge_event(
    conn: &Connection,
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
    let ts = now_epoch_ms();

    conn.execute(
        r#"INSERT INTO ui_bridge_events
           (task_run_id, timestamp, sequence, event_type, element_id,
            state_id, transition_id, action, params, result,
            duration_ms, success, error_message, metadata)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"#,
        params![
            task_run_id,
            ts,
            sequence,
            event_type,
            element_id,
            state_id,
            transition_id,
            action,
            params,
            result,
            duration_ms,
            success as i32,
            error_message,
            metadata,
        ],
    )
    .map_err(|e| format!("Failed to insert UI Bridge event: {e}"))?;

    Ok(conn.last_insert_rowid())
}

/// Get all events for a task run, ordered by sequence.
pub fn get_element_interactions(
    conn: &Connection,
    task_run_id: i64,
) -> Result<Vec<UiBridgeEvent>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT id, task_run_id, timestamp, sequence, event_type,
                      element_id, state_id, transition_id, action, params,
                      result, duration_ms, success, error_message, metadata
               FROM ui_bridge_events
               WHERE task_run_id = ?1
               ORDER BY sequence ASC"#,
        )
        .map_err(|e| format!("Failed to prepare element interactions query: {e}"))?;

    let results = stmt
        .query_map(params![task_run_id], |row| {
            Ok(UiBridgeEvent {
                id: row.get(0)?,
                task_run_id: row.get(1)?,
                timestamp: row.get(2)?,
                sequence: row.get(3)?,
                event_type: row.get(4)?,
                element_id: row.get(5)?,
                state_id: row.get(6)?,
                transition_id: row.get(7)?,
                action: row.get(8)?,
                params: row.get(9)?,
                result: row.get(10)?,
                duration_ms: row.get(11)?,
                success: row.get::<_, i32>(12)? != 0,
                error_message: row.get(13)?,
                metadata: row.get(14)?,
            })
        })
        .map_err(|e| format!("Failed to query element interactions: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

/// Get cross-run interaction history for a single element.
pub fn get_element_history(
    conn: &Connection,
    element_id: &str,
    limit: i64,
) -> Result<Vec<UiBridgeEvent>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT id, task_run_id, timestamp, sequence, event_type,
                      element_id, state_id, transition_id, action, params,
                      result, duration_ms, success, error_message, metadata
               FROM ui_bridge_events
               WHERE element_id = ?1
               ORDER BY timestamp DESC
               LIMIT ?2"#,
        )
        .map_err(|e| format!("Failed to prepare element history query: {e}"))?;

    let results = stmt
        .query_map(params![element_id, limit], |row| {
            Ok(UiBridgeEvent {
                id: row.get(0)?,
                task_run_id: row.get(1)?,
                timestamp: row.get(2)?,
                sequence: row.get(3)?,
                event_type: row.get(4)?,
                element_id: row.get(5)?,
                state_id: row.get(6)?,
                transition_id: row.get(7)?,
                action: row.get(8)?,
                params: row.get(9)?,
                result: row.get(10)?,
                duration_ms: row.get(11)?,
                success: row.get::<_, i32>(12)? != 0,
                error_message: row.get(13)?,
                metadata: row.get(14)?,
            })
        })
        .map_err(|e| format!("Failed to query element history: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

/// Get elements with high failure rates across runs.
///
/// Returns elements that have at least `min_interactions` total events
/// and a success rate at or below `max_success_rate`.
pub fn get_flaky_elements(
    conn: &Connection,
    min_interactions: i64,
    max_success_rate: f64,
) -> Result<Vec<ElementReliability>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT
                 element_id,
                 COUNT(*) as total,
                 SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) as successes,
                 CAST(SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) AS REAL) / COUNT(*) as rate
               FROM ui_bridge_events
               WHERE element_id IS NOT NULL
                 AND event_type = 'action_executed'
               GROUP BY element_id
               HAVING COUNT(*) >= ?1
                 AND rate <= ?2
               ORDER BY rate ASC"#,
        )
        .map_err(|e| format!("Failed to prepare flaky elements query: {e}"))?;

    let results = stmt
        .query_map(params![min_interactions, max_success_rate], |row| {
            let element_id: String = row.get(0)?;
            let total: i64 = row.get(1)?;
            let successes: i64 = row.get(2)?;
            let rate: f64 = row.get(3)?;
            Ok(ElementReliability {
                element_id,
                total_interactions: total,
                successful_interactions: successes,
                success_rate: rate,
                last_failure_reason: None, // filled below
                flaky: rate < 0.95,
                recommended_confidence: rate.max(0.1), // floor at 0.1
            })
        })
        .map_err(|e| format!("Failed to query flaky elements: {e}"))?
        .filter_map(|r| r.ok())
        .collect::<Vec<_>>();

    // Fetch last failure reason for each flaky element
    let mut enriched = results;
    for elem in &mut enriched {
        if let Ok(reason) = conn.query_row(
            r#"SELECT error_message FROM ui_bridge_events
               WHERE element_id = ?1 AND success = 0 AND error_message IS NOT NULL
               ORDER BY timestamp DESC LIMIT 1"#,
            params![elem.element_id],
            |row| row.get::<_, String>(0),
        ) {
            elem.last_failure_reason = Some(reason);
        }
    }

    Ok(enriched)
}

/// Get reliability data for a single element across all runs.
pub fn get_element_reliability(
    conn: &Connection,
    element_id: &str,
) -> Result<Option<ElementReliability>, String> {
    let result = conn.query_row(
        r#"SELECT
             COUNT(*) as total,
             COALESCE(SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END), 0) as successes
           FROM ui_bridge_events
           WHERE element_id = ?1
             AND event_type = 'action_executed'"#,
        params![element_id],
        |row| {
            let total: i64 = row.get(0)?;
            let successes: i64 = row.get(1)?;
            Ok((total, successes))
        },
    );

    match result {
        Ok((total, _)) if total == 0 => Ok(None),
        Ok((total, successes)) => {
            let rate = successes as f64 / total as f64;

            let last_failure = conn
                .query_row(
                    r#"SELECT error_message FROM ui_bridge_events
                       WHERE element_id = ?1 AND success = 0 AND error_message IS NOT NULL
                       ORDER BY timestamp DESC LIMIT 1"#,
                    params![element_id],
                    |row| row.get::<_, String>(0),
                )
                .ok();

            Ok(Some(ElementReliability {
                element_id: element_id.to_string(),
                total_interactions: total,
                successful_interactions: successes,
                success_rate: rate,
                last_failure_reason: last_failure,
                flaky: rate < 0.95,
                recommended_confidence: rate.max(0.1),
            }))
        }
        Err(e) => Err(format!("Failed to query element reliability: {e}")),
    }
}

// ============================================================================
// ui_bridge_navigation_history
// ============================================================================

/// Insert a navigation event (path execution record).
pub fn insert_navigation_event(
    conn: &Connection,
    task_run_id: Option<i64>,
    target_states: &str,
    path_found: bool,
    transitions_planned: Option<&str>,
    transitions_executed: Option<&str>,
    total_cost: Option<f64>,
    duration_ms: Option<f64>,
    success: bool,
    final_active_states: Option<&str>,
    error_message: Option<&str>,
) -> Result<i64, String> {
    let ts = now_epoch_ms();

    conn.execute(
        r#"INSERT INTO ui_bridge_navigation_history
           (task_run_id, timestamp, target_states, path_found,
            transitions_planned, transitions_executed, total_cost,
            duration_ms, success, final_active_states, error_message)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"#,
        params![
            task_run_id,
            ts,
            target_states,
            path_found as i32,
            transitions_planned,
            transitions_executed,
            total_cost,
            duration_ms,
            success as i32,
            final_active_states,
            error_message,
        ],
    )
    .map_err(|e| format!("Failed to insert navigation event: {e}"))?;

    Ok(conn.last_insert_rowid())
}

// ============================================================================
// stall_events
// ============================================================================

/// Insert a stall detection event.
pub fn insert_stall_event(
    conn: &Connection,
    task_run_id: &str,
    iteration: i64,
    pattern_type: &str,
    pattern_details: Option<&str>,
    action_count: Option<i64>,
    intervention_action: Option<&str>,
    intervention_result: Option<&str>,
) -> Result<String, String> {
    // stall_events uses hex(randomblob(16)) for ID, but we generate here for return
    let id = format!(
        "{:032x}",
        u128::from_be_bytes(uuid::Uuid::new_v4().as_bytes().clone())
    );

    conn.execute(
        r#"INSERT INTO stall_events
           (id, task_run_id, iteration, pattern_type, pattern_details,
            action_count, intervention_action, intervention_result)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"#,
        params![
            id,
            task_run_id,
            iteration,
            pattern_type,
            pattern_details,
            action_count,
            intervention_action,
            intervention_result,
        ],
    )
    .map_err(|e| format!("Failed to insert stall event: {e}"))?;

    Ok(id)
}

/// Get stall events for a task run.
pub fn get_stall_events(
    conn: &Connection,
    task_run_id: &str,
) -> Result<Vec<StallEvent>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT id, task_run_id, iteration, pattern_type, pattern_details,
                      action_count, intervention_action, intervention_result, created_at
               FROM stall_events
               WHERE task_run_id = ?1
               ORDER BY created_at ASC"#,
        )
        .map_err(|e| format!("Failed to prepare stall events query: {e}"))?;

    let results = stmt
        .query_map(params![task_run_id], |row| {
            Ok(StallEvent {
                id: row.get(0)?,
                task_run_id: row.get(1)?,
                iteration: row.get(2)?,
                pattern_type: row.get(3)?,
                pattern_details: row.get(4)?,
                action_count: row.get(5)?,
                intervention_action: row.get(6)?,
                intervention_result: row.get(7)?,
                created_at: row.get(8)?,
            })
        })
        .map_err(|e| format!("Failed to query stall events: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

/// Get stall event patterns for a workflow (cross-run analysis).
/// Joins stall_events with task_runs to filter by workflow name.
pub fn get_stall_patterns_by_workflow(
    conn: &Connection,
    workflow_name: &str,
) -> Result<Vec<StallEvent>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT se.id, se.task_run_id, se.iteration, se.pattern_type,
                      se.pattern_details, se.action_count, se.intervention_action,
                      se.intervention_result, se.created_at
               FROM stall_events se
               JOIN task_runs tr ON se.task_run_id = CAST(tr.id AS TEXT)
               WHERE tr.workflow_name = ?1
               ORDER BY se.created_at DESC
               LIMIT 100"#,
        )
        .map_err(|e| format!("Failed to prepare stall patterns query: {e}"))?;

    let results = stmt
        .query_map(params![workflow_name], |row| {
            Ok(StallEvent {
                id: row.get(0)?,
                task_run_id: row.get(1)?,
                iteration: row.get(2)?,
                pattern_type: row.get(3)?,
                pattern_details: row.get(4)?,
                action_count: row.get(5)?,
                intervention_action: row.get(6)?,
                intervention_result: row.get(7)?,
                created_at: row.get(8)?,
            })
        })
        .map_err(|e| format!("Failed to query stall patterns: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

// ============================================================================
// Analytics: Selector & Action Performance (Phase 1)
// ============================================================================

/// A single time-window bucket in a decay curve.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DecayCurveBucket {
    pub bucket: i64,
    pub total: i64,
    pub successes: i64,
    pub success_rate: f64,
}

/// Action type performance baseline.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActionBaseline {
    pub action: String,
    pub count: i64,
    pub avg_duration_ms: Option<f64>,
    pub min_duration_ms: Option<f64>,
    pub max_duration_ms: Option<f64>,
    pub success_rate: f64,
}

/// A cluster of failures grouped by error message.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FailureCluster {
    pub error_message: String,
    pub count: i64,
    pub affected_elements: String,
}

/// Element with its bounds and success rate for fragility mapping.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ElementFragility {
    pub element_id: String,
    pub bounds: Option<String>,
    pub interaction_count: i64,
    pub success_rate: f64,
}

/// Automation regression: element+action that degraded between runs.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AutomationRegression {
    pub element_id: String,
    pub action: String,
    pub prior_success_rate: f64,
    pub recent_success_rate: f64,
    pub delta: f64,
}

/// Stall frequency grouped by pattern type.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StallFrequency {
    pub pattern_type: String,
    pub count: i64,
}

/// Intervention effectiveness stats.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InterventionStats {
    pub intervention_action: String,
    pub total: i64,
    pub successful: i64,
    pub success_rate: f64,
}

/// Element success rate decay curve over time windows.
///
/// Each bucket represents a time window of `window_ms` milliseconds.
/// Returns `num_windows` most recent buckets, newest first.
pub fn get_element_decay_curve(
    conn: &Connection,
    element_id: &str,
    window_ms: i64,
    num_windows: i64,
) -> Result<Vec<DecayCurveBucket>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT (timestamp / ?2) AS bucket,
                      COUNT(*) AS total,
                      COALESCE(SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END), 0) AS successes,
                      CAST(COALESCE(SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END), 0) AS REAL) / COUNT(*) AS rate
               FROM ui_bridge_events
               WHERE element_id = ?1 AND event_type = 'action_executed'
               GROUP BY bucket
               ORDER BY bucket DESC
               LIMIT ?3"#,
        )
        .map_err(|e| format!("Failed to prepare decay curve query: {e}"))?;

    let results = stmt
        .query_map(params![element_id, window_ms, num_windows], |row| {
            Ok(DecayCurveBucket {
                bucket: row.get(0)?,
                total: row.get(1)?,
                successes: row.get(2)?,
                success_rate: row.get(3)?,
            })
        })
        .map_err(|e| format!("Failed to query decay curve: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

/// Per-action-type performance baselines (latency + success rate).
pub fn get_action_latency_baselines(
    conn: &Connection,
    since_epoch_ms: i64,
) -> Result<Vec<ActionBaseline>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT action,
                      COUNT(*) AS cnt,
                      AVG(duration_ms),
                      MIN(duration_ms),
                      MAX(duration_ms),
                      CAST(COALESCE(SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END), 0) AS REAL) / COUNT(*) AS rate
               FROM ui_bridge_events
               WHERE event_type = 'action_executed'
                 AND action IS NOT NULL
                 AND timestamp >= ?1
               GROUP BY action
               ORDER BY cnt DESC
               LIMIT 50"#,
        )
        .map_err(|e| format!("Failed to prepare action baselines query: {e}"))?;

    let results = stmt
        .query_map(params![since_epoch_ms], |row| {
            Ok(ActionBaseline {
                action: row.get(0)?,
                count: row.get(1)?,
                avg_duration_ms: row.get(2)?,
                min_duration_ms: row.get(3)?,
                max_duration_ms: row.get(4)?,
                success_rate: row.get(5)?,
            })
        })
        .map_err(|e| format!("Failed to query action baselines: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

/// Cluster failures by error message, with affected element IDs.
pub fn get_failure_taxonomy(
    conn: &Connection,
    since_epoch_ms: i64,
    limit: i64,
) -> Result<Vec<FailureCluster>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT error_message,
                      COUNT(*) AS freq,
                      GROUP_CONCAT(DISTINCT element_id) AS elements
               FROM ui_bridge_events
               WHERE success = 0
                 AND error_message IS NOT NULL
                 AND timestamp >= ?1
               GROUP BY error_message
               ORDER BY freq DESC
               LIMIT ?2"#,
        )
        .map_err(|e| format!("Failed to prepare failure taxonomy query: {e}"))?;

    let results = stmt
        .query_map(params![since_epoch_ms, limit], |row| {
            Ok(FailureCluster {
                error_message: row.get(0)?,
                count: row.get(1)?,
                affected_elements: row.get::<_, String>(2).unwrap_or_default(),
            })
        })
        .map_err(|e| format!("Failed to query failure taxonomy: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

/// Element fragility: join bounds with success rates for heatmap visualization.
pub fn get_element_fragility_by_region(
    conn: &Connection,
    since_epoch_ms: i64,
) -> Result<Vec<ElementFragility>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT ev.element_id,
                      el.bounds,
                      COUNT(*) AS cnt,
                      CAST(COALESCE(SUM(CASE WHEN ev.success = 1 THEN 1 ELSE 0 END), 0) AS REAL) / COUNT(*) AS rate
               FROM ui_bridge_events ev
               LEFT JOIN ui_bridge_elements el ON ev.element_id = el.element_id
               WHERE ev.event_type = 'action_executed'
                 AND ev.element_id IS NOT NULL
                 AND ev.timestamp >= ?1
               GROUP BY ev.element_id
               HAVING cnt >= 3
               ORDER BY rate ASC
               LIMIT 100"#,
        )
        .map_err(|e| format!("Failed to prepare fragility query: {e}"))?;

    let results = stmt
        .query_map(params![since_epoch_ms], |row| {
            Ok(ElementFragility {
                element_id: row.get(0)?,
                bounds: row.get(1)?,
                interaction_count: row.get(2)?,
                success_rate: row.get(3)?,
            })
        })
        .map_err(|e| format!("Failed to query element fragility: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

// ============================================================================
// Analytics: Cross-Run Quality (Phase 2)
// ============================================================================

/// Detect automation regressions: elements that succeeded in older runs but fail in recent ones.
pub fn get_automation_regressions(
    conn: &Connection,
    since_epoch_ms: i64,
    limit: i64,
) -> Result<Vec<AutomationRegression>, String> {
    // Compare recent half vs older half of events per element+action pair
    let mut stmt = conn
        .prepare(
            r#"WITH element_splits AS (
                 SELECT element_id, action,
                        timestamp,
                        success,
                        NTILE(2) OVER (PARTITION BY element_id, action ORDER BY timestamp) AS half
                 FROM ui_bridge_events
                 WHERE event_type = 'action_executed'
                   AND element_id IS NOT NULL
                   AND action IS NOT NULL
                   AND timestamp >= ?1
               )
               SELECT element_id, action,
                      CAST(SUM(CASE WHEN half = 1 THEN success ELSE 0 END) AS REAL) /
                        MAX(1, SUM(CASE WHEN half = 1 THEN 1 ELSE 0 END)) AS prior_rate,
                      CAST(SUM(CASE WHEN half = 2 THEN success ELSE 0 END) AS REAL) /
                        MAX(1, SUM(CASE WHEN half = 2 THEN 1 ELSE 0 END)) AS recent_rate
               FROM element_splits
               GROUP BY element_id, action
               HAVING COUNT(*) >= 4
                 AND recent_rate < prior_rate - 0.1
               ORDER BY (prior_rate - recent_rate) DESC
               LIMIT ?2"#,
        )
        .map_err(|e| format!("Failed to prepare regressions query: {e}"))?;

    let results = stmt
        .query_map(params![since_epoch_ms, limit], |row| {
            let prior: f64 = row.get(2)?;
            let recent: f64 = row.get(3)?;
            Ok(AutomationRegression {
                element_id: row.get(0)?,
                action: row.get(1)?,
                prior_success_rate: prior,
                recent_success_rate: recent,
                delta: recent - prior,
            })
        })
        .map_err(|e| format!("Failed to query regressions: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

/// Stall frequency grouped by pattern type.
pub fn get_stall_frequency(
    conn: &Connection,
    since_timestamp: &str,
) -> Result<Vec<StallFrequency>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT pattern_type, COUNT(*) AS cnt
               FROM stall_events
               WHERE created_at >= ?1
               GROUP BY pattern_type
               ORDER BY cnt DESC"#,
        )
        .map_err(|e| format!("Failed to prepare stall frequency query: {e}"))?;

    let results = stmt
        .query_map(params![since_timestamp], |row| {
            Ok(StallFrequency {
                pattern_type: row.get(0)?,
                count: row.get(1)?,
            })
        })
        .map_err(|e| format!("Failed to query stall frequency: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

/// Intervention effectiveness: success rate per intervention action type.
pub fn get_intervention_effectiveness(
    conn: &Connection,
    since_timestamp: &str,
) -> Result<Vec<InterventionStats>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT COALESCE(intervention_action, 'none'),
                      COUNT(*) AS total,
                      SUM(CASE WHEN intervention_result = 'success' THEN 1 ELSE 0 END) AS successful
               FROM stall_events
               WHERE created_at >= ?1
                 AND intervention_action IS NOT NULL
               GROUP BY intervention_action
               ORDER BY total DESC"#,
        )
        .map_err(|e| format!("Failed to prepare intervention effectiveness query: {e}"))?;

    let results = stmt
        .query_map(params![since_timestamp], |row| {
            let total: i64 = row.get(1)?;
            let successful: i64 = row.get(2)?;
            Ok(InterventionStats {
                intervention_action: row.get(0)?,
                total,
                successful,
                success_rate: if total > 0 {
                    successful as f64 / total as f64
                } else {
                    0.0
                },
            })
        })
        .map_err(|e| format!("Failed to query intervention effectiveness: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

/// Count distinct states observed in a task run.
pub fn get_state_coverage(
    conn: &Connection,
    task_run_id: i64,
) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT DISTINCT state_id
               FROM ui_bridge_events
               WHERE task_run_id = ?1
                 AND state_id IS NOT NULL"#,
        )
        .map_err(|e| format!("Failed to prepare state coverage query: {e}"))?;

    let results = stmt
        .query_map(params![task_run_id], |row| row.get::<_, String>(0))
        .map_err(|e| format!("Failed to query state coverage: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        // Create minimal tables for testing
        conn.execute_batch(
            r#"
            CREATE TABLE ui_bridge_elements (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_run_id INTEGER,
                timestamp INTEGER NOT NULL,
                element_id TEXT NOT NULL,
                tag_name TEXT,
                element_type TEXT,
                bounds TEXT,
                visible INTEGER DEFAULT 1,
                enabled INTEGER DEFAULT 1,
                focused INTEGER DEFAULT 0,
                value TEXT,
                text_content TEXT,
                label TEXT,
                parent_id TEXT,
                children TEXT,
                actions TEXT,
                metadata TEXT
            );

            CREATE TABLE ui_bridge_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_run_id INTEGER,
                timestamp INTEGER NOT NULL,
                sequence INTEGER NOT NULL,
                event_type TEXT NOT NULL,
                element_id TEXT,
                state_id TEXT,
                transition_id TEXT,
                action TEXT,
                params TEXT,
                result TEXT,
                duration_ms REAL,
                success INTEGER DEFAULT 1,
                error_message TEXT,
                metadata TEXT
            );

            CREATE TABLE ui_bridge_navigation_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_run_id INTEGER,
                timestamp INTEGER NOT NULL,
                target_states TEXT NOT NULL,
                path_found INTEGER NOT NULL,
                transitions_planned TEXT,
                transitions_executed TEXT,
                total_cost REAL,
                duration_ms REAL,
                success INTEGER DEFAULT 0,
                final_active_states TEXT,
                error_message TEXT
            );

            CREATE TABLE stall_events (
                id TEXT PRIMARY KEY,
                task_run_id TEXT NOT NULL,
                iteration INTEGER NOT NULL,
                pattern_type TEXT NOT NULL,
                pattern_details TEXT,
                action_count INTEGER,
                intervention_action TEXT,
                intervention_result TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE task_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workflow_name TEXT
            );
            "#,
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_insert_and_get_element_snapshot() {
        let conn = setup_test_db();

        let id = insert_element_snapshot(
            &conn,
            Some(1),
            "login-btn",
            Some("button"),
            Some("button"),
            Some(r#"{"x":10,"y":20,"width":100,"height":40}"#),
            true,
            true,
            None,
            Some("Log In"),
            Some("Login Button"),
            Some(r#"["click"]"#),
            None,
        )
        .unwrap();

        assert!(id > 0);
    }

    #[test]
    fn test_insert_and_get_events() {
        let conn = setup_test_db();

        insert_ui_bridge_event(
            &conn,
            Some(1),
            1,
            "action_executed",
            Some("login-btn"),
            None,
            None,
            Some("click"),
            None,
            Some(r#"{"success": true}"#),
            Some(150.0),
            true,
            None,
            None,
        )
        .unwrap();

        insert_ui_bridge_event(
            &conn,
            Some(1),
            2,
            "action_executed",
            Some("login-btn"),
            None,
            None,
            Some("click"),
            None,
            None,
            Some(200.0),
            false,
            Some("element not found"),
            None,
        )
        .unwrap();

        let events = get_element_interactions(&conn, 1).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].sequence, 1);
        assert!(events[0].success);
        assert!(!events[1].success);
    }

    #[test]
    fn test_element_history() {
        let conn = setup_test_db();

        // Insert events across different task runs
        for tr in 1..=3 {
            insert_ui_bridge_event(
                &conn,
                Some(tr),
                1,
                "action_executed",
                Some("submit-btn"),
                None,
                None,
                Some("click"),
                None,
                None,
                Some(100.0),
                tr != 2, // fail on run 2
                if tr == 2 {
                    Some("timeout")
                } else {
                    None
                },
                None,
            )
            .unwrap();
        }

        let history = get_element_history(&conn, "submit-btn", 10).unwrap();
        assert_eq!(history.len(), 3);
    }

    #[test]
    fn test_flaky_elements() {
        let conn = setup_test_db();

        // Insert 5 events: 3 success, 2 failure for "flaky-btn"
        for i in 1..=5 {
            insert_ui_bridge_event(
                &conn,
                Some(1),
                i,
                "action_executed",
                Some("flaky-btn"),
                None,
                None,
                Some("click"),
                None,
                None,
                Some(100.0),
                i <= 3,
                if i > 3 {
                    Some("element detached")
                } else {
                    None
                },
                None,
            )
            .unwrap();
        }

        // Insert 5 success events for "stable-btn"
        for i in 1..=5 {
            insert_ui_bridge_event(
                &conn,
                Some(1),
                10 + i,
                "action_executed",
                Some("stable-btn"),
                None,
                None,
                Some("click"),
                None,
                None,
                Some(50.0),
                true,
                None,
                None,
            )
            .unwrap();
        }

        let flaky = get_flaky_elements(&conn, 3, 0.8).unwrap();
        assert_eq!(flaky.len(), 1);
        assert_eq!(flaky[0].element_id, "flaky-btn");
        assert_eq!(flaky[0].total_interactions, 5);
        assert!((flaky[0].success_rate - 0.6).abs() < 0.01);
        assert!(flaky[0].flaky);
        assert_eq!(
            flaky[0].last_failure_reason.as_deref(),
            Some("element detached")
        );
    }

    #[test]
    fn test_element_reliability() {
        let conn = setup_test_db();

        // No interactions → None
        let r = get_element_reliability(&conn, "nonexistent").unwrap();
        assert!(r.is_none());

        // Add some interactions
        for i in 1..=4 {
            insert_ui_bridge_event(
                &conn,
                Some(1),
                i,
                "action_executed",
                Some("test-btn"),
                None,
                None,
                Some("click"),
                None,
                None,
                Some(100.0),
                i != 3,
                if i == 3 { Some("stale") } else { None },
                None,
            )
            .unwrap();
        }

        let r = get_element_reliability(&conn, "test-btn").unwrap().unwrap();
        assert_eq!(r.total_interactions, 4);
        assert_eq!(r.successful_interactions, 3);
        assert!((r.success_rate - 0.75).abs() < 0.01);
        assert!(r.flaky);
        assert_eq!(r.last_failure_reason.as_deref(), Some("stale"));
    }

    #[test]
    fn test_stall_events() {
        let conn = setup_test_db();

        let id = insert_stall_event(
            &conn,
            "task-run-123",
            5,
            "RepeatedIdenticalAction",
            Some("clicked same button 5 times"),
            Some(5),
            Some("RetryWithAlternativePrompt"),
            Some("success"),
        )
        .unwrap();

        assert!(!id.is_empty());

        let events = get_stall_events(&conn, "task-run-123").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].pattern_type, "RepeatedIdenticalAction");
        assert_eq!(events[0].iteration, 5);
    }

    #[test]
    fn test_navigation_event() {
        let conn = setup_test_db();

        let id = insert_navigation_event(
            &conn,
            Some(1),
            r#"["dashboard-state"]"#,
            true,
            Some(r#"["login-to-dashboard"]"#),
            Some(r#"["login-to-dashboard"]"#),
            Some(1.0),
            Some(2500.0),
            true,
            Some(r#"["dashboard-state"]"#),
            None,
        )
        .unwrap();

        assert!(id > 0);
    }
}
