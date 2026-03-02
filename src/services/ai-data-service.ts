/**
 * AI Data Service
 *
 * Provides Tauri invoke calls for the AI Data Viewer.
 * Wraps the ai_data Tauri commands from src-tauri/src/commands/ai_data.rs.
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
  LoadedConfigInfo,
  AiPromptsResult,
  ContextsResult,
  ConsolidatedAiOutputResult,
  // SQLite migrated log types
  TaskRunEventsResult,
  TaskRunPlaywrightResultsDbResult,
  TaskRunMigratedLogsSummary,
  TaskRunApiRequestsDbResult,
  TaskRunAwasStepsDbResult,
  TaskRunVerificationResultsDbResult,
  // Process session types
  ProcessSession,
  ProcessSessionOutputLine,
} from "../types/aiData";
import type { TaskRunMcpCallsDbResult } from "../types/mcp-config";
import { getApiBase } from "@/lib/runner-api";

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
   * Resume a finished task run with additional iterations.
   * This reopens the task and triggers workflow execution.
   * @param taskId - Task run ID
   * @param additionalSessions - Number of additional sessions to add
   */
  async reopenTaskRun(
    taskId: string,
    additionalSessions: number,
  ): Promise<AiDataResponse<TaskRun>> {
    try {
      const response = await fetch(`${getApiBase()}/task-runs/${taskId}/resume`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ additional_sessions: additionalSessions }),
      });
      const result = await response.json();
      if (!result.success) {
        return { success: false, error: result.error || "Failed to resume task run" };
      }
      // Fetch the updated task run to return
      return invoke("get_task_run_for_viewer", { taskId });
    } catch (error) {
      return { success: false, error: String(error) };
    }
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
      const response = await fetch(`${getApiBase()}/task-runs/${taskId}/generate-summary`, {
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

  /**
   * Get verification phase results for a task run from SQLite database.
   * Returns results from all verification iterations including individual test/check results.
   * @param taskRunId - Task run ID to get verification results for
   */
  async getTaskRunVerificationResults(
    taskRunId: string,
  ): Promise<AiDataResponse<TaskRunVerificationResultsDbResult>> {
    return invoke("get_task_run_verification_results_from_db", { taskRunId });
  },

  // ===========================================================================
  // Process Sessions (persistent history)
  // ===========================================================================

  /**
   * Get process sessions from database.
   * @param configId - Optional process config ID to filter by
   * @param limit - Maximum number of sessions to return (default: 50)
   */
  async getProcessSessions(
    configId?: string,
    limit?: number,
  ): Promise<AiDataResponse<ProcessSession[]>> {
    try {
      const data = await invoke<ProcessSession[]>("get_process_sessions_from_db", {
        configId: configId || null,
        limit: limit || 50,
      });
      return { success: true, data };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  },

  /**
   * Get process session output from database.
   * @param sessionId - Session ID to get output for
   * @param limit - Maximum number of lines to return (default: 5000)
   * @param offset - Offset for pagination (default: 0)
   */
  async getProcessSessionOutput(
    sessionId: string,
    limit?: number,
    offset?: number,
  ): Promise<AiDataResponse<ProcessSessionOutputLine[]>> {
    try {
      const data = await invoke<ProcessSessionOutputLine[]>("get_process_session_output_from_db", {
        sessionId,
        limit: limit || 5000,
        offset: offset || 0,
      });
      return { success: true, data };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  },
};
