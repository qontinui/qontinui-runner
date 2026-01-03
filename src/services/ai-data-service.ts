/**
 * AI Data Service
 *
 * Provides Tauri invoke calls for the AI Data Viewer.
 * Wraps the ai_data Tauri commands from src-tauri/src/commands/ai_data.rs
 * and tiered_info commands for automation runs.
 */

import { invoke } from "@tauri-apps/api/core";
import type {
  AiDataResponse,
  TaskRun,
  JsonlLogsResult,
  JsonlLogsSummary,
  JsonlLogType,
} from "../types/aiData";
import type { TieredInfoResponse, RunDetails } from "../types/statistics";

/**
 * Service for accessing AI data viewer data via Tauri commands.
 */
export const aiDataService = {
  // ===========================================================================
  // Task Runs (AI sessions from task_runs table)
  // ===========================================================================

  /**
   * Get recent task runs.
   * @param limit - Maximum number of runs to return (default: 20)
   */
  async getTaskRuns(limit?: number): Promise<AiDataResponse<TaskRun[]>> {
    return invoke("get_task_runs_for_viewer", { limit });
  },

  /**
   * Get a specific task run with full output.
   * @param taskId - Task run ID
   */
  async getTaskRun(taskId: string): Promise<AiDataResponse<TaskRun>> {
    return invoke("get_task_run_for_viewer", { taskId });
  },

  // ===========================================================================
  // Automation Runs (from run_details table via tiered_info)
  // ===========================================================================

  /**
   * Get recent automation runs for a configuration.
   * @param configId - Configuration ID
   * @param limit - Maximum number of runs to return (default: 10)
   */
  async getAutomationRuns(
    configId: string,
    limit?: number,
  ): Promise<TieredInfoResponse<RunDetails[]>> {
    return invoke("get_recent_runs", { configId, limit });
  },

  /**
   * Get a specific automation run.
   * @param runId - Run ID
   */
  async getAutomationRun(runId: string): Promise<TieredInfoResponse<RunDetails>> {
    return invoke("get_run_details", { runId });
  },

  // ===========================================================================
  // JSONL Logs (from .dev-logs/ directory)
  // ===========================================================================

  /**
   * Get summary of all JSONL log files.
   */
  async getJsonlLogsSummary(): Promise<AiDataResponse<JsonlLogsSummary>> {
    return invoke("get_jsonl_logs_summary");
  },

  /**
   * Read JSONL log entries from a specific log file.
   * @param logType - Type of log to read
   * @param limit - Maximum number of entries to return (default: 100)
   */
  async readJsonlLogs(
    logType: JsonlLogType,
    limit?: number,
  ): Promise<AiDataResponse<JsonlLogsResult>> {
    return invoke("read_jsonl_logs_for_viewer", { logType, limit });
  },
};
