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
  // SQLite migrated log types
  TaskRunEventsResult,
  TaskRunScreenshotsDbResult,
  TaskRunPlaywrightResultsDbResult,
  TaskRunMigratedLogsSummary,
  TaskRunApiRequestsDbResult,
  TaskRunAwasStepsDbResult,
} from "../types/aiData";
import type { TieredInfoResponse, RunDetails } from "../types/statistics";
import type { TaskRunMcpCallsDbResult } from "../types/mcp-config";

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

  /**
   * Generate an AI summary for a completed task run.
   * This calls Claude CLI to analyze the task output and generate:
   * - A paragraph summary of what was accomplished
   * - Whether the stated goal was achieved
   * - What remaining work exists (if goal not achieved)
   *
   * @param taskId - Task run ID
   * @returns Summary generation result
   */
  async generateSummary(taskId: string): Promise<{
    success: boolean;
    summary?: string;
    goal_achieved?: boolean;
    remaining_work?: string | null;
    error?: string;
  }> {
    try {
      const response = await fetch(`http://localhost:9876/task-runs/${taskId}/generate-summary`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
        },
      });

      if (!response.ok) {
        const errorText = await response.text();
        return { success: false, error: errorText };
      }

      return await response.json();
    } catch (error) {
      return {
        success: false,
        error: error instanceof Error ? error.message : "Failed to generate summary",
      };
    }
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

  // ===========================================================================
  // SQLite Migrated Logs (replaces JSONL for historical queries)
  // ===========================================================================

  /**
   * Get task run events from SQLite database.
   * This replaces JSONL file reading for historical analysis.
   * @param taskRunId - Task run ID to get events for
   * @param eventType - Optional event type filter ('general', 'action', 'image_recognition', etc.)
   * @param limit - Maximum number of events to return (default: 1000)
   */
  async getTaskRunEvents(
    taskRunId: string,
    eventType?: string,
    limit?: number,
  ): Promise<AiDataResponse<TaskRunEventsResult>> {
    return invoke("get_task_run_events_from_db", { taskRunId, eventType, limit });
  },

  /**
   * Get task run screenshots from SQLite database.
   * @param taskRunId - Task run ID to get screenshots for
   * @param screenshotType - Optional filter by screenshot type ('annotated', 'raw', 'diff', 'failure')
   */
  async getTaskRunScreenshotsFromDb(
    taskRunId: string,
    screenshotType?: string,
  ): Promise<AiDataResponse<TaskRunScreenshotsDbResult>> {
    return invoke("get_task_run_screenshots_from_db", { taskRunId, screenshotType });
  },

  /**
   * Get Playwright test results from SQLite database.
   * @param taskRunId - Task run ID to get results for
   */
  async getTaskRunPlaywrightResults(
    taskRunId: string,
  ): Promise<AiDataResponse<TaskRunPlaywrightResultsDbResult>> {
    return invoke("get_task_run_playwright_results_from_db", { taskRunId });
  },

  /**
   * Get summary of all migrated log data for a task run.
   * @param taskRunId - Task run ID to get summary for
   */
  async getTaskRunMigratedLogsSummary(
    taskRunId: string,
  ): Promise<AiDataResponse<TaskRunMigratedLogsSummary>> {
    return invoke("get_task_run_migrated_logs_summary", { taskRunId });
  },

  /**
   * Get API requests for a task run from SQLite database.
   * @param taskRunId - Task run ID to get API requests for
   * @param successFilter - Optional filter by success status (true = success only, false = failures only)
   */
  async getTaskRunApiRequests(
    taskRunId: string,
    successFilter?: boolean,
  ): Promise<AiDataResponse<TaskRunApiRequestsDbResult>> {
    return invoke("get_task_run_api_requests_from_db", { taskRunId, successFilter });
  },

  /**
   * Get AWAS steps for a task run from SQLite database.
   * @param taskRunId - Task run ID to get AWAS steps for
   * @param stepType - Optional filter by step type ('awas_discover', 'awas_execute', etc.)
   */
  async getTaskRunAwasSteps(
    taskRunId: string,
    stepType?: string,
  ): Promise<AiDataResponse<TaskRunAwasStepsDbResult>> {
    return invoke("get_task_run_awas_steps_from_db", { taskRunId, stepType });
  },

  /**
   * Get MCP calls for a task run from SQLite database.
   * @param taskRunId - Task run ID to get MCP calls for
   * @param successFilter - Optional filter by success status (true = success only, false = failures only)
   */
  async getTaskRunMcpCalls(
    taskRunId: string,
    successFilter?: boolean,
  ): Promise<AiDataResponse<TaskRunMcpCallsDbResult>> {
    return invoke("get_task_run_mcp_calls", { taskRunId, successFilter });
  },
};
