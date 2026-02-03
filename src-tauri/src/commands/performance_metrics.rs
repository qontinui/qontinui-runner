//! Performance Metrics Dashboard commands.
//!
//! Provides Tauri commands for the Performance Metrics Dashboard:
//! - Action execution times with percentiles (p50, p95, p99)
//! - Element resolution success rates
//! - State transition reliability metrics
//!
//! Data sources:
//! - task_run_events table (action-level timing and success data)
//! - config_statistics table (aggregated transition/template statistics)
//! - task_run_automation table (run-level metrics)

use crate::commands::AppState;
use crate::tiered_info;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tauri::State;
use tracing::debug;

// =============================================================================
// Response Types
// =============================================================================

/// Response wrapper for performance metrics commands.
#[derive(Debug, Serialize)]
pub struct PerformanceMetricsResponse<T> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl<T> PerformanceMetricsResponse<T> {
    pub fn ok(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(message: String) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message),
        }
    }
}

// =============================================================================
// Performance Metrics Types
// =============================================================================

/// Performance metrics for a specific action type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionPerformanceMetrics {
    /// Action type (e.g., "click", "type", "wait", "navigate")
    pub action_type: String,
    /// Total number of executions
    pub total_executions: u32,
    /// Average duration in milliseconds
    pub avg_duration_ms: f64,
    /// 50th percentile (median) duration
    pub p50_duration_ms: u64,
    /// 95th percentile duration
    pub p95_duration_ms: u64,
    /// 99th percentile duration
    pub p99_duration_ms: u64,
    /// Number of successful executions
    pub success_count: u32,
    /// Number of failed executions
    pub failure_count: u32,
    /// Success rate as a decimal (0.0 to 1.0)
    pub success_rate: f64,
}

/// Metrics for state transitions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateTransitionMetrics {
    /// Source state
    pub from_state: String,
    /// Target state
    pub to_state: String,
    /// Total transition attempts
    pub total_attempts: u32,
    /// Success rate as a decimal (0.0 to 1.0)
    pub success_rate: f64,
    /// Average duration in milliseconds
    pub avg_duration_ms: f64,
    /// Whether this transition is considered flaky (20-80% success rate)
    pub is_flaky: bool,
    /// Last failure timestamp (ISO format)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<String>,
}

/// Metrics for element/template resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementResolutionMetrics {
    /// Template/element name
    pub element_name: String,
    /// Total resolution attempts
    pub total_attempts: u32,
    /// Number of successful matches
    pub successful_matches: u32,
    /// Number of failed matches
    pub failed_matches: u32,
    /// Success rate as a decimal (0.0 to 1.0)
    pub success_rate: f64,
    /// Average confidence score
    pub avg_confidence: f64,
    /// Minimum confidence observed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_confidence: Option<f64>,
    /// Maximum confidence observed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_confidence: Option<f64>,
    /// Whether this element is considered flaky
    pub is_flaky: bool,
}

/// Overall summary metrics for the dashboard header.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceSummary {
    /// Total runs in the selected time range
    pub total_runs: u32,
    /// Overall success rate
    pub overall_success_rate: f64,
    /// Average run duration in milliseconds
    pub avg_run_duration_ms: f64,
    /// Total actions executed
    pub total_actions: u32,
    /// Number of flaky transitions
    pub flaky_transition_count: u32,
    /// Number of flaky templates
    pub flaky_template_count: u32,
    /// Most recent run timestamp
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
}

/// Data point for time-series charts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeSeriesDataPoint {
    /// Timestamp (ISO format, bucketed by hour/day)
    pub timestamp: String,
    /// Value at this time point
    pub value: f64,
    /// Optional count for aggregation context
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
}

/// Time range specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeRange {
    /// Last hour
    OneHour,
    /// Last 24 hours
    TwentyFourHours,
    /// Last 7 days
    SevenDays,
    /// Last 30 days
    ThirtyDays,
    /// All time (no filter)
    AllTime,
}

impl TimeRange {
    /// Get the cutoff datetime for this time range.
    pub fn cutoff(&self) -> Option<DateTime<Utc>> {
        let now = Utc::now();
        match self {
            TimeRange::OneHour => Some(now - Duration::hours(1)),
            TimeRange::TwentyFourHours => Some(now - Duration::hours(24)),
            TimeRange::SevenDays => Some(now - Duration::days(7)),
            TimeRange::ThirtyDays => Some(now - Duration::days(30)),
            TimeRange::AllTime => None,
        }
    }

    /// Parse from string (for Tauri command parameters).
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "1h" | "one_hour" | "hour" => TimeRange::OneHour,
            "24h" | "twenty_four_hours" | "day" => TimeRange::TwentyFourHours,
            "7d" | "seven_days" | "week" => TimeRange::SevenDays,
            "30d" | "thirty_days" | "month" => TimeRange::ThirtyDays,
            _ => TimeRange::AllTime,
        }
    }
}

/// Complete performance dashboard data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceDashboardData {
    /// Summary metrics for the header
    pub summary: PerformanceSummary,
    /// Per-action performance metrics
    pub action_metrics: Vec<ActionPerformanceMetrics>,
    /// State transition reliability
    pub transition_metrics: Vec<StateTransitionMetrics>,
    /// Element resolution success rates
    pub element_metrics: Vec<ElementResolutionMetrics>,
    /// Success rate trend over time
    pub success_rate_trend: Vec<TimeSeriesDataPoint>,
    /// Average duration trend over time
    pub duration_trend: Vec<TimeSeriesDataPoint>,
}

// =============================================================================
// Tauri Commands
// =============================================================================

/// Get complete performance dashboard data.
#[tauri::command]
pub async fn get_performance_dashboard(
    state: State<'_, Arc<AppState>>,
    config_id: String,
    time_range: Option<String>,
) -> Result<PerformanceMetricsResponse<PerformanceDashboardData>, String> {
    let db = &state.checkpoint_db;
    let conn = db.connection()?;

    let range = time_range
        .map(|s| TimeRange::from_str(&s))
        .unwrap_or(TimeRange::SevenDays);

    // Aggregate all dashboard data
    let summary = aggregate_summary(&conn, &config_id, &range)?;
    let action_metrics = aggregate_action_performance(&conn, &config_id, &range)?;
    let transition_metrics = get_transition_metrics(&conn, &config_id)?;
    let element_metrics = get_element_metrics(&conn, &config_id)?;
    let success_rate_trend = aggregate_success_rate_trend(&conn, &config_id, &range)?;
    let duration_trend = aggregate_duration_trend(&conn, &config_id, &range)?;

    let dashboard = PerformanceDashboardData {
        summary,
        action_metrics,
        transition_metrics,
        element_metrics,
        success_rate_trend,
        duration_trend,
    };

    debug!(
        "Performance dashboard for config {}: {} actions, {} transitions, {} elements",
        config_id,
        dashboard.action_metrics.len(),
        dashboard.transition_metrics.len(),
        dashboard.element_metrics.len()
    );

    Ok(PerformanceMetricsResponse::ok(dashboard))
}

/// Get action performance metrics only.
#[tauri::command]
pub async fn get_action_performance(
    state: State<'_, Arc<AppState>>,
    config_id: String,
    time_range: Option<String>,
) -> Result<PerformanceMetricsResponse<Vec<ActionPerformanceMetrics>>, String> {
    let db = &state.checkpoint_db;
    let conn = db.connection()?;

    let range = time_range
        .map(|s| TimeRange::from_str(&s))
        .unwrap_or(TimeRange::SevenDays);

    match aggregate_action_performance(&conn, &config_id, &range) {
        Ok(metrics) => Ok(PerformanceMetricsResponse::ok(metrics)),
        Err(e) => Ok(PerformanceMetricsResponse::err(e)),
    }
}

/// Get transition metrics only.
#[tauri::command]
pub async fn get_transition_reliability(
    state: State<'_, Arc<AppState>>,
    config_id: String,
) -> Result<PerformanceMetricsResponse<Vec<StateTransitionMetrics>>, String> {
    let db = &state.checkpoint_db;
    let conn = db.connection()?;

    match get_transition_metrics(&conn, &config_id) {
        Ok(metrics) => Ok(PerformanceMetricsResponse::ok(metrics)),
        Err(e) => Ok(PerformanceMetricsResponse::err(e)),
    }
}

/// Get element resolution metrics only.
#[tauri::command]
pub async fn get_element_resolution_metrics(
    state: State<'_, Arc<AppState>>,
    config_id: String,
) -> Result<PerformanceMetricsResponse<Vec<ElementResolutionMetrics>>, String> {
    let db = &state.checkpoint_db;
    let conn = db.connection()?;

    match get_element_metrics(&conn, &config_id) {
        Ok(metrics) => Ok(PerformanceMetricsResponse::ok(metrics)),
        Err(e) => Ok(PerformanceMetricsResponse::err(e)),
    }
}

/// Get success rate trend for time-series chart.
#[tauri::command]
pub async fn get_success_rate_trend(
    state: State<'_, Arc<AppState>>,
    config_id: String,
    time_range: Option<String>,
) -> Result<PerformanceMetricsResponse<Vec<TimeSeriesDataPoint>>, String> {
    let db = &state.checkpoint_db;
    let conn = db.connection()?;

    let range = time_range
        .map(|s| TimeRange::from_str(&s))
        .unwrap_or(TimeRange::SevenDays);

    match aggregate_success_rate_trend(&conn, &config_id, &range) {
        Ok(trend) => Ok(PerformanceMetricsResponse::ok(trend)),
        Err(e) => Ok(PerformanceMetricsResponse::err(e)),
    }
}

// =============================================================================
// Aggregation Functions
// =============================================================================

/// Aggregate summary metrics for the dashboard header.
fn aggregate_summary(
    conn: &Connection,
    config_id: &str,
    range: &TimeRange,
) -> Result<PerformanceSummary, String> {
    // Get config statistics for overall metrics
    let stats = tiered_info::get_config_statistics(conn, config_id)?;

    let (total_runs, overall_success_rate, avg_run_duration_ms, flaky_transition_count, flaky_template_count, last_run_at) = match stats {
        Some(s) => {
            let success_rate = if s.total_runs > 0 {
                s.successful_runs as f64 / s.total_runs as f64
            } else {
                0.0
            };
            (
                s.total_runs,
                success_rate,
                s.avg_duration_ms.unwrap_or(0) as f64,
                s.flaky_transitions.len() as u32,
                s.flaky_templates.len() as u32,
                s.last_run_at,
            )
        }
        None => (0, 0.0, 0.0, 0, 0, None),
    };

    // Count total actions from task_run_events within time range
    let total_actions = count_actions_in_range(conn, config_id, range)?;

    Ok(PerformanceSummary {
        total_runs,
        overall_success_rate,
        avg_run_duration_ms,
        total_actions,
        flaky_transition_count,
        flaky_template_count,
        last_run_at,
    })
}

/// Count actions within the time range.
fn count_actions_in_range(
    conn: &Connection,
    config_id: &str,
    range: &TimeRange,
) -> Result<u32, String> {
    let cutoff = range.cutoff();

    let sql = if cutoff.is_some() {
        r#"
        SELECT COUNT(*)
        FROM task_run_events e
        INNER JOIN task_runs tr ON e.task_run_id = tr.id
        WHERE tr.config_id = ?1
          AND e.event_type = 'action'
          AND e.timestamp >= ?2
        "#
    } else {
        r#"
        SELECT COUNT(*)
        FROM task_run_events e
        INNER JOIN task_runs tr ON e.task_run_id = tr.id
        WHERE tr.config_id = ?1
          AND e.event_type = 'action'
        "#
    };

    let count: u32 = if let Some(cutoff_dt) = cutoff {
        conn.query_row(sql, params![config_id, cutoff_dt.to_rfc3339()], |row| {
            row.get(0)
        })
        .unwrap_or(0)
    } else {
        conn.query_row(sql, params![config_id], |row| row.get(0))
            .unwrap_or(0)
    };

    Ok(count)
}

/// Aggregate action performance metrics from task_run_events.
fn aggregate_action_performance(
    conn: &Connection,
    config_id: &str,
    range: &TimeRange,
) -> Result<Vec<ActionPerformanceMetrics>, String> {
    let cutoff = range.cutoff();

    // Query action events with duration data
    let sql = if cutoff.is_some() {
        r#"
        SELECT
            e.event_subtype as action_type,
            e.duration_ms,
            CASE
                WHEN e.data LIKE '%"success":true%' OR e.data LIKE '%"success": true%' THEN 1
                WHEN e.data LIKE '%"success":false%' OR e.data LIKE '%"success": false%' THEN 0
                ELSE 1
            END as success
        FROM task_run_events e
        INNER JOIN task_runs tr ON e.task_run_id = tr.id
        WHERE tr.config_id = ?1
          AND e.event_type = 'action'
          AND e.event_subtype IS NOT NULL
          AND e.duration_ms IS NOT NULL
          AND e.timestamp >= ?2
        ORDER BY e.event_subtype
        "#
    } else {
        r#"
        SELECT
            e.event_subtype as action_type,
            e.duration_ms,
            CASE
                WHEN e.data LIKE '%"success":true%' OR e.data LIKE '%"success": true%' THEN 1
                WHEN e.data LIKE '%"success":false%' OR e.data LIKE '%"success": false%' THEN 0
                ELSE 1
            END as success
        FROM task_run_events e
        INNER JOIN task_runs tr ON e.task_run_id = tr.id
        WHERE tr.config_id = ?1
          AND e.event_type = 'action'
          AND e.event_subtype IS NOT NULL
          AND e.duration_ms IS NOT NULL
        ORDER BY e.event_subtype
        "#
    };

    // Collect all durations per action type
    let mut action_data: HashMap<String, Vec<(u64, bool)>> = HashMap::new();

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

    let mut rows = if let Some(cutoff_dt) = cutoff {
        stmt.query(params![config_id, cutoff_dt.to_rfc3339()])
    } else {
        stmt.query(params![config_id])
    }
    .map_err(|e| format!("Failed to query action events: {}", e))?;

    while let Some(row) = rows.next().map_err(|e| format!("Failed to read row: {}", e))? {
        let action_type: String = row.get(0).map_err(|e| format!("Failed to get action_type: {}", e))?;
        let duration_ms: i64 = row.get(1).map_err(|e| format!("Failed to get duration_ms: {}", e))?;
        let success: i32 = row.get(2).map_err(|e| format!("Failed to get success: {}", e))?;

        action_data
            .entry(action_type)
            .or_default()
            .push((duration_ms as u64, success == 1));
    }

    // Calculate metrics for each action type
    let mut metrics: Vec<ActionPerformanceMetrics> = action_data
        .into_iter()
        .map(|(action_type, data)| {
            let durations: Vec<u64> = data.iter().map(|(d, _)| *d).collect();
            let (p50, p95, p99) = calculate_percentiles(&durations);

            let total = data.len() as u32;
            let success_count = data.iter().filter(|(_, s)| *s).count() as u32;
            let failure_count = total - success_count;
            let avg_duration: f64 = if total > 0 {
                durations.iter().sum::<u64>() as f64 / total as f64
            } else {
                0.0
            };
            let success_rate = if total > 0 {
                success_count as f64 / total as f64
            } else {
                0.0
            };

            ActionPerformanceMetrics {
                action_type,
                total_executions: total,
                avg_duration_ms: avg_duration,
                p50_duration_ms: p50,
                p95_duration_ms: p95,
                p99_duration_ms: p99,
                success_count,
                failure_count,
                success_rate,
            }
        })
        .collect();

    // Sort by total executions (most common first)
    metrics.sort_by(|a, b| b.total_executions.cmp(&a.total_executions));

    Ok(metrics)
}

/// Calculate percentiles (p50, p95, p99) from a list of durations.
fn calculate_percentiles(durations: &[u64]) -> (u64, u64, u64) {
    if durations.is_empty() {
        return (0, 0, 0);
    }

    let mut sorted = durations.to_vec();
    sorted.sort_unstable();

    let len = sorted.len();
    // Use (n-1) * percentile formula for 0-indexed arrays
    // This gives the correct index where the percentile falls
    let p50_idx = ((len - 1) as f64 * 0.5).floor() as usize;
    let p95_idx = ((len - 1) as f64 * 0.95).floor() as usize;
    let p99_idx = ((len - 1) as f64 * 0.99).floor() as usize;

    let p50 = sorted[p50_idx.min(len - 1)];
    let p95 = sorted[p95_idx.min(len - 1)];
    let p99 = sorted[p99_idx.min(len - 1)];

    (p50, p95, p99)
}

/// Get transition metrics from config_statistics.
fn get_transition_metrics(
    conn: &Connection,
    config_id: &str,
) -> Result<Vec<StateTransitionMetrics>, String> {
    let stats = tiered_info::get_config_statistics(conn, config_id)?;

    let metrics: Vec<StateTransitionMetrics> = match stats {
        Some(s) => s
            .transition_stats
            .iter()
            .map(|(key, ts)| {
                let parts: Vec<&str> = key.split('|').collect();
                let (from_state, to_state) = if parts.len() >= 2 {
                    (parts[0].to_string(), parts[1].to_string())
                } else {
                    (key.clone(), "unknown".to_string())
                };

                let success_rate = ts.success_rate();
                let is_flaky = ts.is_flaky(0.2);

                StateTransitionMetrics {
                    from_state,
                    to_state,
                    total_attempts: ts.total,
                    success_rate,
                    avg_duration_ms: ts.avg_duration_ms as f64,
                    is_flaky,
                    last_failure: ts.last_failure.clone(),
                }
            })
            .collect(),
        None => Vec::new(),
    };

    Ok(metrics)
}

/// Get element resolution metrics from config_statistics.
fn get_element_metrics(
    conn: &Connection,
    config_id: &str,
) -> Result<Vec<ElementResolutionMetrics>, String> {
    let stats = tiered_info::get_config_statistics(conn, config_id)?;

    let metrics: Vec<ElementResolutionMetrics> = match stats {
        Some(s) => s
            .template_stats
            .iter()
            .map(|(key, ts)| {
                let success_rate = ts.match_rate();
                let is_flaky = ts.is_flaky(0.2);

                ElementResolutionMetrics {
                    element_name: key.clone(),
                    total_attempts: ts.total,
                    successful_matches: ts.matches,
                    failed_matches: ts.failures,
                    success_rate,
                    avg_confidence: ts.avg_confidence as f64,
                    min_confidence: ts.min_confidence.map(|c| c as f64),
                    max_confidence: ts.max_confidence.map(|c| c as f64),
                    is_flaky,
                }
            })
            .collect(),
        None => Vec::new(),
    };

    Ok(metrics)
}

/// Aggregate success rate trend over time.
fn aggregate_success_rate_trend(
    conn: &Connection,
    config_id: &str,
    range: &TimeRange,
) -> Result<Vec<TimeSeriesDataPoint>, String> {
    let cutoff = range.cutoff();

    // Determine bucket format based on time range
    let (bucket_format, sql) = match range {
        TimeRange::OneHour | TimeRange::TwentyFourHours => {
            // Bucket by hour
            let sql = if cutoff.is_some() {
                r#"
                SELECT
                    strftime('%Y-%m-%dT%H:00:00Z', tra.started_at) as bucket,
                    COUNT(*) as total,
                    SUM(CASE WHEN tra.success = 1 THEN 1 ELSE 0 END) as successes
                FROM task_run_automation tra
                INNER JOIN task_runs tr ON tra.task_run_id = tr.id
                WHERE tr.config_id = ?1
                  AND tra.started_at >= ?2
                GROUP BY bucket
                ORDER BY bucket ASC
                "#
            } else {
                r#"
                SELECT
                    strftime('%Y-%m-%dT%H:00:00Z', tra.started_at) as bucket,
                    COUNT(*) as total,
                    SUM(CASE WHEN tra.success = 1 THEN 1 ELSE 0 END) as successes
                FROM task_run_automation tra
                INNER JOIN task_runs tr ON tra.task_run_id = tr.id
                WHERE tr.config_id = ?1
                GROUP BY bucket
                ORDER BY bucket ASC
                "#
            };
            ("hourly", sql)
        }
        _ => {
            // Bucket by day
            let sql = if cutoff.is_some() {
                r#"
                SELECT
                    strftime('%Y-%m-%dT00:00:00Z', tra.started_at) as bucket,
                    COUNT(*) as total,
                    SUM(CASE WHEN tra.success = 1 THEN 1 ELSE 0 END) as successes
                FROM task_run_automation tra
                INNER JOIN task_runs tr ON tra.task_run_id = tr.id
                WHERE tr.config_id = ?1
                  AND tra.started_at >= ?2
                GROUP BY bucket
                ORDER BY bucket ASC
                "#
            } else {
                r#"
                SELECT
                    strftime('%Y-%m-%dT00:00:00Z', tra.started_at) as bucket,
                    COUNT(*) as total,
                    SUM(CASE WHEN tra.success = 1 THEN 1 ELSE 0 END) as successes
                FROM task_run_automation tra
                INNER JOIN task_runs tr ON tra.task_run_id = tr.id
                WHERE tr.config_id = ?1
                GROUP BY bucket
                ORDER BY bucket ASC
                "#
            };
            ("daily", sql)
        }
    };

    debug!("Aggregating success rate trend ({}) for config {}", bucket_format, config_id);

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("Failed to prepare trend query: {}", e))?;

    let mut rows = if let Some(cutoff_dt) = cutoff {
        stmt.query(params![config_id, cutoff_dt.to_rfc3339()])
    } else {
        stmt.query(params![config_id])
    }
    .map_err(|e| format!("Failed to query trend data: {}", e))?;

    let mut trend: Vec<TimeSeriesDataPoint> = Vec::new();
    while let Some(row) = rows.next().map_err(|e| format!("Failed to read trend row: {}", e))? {
        let timestamp: String = row.get(0).map_err(|e| format!("Failed to get timestamp: {}", e))?;
        let total: u32 = row.get(1).map_err(|e| format!("Failed to get total: {}", e))?;
        let successes: u32 = row.get(2).map_err(|e| format!("Failed to get successes: {}", e))?;

        let value = if total > 0 {
            successes as f64 / total as f64
        } else {
            0.0
        };
        trend.push(TimeSeriesDataPoint {
            timestamp,
            value,
            count: Some(total),
        });
    }

    Ok(trend)
}

/// Aggregate duration trend over time.
fn aggregate_duration_trend(
    conn: &Connection,
    config_id: &str,
    range: &TimeRange,
) -> Result<Vec<TimeSeriesDataPoint>, String> {
    let cutoff = range.cutoff();

    // Determine bucket format based on time range
    let sql = match range {
        TimeRange::OneHour | TimeRange::TwentyFourHours => {
            if cutoff.is_some() {
                r#"
                SELECT
                    strftime('%Y-%m-%dT%H:00:00Z', tra.started_at) as bucket,
                    AVG(tra.duration_ms) as avg_duration,
                    COUNT(*) as total
                FROM task_run_automation tra
                INNER JOIN task_runs tr ON tra.task_run_id = tr.id
                WHERE tr.config_id = ?1
                  AND tra.started_at >= ?2
                  AND tra.duration_ms IS NOT NULL
                GROUP BY bucket
                ORDER BY bucket ASC
                "#
            } else {
                r#"
                SELECT
                    strftime('%Y-%m-%dT%H:00:00Z', tra.started_at) as bucket,
                    AVG(tra.duration_ms) as avg_duration,
                    COUNT(*) as total
                FROM task_run_automation tra
                INNER JOIN task_runs tr ON tra.task_run_id = tr.id
                WHERE tr.config_id = ?1
                  AND tra.duration_ms IS NOT NULL
                GROUP BY bucket
                ORDER BY bucket ASC
                "#
            }
        }
        _ => {
            if cutoff.is_some() {
                r#"
                SELECT
                    strftime('%Y-%m-%dT00:00:00Z', tra.started_at) as bucket,
                    AVG(tra.duration_ms) as avg_duration,
                    COUNT(*) as total
                FROM task_run_automation tra
                INNER JOIN task_runs tr ON tra.task_run_id = tr.id
                WHERE tr.config_id = ?1
                  AND tra.started_at >= ?2
                  AND tra.duration_ms IS NOT NULL
                GROUP BY bucket
                ORDER BY bucket ASC
                "#
            } else {
                r#"
                SELECT
                    strftime('%Y-%m-%dT00:00:00Z', tra.started_at) as bucket,
                    AVG(tra.duration_ms) as avg_duration,
                    COUNT(*) as total
                FROM task_run_automation tra
                INNER JOIN task_runs tr ON tra.task_run_id = tr.id
                WHERE tr.config_id = ?1
                  AND tra.duration_ms IS NOT NULL
                GROUP BY bucket
                ORDER BY bucket ASC
                "#
            }
        }
    };

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("Failed to prepare duration trend query: {}", e))?;

    let mut rows = if let Some(cutoff_dt) = cutoff {
        stmt.query(params![config_id, cutoff_dt.to_rfc3339()])
    } else {
        stmt.query(params![config_id])
    }
    .map_err(|e| format!("Failed to query duration trend data: {}", e))?;

    let mut trend: Vec<TimeSeriesDataPoint> = Vec::new();
    while let Some(row) = rows.next().map_err(|e| format!("Failed to read duration row: {}", e))? {
        let timestamp: String = row.get(0).map_err(|e| format!("Failed to get timestamp: {}", e))?;
        let avg_duration: f64 = row.get(1).map_err(|e| format!("Failed to get avg_duration: {}", e))?;
        let total: u32 = row.get(2).map_err(|e| format!("Failed to get total: {}", e))?;

        trend.push(TimeSeriesDataPoint {
            timestamp,
            value: avg_duration,
            count: Some(total),
        });
    }

    Ok(trend)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_percentiles_empty() {
        let (p50, p95, p99) = calculate_percentiles(&[]);
        assert_eq!(p50, 0);
        assert_eq!(p95, 0);
        assert_eq!(p99, 0);
    }

    #[test]
    fn test_calculate_percentiles_single() {
        let (p50, p95, p99) = calculate_percentiles(&[100]);
        assert_eq!(p50, 100);
        assert_eq!(p95, 100);
        assert_eq!(p99, 100);
    }

    #[test]
    fn test_calculate_percentiles_range() {
        // 1-100 range, sorted
        let durations: Vec<u64> = (1..=100).collect();
        let (p50, p95, p99) = calculate_percentiles(&durations);
        assert_eq!(p50, 50);
        assert_eq!(p95, 95);
        assert_eq!(p99, 99);
    }

    #[test]
    fn test_time_range_parsing() {
        assert!(matches!(TimeRange::from_str("1h"), TimeRange::OneHour));
        assert!(matches!(
            TimeRange::from_str("24h"),
            TimeRange::TwentyFourHours
        ));
        assert!(matches!(TimeRange::from_str("7d"), TimeRange::SevenDays));
        assert!(matches!(TimeRange::from_str("30d"), TimeRange::ThirtyDays));
        assert!(matches!(TimeRange::from_str("all"), TimeRange::AllTime));
    }

    #[test]
    fn test_time_range_cutoff() {
        assert!(TimeRange::OneHour.cutoff().is_some());
        assert!(TimeRange::AllTime.cutoff().is_none());
    }
}
