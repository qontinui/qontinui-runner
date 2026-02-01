/**
 * Performance Metrics Dashboard Types
 *
 * These types mirror the Rust types in src-tauri/src/commands/performance_metrics.rs
 * for the Performance Metrics Dashboard feature.
 */

// =============================================================================
// Response Wrapper
// =============================================================================

/**
 * Response wrapper for performance metrics commands.
 */
export interface PerformanceMetricsResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

// =============================================================================
// Core Metrics Types
// =============================================================================

/**
 * Performance metrics for a specific action type.
 */
export interface ActionPerformanceMetrics {
  /** Action type (e.g., "click", "type", "wait", "navigate") */
  action_type: string;
  /** Total number of executions */
  total_executions: number;
  /** Average duration in milliseconds */
  avg_duration_ms: number;
  /** 50th percentile (median) duration */
  p50_duration_ms: number;
  /** 95th percentile duration */
  p95_duration_ms: number;
  /** 99th percentile duration */
  p99_duration_ms: number;
  /** Number of successful executions */
  success_count: number;
  /** Number of failed executions */
  failure_count: number;
  /** Success rate as a decimal (0.0 to 1.0) */
  success_rate: number;
}

/**
 * Metrics for state transitions.
 */
export interface StateTransitionMetrics {
  /** Source state */
  from_state: string;
  /** Target state */
  to_state: string;
  /** Total transition attempts */
  total_attempts: number;
  /** Success rate as a decimal (0.0 to 1.0) */
  success_rate: number;
  /** Average duration in milliseconds */
  avg_duration_ms: number;
  /** Whether this transition is considered flaky (20-80% success rate) */
  is_flaky: boolean;
  /** Last failure timestamp (ISO format) */
  last_failure?: string;
}

/**
 * Metrics for element/template resolution.
 */
export interface ElementResolutionMetrics {
  /** Template/element name */
  element_name: string;
  /** Total resolution attempts */
  total_attempts: number;
  /** Number of successful matches */
  successful_matches: number;
  /** Number of failed matches */
  failed_matches: number;
  /** Success rate as a decimal (0.0 to 1.0) */
  success_rate: number;
  /** Average confidence score */
  avg_confidence: number;
  /** Minimum confidence observed */
  min_confidence?: number;
  /** Maximum confidence observed */
  max_confidence?: number;
  /** Whether this element is considered flaky */
  is_flaky: boolean;
}

/**
 * Overall summary metrics for the dashboard header.
 */
export interface PerformanceSummary {
  /** Total runs in the selected time range */
  total_runs: number;
  /** Overall success rate */
  overall_success_rate: number;
  /** Average run duration in milliseconds */
  avg_run_duration_ms: number;
  /** Total actions executed */
  total_actions: number;
  /** Number of flaky transitions */
  flaky_transition_count: number;
  /** Number of flaky templates */
  flaky_template_count: number;
  /** Most recent run timestamp */
  last_run_at?: string;
}

/**
 * Data point for time-series charts.
 */
export interface TimeSeriesDataPoint {
  /** Timestamp (ISO format, bucketed by hour/day) */
  timestamp: string;
  /** Value at this time point */
  value: number;
  /** Optional count for aggregation context */
  count?: number;
}

/**
 * Time range specification.
 */
export type TimeRange = "1h" | "24h" | "7d" | "30d" | "all";

/**
 * Human-readable labels for time ranges.
 */
export const TIME_RANGE_LABELS: Record<TimeRange, string> = {
  "1h": "Last Hour",
  "24h": "Last 24 Hours",
  "7d": "Last 7 Days",
  "30d": "Last 30 Days",
  all: "All Time",
};

/**
 * Complete performance dashboard data.
 */
export interface PerformanceDashboardData {
  /** Summary metrics for the header */
  summary: PerformanceSummary;
  /** Per-action performance metrics */
  action_metrics: ActionPerformanceMetrics[];
  /** State transition reliability */
  transition_metrics: StateTransitionMetrics[];
  /** Element resolution success rates */
  element_metrics: ElementResolutionMetrics[];
  /** Success rate trend over time */
  success_rate_trend: TimeSeriesDataPoint[];
  /** Average duration trend over time */
  duration_trend: TimeSeriesDataPoint[];
}

// =============================================================================
// Helper Functions
// =============================================================================

/**
 * Format duration in milliseconds to human-readable string.
 */
export function formatDurationMs(ms: number): string {
  if (ms < 1000) {
    return `${Math.round(ms)}ms`;
  }
  if (ms < 60000) {
    return `${(ms / 1000).toFixed(1)}s`;
  }
  const minutes = Math.floor(ms / 60000);
  const seconds = Math.floor((ms % 60000) / 1000);
  return `${minutes}m ${seconds}s`;
}

/**
 * Format success rate as percentage string.
 */
export function formatSuccessRate(rate: number): string {
  return `${(rate * 100).toFixed(1)}%`;
}

/**
 * Get color class for success rate.
 */
export function getSuccessRateColor(rate: number): string {
  if (rate >= 0.95) return "text-green-500";
  if (rate >= 0.8) return "text-yellow-500";
  if (rate >= 0.5) return "text-orange-500";
  return "text-red-500";
}

/**
 * Get background color class for success rate.
 */
export function getSuccessRateBgColor(rate: number): string {
  if (rate >= 0.95) return "bg-green-500/10";
  if (rate >= 0.8) return "bg-yellow-500/10";
  if (rate >= 0.5) return "bg-orange-500/10";
  return "bg-red-500/10";
}

/**
 * Check if an item is considered flaky based on success rate.
 * Flaky: 20-80% success rate with at least 5 attempts.
 */
export function isFlaky(successRate: number, totalAttempts: number): boolean {
  return totalAttempts >= 5 && successRate > 0.2 && successRate < 0.8;
}

/**
 * Format a timestamp for display.
 */
export function formatTimestamp(isoString: string): string {
  const date = new Date(isoString);
  return date.toLocaleString();
}

/**
 * Format a timestamp for chart axis.
 */
export function formatChartTimestamp(isoString: string, range: TimeRange): string {
  const date = new Date(isoString);
  if (range === "1h" || range === "24h") {
    return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  }
  return date.toLocaleDateString([], { month: "short", day: "numeric" });
}
