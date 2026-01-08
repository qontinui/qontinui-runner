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
  TextLogsResult,
  TextLogsSummary,
  TextLogType,
  ScreenshotsResult,
  LoadedConfigInfo,
  AiPromptsResult,
  ContextsResult,
  ConsolidatedAiOutputResult,
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

  /**
   * Reopen a finished task run to add more iterations.
   * This allows continuing a task that didn't achieve its goal.
   * @param taskId - Task run ID
   * @param additionalSessions - Number of additional sessions to add
   */
  async reopenTaskRun(
    taskId: string,
    additionalSessions: number,
  ): Promise<AiDataResponse<TaskRun>> {
    return invoke("reopen_task_run", { taskId, additionalSessions });
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

  /**
   * Read JSONL log entries filtered by task run time range.
   * @param logType - Type of log to read
   * @param taskRunId - Task run ID to filter logs by
   */
  async readJsonlLogsForTaskRun(
    logType: JsonlLogType,
    taskRunId: string,
  ): Promise<AiDataResponse<JsonlLogsResult>> {
    return invoke("read_jsonl_logs_for_task_run", { logType, taskRunId });
  },

  /**
   * Get consolidated AI output for a task run.
   * Groups consecutive log entries by source into readable chunks.
   * @param taskRunId - Task run ID to get output for
   */
  async getConsolidatedAiOutput(
    taskRunId: string,
  ): Promise<AiDataResponse<ConsolidatedAiOutputResult>> {
    return invoke("get_consolidated_ai_output", { taskRunId });
  },

  // ===========================================================================
  // Text Logs (plain text, filtered by task run time range)
  // ===========================================================================

  /**
   * Get summary of all text log files for a task run.
   * @param taskRunId - Task run ID to get logs for
   */
  async getTextLogsSummary(taskRunId: string): Promise<AiDataResponse<TextLogsSummary>> {
    return invoke("get_text_logs_summary", { taskRunId });
  },

  /**
   * Read text log content filtered by task run time range.
   * @param logType - Type of log to read
   * @param taskRunId - Task run ID to filter logs by
   */
  async readTextLogs(
    logType: TextLogType,
    taskRunId: string,
  ): Promise<AiDataResponse<TextLogsResult>> {
    return invoke("read_text_logs_for_viewer", { logType, taskRunId });
  },

  // ===========================================================================
  // Screenshots (annotated and playwright)
  // ===========================================================================

  /**
   * Get list of screenshots from .dev-logs/screenshots/ and playwright-screenshots/.
   */
  async getScreenshots(): Promise<AiDataResponse<ScreenshotsResult>> {
    return invoke("get_screenshots_for_viewer");
  },

  // ===========================================================================
  // Loaded Config
  // ===========================================================================

  /**
   * Get the currently loaded workflow config.
   */
  async getLoadedConfig(): Promise<AiDataResponse<LoadedConfigInfo>> {
    return invoke("get_loaded_config_for_viewer");
  },

  // ===========================================================================
  // AI Prompts
  // ===========================================================================

  /**
   * Get AI prompts for a task run.
   * @param taskRunId - Task run ID to get prompts for
   */
  async getAiPrompts(taskRunId: string): Promise<AiDataResponse<AiPromptsResult>> {
    return invoke("get_ai_prompts_for_viewer", { taskRunId });
  },

  // ===========================================================================
  // Contexts
  // ===========================================================================

  /**
   * Get all available contexts (user, builtin, project).
   */
  async getContexts(): Promise<AiDataResponse<ContextsResult>> {
    return invoke("get_contexts_for_viewer");
  },
};
