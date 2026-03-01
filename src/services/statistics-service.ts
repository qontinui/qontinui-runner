/**
 * Statistics Service
 *
 * Provides Tauri invoke calls for the statistics dashboard.
 * Wraps the tiered_info Tauri commands from src-tauri/src/commands/tiered_info.rs
 */

import { invoke } from "@tauri-apps/api/core";
import type {
  TieredInfoResponse,
  ConfigStatistics,
  FlakyItem,
  DebuggingContext,
  RunDetails,
  FlakinessSummary,
  ExecutionOptions,
  RecordRunInput,
} from "../types/statistics";

/**
 * Service for accessing statistics dashboard data via Tauri commands.
 */
export const statisticsService = {
  // ===========================================================================
  // Config Statistics (Tier 4)
  // ===========================================================================

  /**
   * Get aggregated statistics for a configuration.
   * @param configId - Configuration ID
   */
  async getConfigStatistics(configId: string): Promise<TieredInfoResponse<ConfigStatistics>> {
    return invoke("get_config_statistics", { configId });
  },

  // ===========================================================================
  // Flakiness Data
  // ===========================================================================

  /**
   * Get flaky transitions for a configuration.
   * @param configId - Configuration ID
   * @param threshold - Flakiness threshold (default: 0.2, meaning 20-80% success rate is flaky)
   */
  async getFlakyTransitions(
    configId: string,
    threshold?: number,
  ): Promise<TieredInfoResponse<FlakyItem[]>> {
    return invoke("get_flaky_transitions", { configId, threshold });
  },

  /**
   * Get flaky templates for a configuration.
   * @param configId - Configuration ID
   * @param threshold - Flakiness threshold (default: 0.2)
   */
  async getFlakyTemplates(
    configId: string,
    threshold?: number,
  ): Promise<TieredInfoResponse<FlakyItem[]>> {
    return invoke("get_flaky_templates", { configId, threshold });
  },

  /**
   * Get a summary of all flaky items for a configuration.
   * Useful for preloading flakiness data before starting execution.
   * @param configId - Configuration ID
   * @param threshold - Flakiness threshold (default: 0.2)
   */
  async getFlakinessSummary(
    configId: string,
    threshold?: number,
  ): Promise<TieredInfoResponse<FlakinessSummary>> {
    return invoke("get_flakiness_summary", { configId, threshold });
  },

  // ===========================================================================
  // Debugging Context
  // ===========================================================================

  /**
   * Get debugging context for AI prompts.
   * Returns Tier 4 summary formatted for AI consumption.
   * @param configId - Configuration ID
   * @param configName - Optional configuration name
   */
  async getDebuggingContext(
    configId: string,
    configName?: string,
  ): Promise<TieredInfoResponse<DebuggingContext>> {
    return invoke("get_debugging_context", { configId, configName });
  },

  /**
   * Get debugging context formatted as markdown for AI prompts.
   * @param configId - Configuration ID
   * @param configName - Optional configuration name
   */
  async getDebuggingContextPrompt(
    configId: string,
    configName?: string,
  ): Promise<TieredInfoResponse<string>> {
    return invoke("get_debugging_context_prompt", { configId, configName });
  },

  // ===========================================================================
  // Run Details (Tier 1)
  // ===========================================================================

  /**
   * Get detailed run information.
   * @param runId - Run ID
   */
  async getRunDetails(runId: string): Promise<TieredInfoResponse<RunDetails>> {
    return invoke("get_run_details", { runId });
  },

  /**
   * Get recent runs for a configuration.
   * @param configId - Configuration ID
   * @param limit - Maximum number of runs to return (default: 10)
   */
  async getRecentRuns(configId: string, limit?: number): Promise<TieredInfoResponse<RunDetails[]>> {
    return invoke("get_recent_runs", { configId, limit });
  },

  /**
   * Get failed runs for a configuration (for debugging).
   * @param configId - Configuration ID
   * @param limit - Maximum number of runs to return (default: 5)
   */
  async getFailedRuns(configId: string, limit?: number): Promise<TieredInfoResponse<RunDetails[]>> {
    return invoke("get_failed_runs", { configId, limit });
  },

  // ===========================================================================
  // Execution Options
  // ===========================================================================

  /**
   * Get flakiness-aware execution options for a transition or template.
   *
   * Returns adjusted execution options (retry count, timeout multiplier,
   * confidence threshold) based on historical flakiness data.
   *
   * @param configId - Configuration ID to load flakiness data for
   * @param transitionId - Optional transition ID (format: "FromState|ToState")
   * @param templateId - Optional template/image ID
   *
   * If both transitionId and templateId are provided, transition options take precedence.
   * If neither is provided, returns default execution options.
   */
  async getExecutionOptions(
    configId: string,
    transitionId?: string,
    templateId?: string,
  ): Promise<TieredInfoResponse<ExecutionOptions>> {
    return invoke("get_execution_options", { configId, transitionId, templateId });
  },

  // ===========================================================================
  // Run Recording
  // ===========================================================================

  /**
   * Record a completed run and update statistics.
   * This is the main entry point for recording automation runs.
   * After recording, analyzes for discoveries and queues them for sync.
   *
   * @param input - Run recording input data
   * @returns The run ID if successful
   */
  async recordRun(input: RecordRunInput): Promise<TieredInfoResponse<string>> {
    return invoke("record_run", { input });
  },

  // ===========================================================================
  // Maintenance
  // ===========================================================================

  /**
   * Delete old runs to manage storage.
   * Keeps the most recent N runs per configuration.
   *
   * @param configId - Configuration ID
   * @param keepCount - Number of recent runs to keep (default: 100)
   * @returns Number of deleted runs
   */
  async cleanupOldRuns(configId: string, keepCount?: number): Promise<TieredInfoResponse<number>> {
    return invoke("cleanup_old_runs", { configId, keepCount });
  },
};

// ===========================================================================
// Helper Functions
// ===========================================================================

/**
 * Check if a TieredInfoResponse was successful.
 */
export function isSuccess<T>(
  response: TieredInfoResponse<T>,
): response is TieredInfoResponse<T> & { data: T } {
  return response.success && response.data !== undefined;
}

/**
 * Extract data from a TieredInfoResponse or throw an error.
 */
export function unwrapResponse<T>(response: TieredInfoResponse<T>): T {
  if (!response.success) {
    throw new Error(response.error || "Unknown error");
  }
  if (response.data === undefined) {
    throw new Error("Response successful but no data provided");
  }
  return response.data;
}

/**
 * Calculate success rate from ConfigStatistics.
 */
export function calculateSuccessRate(stats: ConfigStatistics): number {
  if (stats.total_runs === 0) {
    return 0;
  }
  return stats.successful_runs / stats.total_runs;
}

/**
 * Format duration in milliseconds to a human-readable string.
 */
export function formatDuration(ms: number): string {
  if (ms < 1000) {
    return `${ms}ms`;
  }
  if (ms < 60000) {
    return `${(ms / 1000).toFixed(1)}s`;
  }
  const minutes = Math.floor(ms / 60000);
  const seconds = Math.floor((ms % 60000) / 1000);
  return `${minutes}m ${seconds}s`;
}

/**
 * Get severity color for anomalies.
 */
export function getAnomalySeverityColor(severity: string): string {
  switch (severity) {
    case "high":
      return "text-red-500";
    case "medium":
      return "text-yellow-500";
    case "low":
      return "text-blue-500";
    default:
      return "text-muted-foreground";
  }
}

/**
 * Get status color for run status.
 */
export function getRunStatusColor(status: string): string {
  switch (status) {
    case "completed":
      return "text-green-500";
    case "running":
      return "text-blue-500";
    case "failed":
      return "text-red-500";
    case "timeout":
      return "text-orange-500";
    case "cancelled":
      return "text-muted-foreground";
    default:
      return "text-muted-foreground";
  }
}
