/**
 * Performance Metrics Service
 *
 * Provides Tauri invoke calls for the Performance Metrics Dashboard.
 * Wraps the performance_metrics Tauri commands from src-tauri/src/commands/performance_metrics.rs
 */

import { invoke } from "@tauri-apps/api/core";
import type {
  PerformanceMetricsResponse,
  PerformanceDashboardData,
  ActionPerformanceMetrics,
  StateTransitionMetrics,
  ElementResolutionMetrics,
  TimeSeriesDataPoint,
  TimeRange,
} from "../types/performance-metrics";

/**
 * Service for accessing performance metrics via Tauri commands.
 */
export const performanceMetricsService = {
  // ===========================================================================
  // Dashboard Data
  // ===========================================================================

  /**
   * Get complete performance dashboard data.
   * @param configId - Configuration ID
   * @param timeRange - Time range filter (default: "7d")
   */
  async getPerformanceDashboard(
    configId: string,
    timeRange: TimeRange = "7d",
  ): Promise<PerformanceMetricsResponse<PerformanceDashboardData>> {
    return invoke("get_performance_dashboard", { configId, timeRange });
  },

  // ===========================================================================
  // Individual Metrics
  // ===========================================================================

  /**
   * Get action performance metrics only.
   * @param configId - Configuration ID
   * @param timeRange - Time range filter
   */
  async getActionPerformance(
    configId: string,
    timeRange?: TimeRange,
  ): Promise<PerformanceMetricsResponse<ActionPerformanceMetrics[]>> {
    return invoke("get_action_performance", { configId, timeRange });
  },

  /**
   * Get transition reliability metrics only.
   * @param configId - Configuration ID
   */
  async getTransitionReliability(
    configId: string,
  ): Promise<PerformanceMetricsResponse<StateTransitionMetrics[]>> {
    return invoke("get_transition_reliability", { configId });
  },

  /**
   * Get element resolution metrics only.
   * @param configId - Configuration ID
   */
  async getElementResolutionMetrics(
    configId: string,
  ): Promise<PerformanceMetricsResponse<ElementResolutionMetrics[]>> {
    return invoke("get_element_resolution_metrics", { configId });
  },

  // ===========================================================================
  // Time Series Data
  // ===========================================================================

  /**
   * Get success rate trend for time-series chart.
   * @param configId - Configuration ID
   * @param timeRange - Time range filter
   */
  async getSuccessRateTrend(
    configId: string,
    timeRange?: TimeRange,
  ): Promise<PerformanceMetricsResponse<TimeSeriesDataPoint[]>> {
    return invoke("get_success_rate_trend", { configId, timeRange });
  },
};

// ===========================================================================
// Helper Functions
// ===========================================================================

/**
 * Check if a PerformanceMetricsResponse was successful.
 */
export function isSuccess<T>(
  response: PerformanceMetricsResponse<T>,
): response is PerformanceMetricsResponse<T> & { data: T } {
  return response.success && response.data !== undefined;
}

/**
 * Extract data from a PerformanceMetricsResponse or throw an error.
 */
export function unwrapResponse<T>(response: PerformanceMetricsResponse<T>): T {
  if (!response.success) {
    throw new Error(response.error || "Unknown error");
  }
  if (response.data === undefined) {
    throw new Error("Response successful but no data provided");
  }
  return response.data;
}
