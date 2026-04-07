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
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::State;

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
    let range = time_range
        .map(|s| TimeRange::from_str(&s))
        .unwrap_or(TimeRange::SevenDays);

    let range_days = match range {
        TimeRange::TwentyFourHours => 1,
        TimeRange::SevenDays => 7,
        TimeRange::ThirtyDays => 30,
        _ => 30,
    };

    match state
        .pg_db
        .get_performance_dashboard(&config_id, range_days)
        .await
    {
        Ok(val) => match serde_json::from_value(val) {
            Ok(dashboard) => Ok(PerformanceMetricsResponse::ok(dashboard)),
            Err(e) => Ok(PerformanceMetricsResponse::err(format!(
                "Deserialization error: {}",
                e
            ))),
        },
        Err(e) => Ok(PerformanceMetricsResponse::err(e)),
    }
}

/// Get action performance metrics only.
#[tauri::command]
pub async fn get_action_performance(
    state: State<'_, Arc<AppState>>,
    config_id: String,
    time_range: Option<String>,
) -> Result<PerformanceMetricsResponse<Vec<ActionPerformanceMetrics>>, String> {
    let range = time_range
        .map(|s| TimeRange::from_str(&s))
        .unwrap_or(TimeRange::SevenDays);
    let range_days = match range {
        TimeRange::TwentyFourHours => 1,
        TimeRange::SevenDays => 7,
        TimeRange::ThirtyDays => 30,
        _ => 30,
    };

    match state
        .pg_db
        .get_action_performance(&config_id, range_days)
        .await
    {
        Ok(vals) => {
            let metrics: Vec<ActionPerformanceMetrics> = vals
                .into_iter()
                .filter_map(|v| serde_json::from_value(v).ok())
                .collect();
            Ok(PerformanceMetricsResponse::ok(metrics))
        }
        Err(e) => Ok(PerformanceMetricsResponse::err(e)),
    }
}

/// Get transition metrics only.
#[tauri::command]
pub async fn get_transition_reliability(
    state: State<'_, Arc<AppState>>,
    config_id: String,
) -> Result<PerformanceMetricsResponse<Vec<StateTransitionMetrics>>, String> {
    match state.pg_db.get_transition_metrics(&config_id).await {
        Ok(vals) => {
            let metrics: Vec<StateTransitionMetrics> = vals
                .into_iter()
                .filter_map(|v| serde_json::from_value(v).ok())
                .collect();
            Ok(PerformanceMetricsResponse::ok(metrics))
        }
        Err(e) => Ok(PerformanceMetricsResponse::err(e)),
    }
}

/// Get element resolution metrics only.
#[tauri::command]
pub async fn get_element_resolution_metrics(
    state: State<'_, Arc<AppState>>,
    config_id: String,
) -> Result<PerformanceMetricsResponse<Vec<ElementResolutionMetrics>>, String> {
    match state.pg_db.get_element_metrics(&config_id).await {
        Ok(vals) => {
            let metrics: Vec<ElementResolutionMetrics> = vals
                .into_iter()
                .filter_map(|v| serde_json::from_value(v).ok())
                .collect();
            Ok(PerformanceMetricsResponse::ok(metrics))
        }
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
    let range = time_range
        .map(|s| TimeRange::from_str(&s))
        .unwrap_or(TimeRange::SevenDays);
    let range_days = match range {
        TimeRange::TwentyFourHours => 1,
        TimeRange::SevenDays => 7,
        TimeRange::ThirtyDays => 30,
        _ => 30,
    };

    match state
        .pg_db
        .get_success_rate_trend(&config_id, range_days)
        .await
    {
        Ok(vals) => {
            let trend: Vec<TimeSeriesDataPoint> = vals
                .into_iter()
                .filter_map(|v| serde_json::from_value(v).ok())
                .collect();
            Ok(PerformanceMetricsResponse::ok(trend))
        }
        Err(e) => Ok(PerformanceMetricsResponse::err(e)),
    }
}

// =============================================================================
// Aggregation Helpers
// =============================================================================

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
