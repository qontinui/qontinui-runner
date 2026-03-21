/**
 * Error Monitor Service
 *
 * Provides Tauri invoke calls for the error monitoring system.
 * Wraps the Tauri commands from src-tauri/src/error_monitor/commands.rs
 *
 * Note: Log source CRUD has been removed. Log sources are now managed through
 * global log source settings (Settings > Log Sources). See LogSourcesSettings.tsx.
 */

import { invoke } from "@tauri-apps/api/core";
import { createLogger } from "@/lib/logger";
import type {
  StoredErrorEvent,
  ErrorQuery,
  ErrorSummary,
  DebugContext,
  ErrorFixWorkflowConfig,
  GeneratedWorkflow,
  FixableErrorsSummary,
  ErrorStatus,
  LogSourceConfig,
} from "../types/errorMonitor";

/**
 * Service for accessing error monitoring data via Tauri commands.
 */
const logger = createLogger("ErrorMonitor");

export const errorMonitorService = {
  // ===========================================================================
  // Error Event Commands
  // ===========================================================================

  /**
   * Query error events with filters.
   */
  async queryErrorEvents(query: ErrorQuery): Promise<StoredErrorEvent[]> {
    logger.debug("Querying error events with:", query);
    try {
      const result = await invoke<StoredErrorEvent[]>("query_error_events", { query });
      logger.debug("Query result:", result?.length, "events");
      return result;
    } catch (err) {
      console.error("[ERROR_MONITOR] Query failed:", err);
      throw err;
    }
  },

  /**
   * Get a single error event by ID.
   */
  async getErrorEvent(id: number): Promise<StoredErrorEvent | null> {
    return invoke("get_error_event", { id });
  },

  /**
   * Get unresolved error events.
   */
  async getUnresolvedErrors(taskRunId?: string, limit?: number): Promise<StoredErrorEvent[]> {
    return invoke("get_unresolved_errors", { taskRunId, limit });
  },

  /**
   * Update the status of an error event.
   */
  async updateErrorStatus(
    id: number,
    status: ErrorStatus,
    resolutionNotes?: string,
  ): Promise<void> {
    return invoke("update_error_status", {
      id,
      status,
      resolutionNotes,
    });
  },

  /**
   * Acknowledge an error (mark as seen).
   */
  async acknowledgeError(id: number): Promise<void> {
    return invoke("acknowledge_error", { id });
  },

  /**
   * Mark an error as resolved.
   */
  async resolveError(
    id: number,
    resolutionNotes?: string,
    resolvedByTaskRunId?: string,
  ): Promise<void> {
    return invoke("resolve_error", {
      id,
      resolutionNotes,
      resolvedByTaskRunId,
    });
  },

  /**
   * Ignore an error.
   */
  async ignoreError(id: number, reason?: string): Promise<void> {
    return invoke("ignore_error", { id, reason });
  },

  /**
   * Link an error to a finding.
   */
  async linkErrorToFinding(errorId: number, findingId: number): Promise<void> {
    return invoke("link_error_to_finding", { errorId, findingId });
  },

  // ===========================================================================
  // Summary and Statistics Commands
  // ===========================================================================

  /**
   * Get error summary statistics.
   */
  async getErrorSummary(taskRunId?: string): Promise<ErrorSummary> {
    return invoke("get_error_summary", { taskRunId });
  },

  /**
   * Search error events by message content.
   */
  async searchErrors(query: string, limit?: number): Promise<StoredErrorEvent[]> {
    return invoke("search_errors", { query, limit });
  },

  /**
   * Check if there are actionable errors.
   */
  async hasActionableErrors(taskRunId?: string): Promise<boolean> {
    return invoke("has_actionable_errors", { taskRunId });
  },

  /**
   * Get recent errors (for notification badge).
   */
  async getRecentErrors(hours?: number, limit?: number): Promise<StoredErrorEvent[]> {
    return invoke("get_recent_errors", { hours, limit });
  },

  /**
   * Bulk acknowledge all new errors.
   * @returns Number of errors acknowledged
   */
  async acknowledgeAllErrors(taskRunId?: string): Promise<number> {
    return invoke("acknowledge_all_errors", { taskRunId });
  },

  // ===========================================================================
  // Debug Context Commands
  // ===========================================================================

  /**
   * Get curated debug context for the AI debug agent.
   */
  async getDebugContext(taskRunId?: string, maxErrors?: number): Promise<DebugContext> {
    return invoke("get_debug_context", { taskRunId, maxErrors });
  },

  /**
   * Get debug context formatted as a string for AI consumption.
   */
  async getDebugContextForAi(taskRunId?: string): Promise<string> {
    return invoke("get_debug_context_for_ai", { taskRunId });
  },

  // ===========================================================================
  // Workflow Generation Commands
  // ===========================================================================

  /**
   * Generate an error fix workflow.
   */
  async generateErrorFixWorkflow(
    config?: Partial<ErrorFixWorkflowConfig>,
  ): Promise<GeneratedWorkflow> {
    return invoke("generate_error_fix_workflow", { config });
  },

  /**
   * Generate a workflow to fix a single error.
   */
  async generateSingleErrorFixWorkflow(errorId: number): Promise<GeneratedWorkflow> {
    return invoke("generate_single_error_fix_workflow", { errorId });
  },

  /**
   * Check if there are fixable errors and get summary.
   */
  async checkFixableErrors(taskRunId?: string): Promise<FixableErrorsSummary> {
    return invoke("check_fixable_errors", { taskRunId });
  },
};

// ===========================================================================
// Helper Functions
// ===========================================================================

/**
 * Get display color for error severity.
 */
export function getSeverityColor(severity: string): string {
  switch (severity) {
    case "critical":
      return "text-red-600";
    case "error":
      return "text-red-500";
    case "warning":
      return "text-yellow-500";
    case "info":
      return "text-blue-500";
    case "debug":
      return "text-gray-500";
    default:
      return "text-muted-foreground";
  }
}

/**
 * Get background color for error severity.
 */
export function getSeverityBgColor(severity: string): string {
  switch (severity) {
    case "critical":
      return "bg-red-600/20";
    case "error":
      return "bg-red-500/20";
    case "warning":
      return "bg-yellow-500/20";
    case "info":
      return "bg-blue-500/20";
    case "debug":
      return "bg-gray-500/20";
    default:
      return "bg-muted";
  }
}

/**
 * Get display color for error status.
 */
export function getStatusColor(status: string): string {
  switch (status) {
    case "new":
      return "text-red-500";
    case "acknowledged":
      return "text-yellow-500";
    case "in_progress":
      return "text-blue-500";
    case "resolved":
      return "text-green-500";
    case "ignored":
      return "text-gray-500";
    case "recurring":
      return "text-orange-500";
    default:
      return "text-muted-foreground";
  }
}

/**
 * Format error timestamp for display.
 */
export function formatErrorTime(timestamp: string): string {
  const date = new Date(timestamp);
  const now = new Date();
  const diffMs = now.getTime() - date.getTime();
  const diffMins = Math.floor(diffMs / 60000);
  const diffHours = Math.floor(diffMs / 3600000);
  const diffDays = Math.floor(diffMs / 86400000);

  if (diffMins < 1) {
    return "just now";
  }
  if (diffMins < 60) {
    return `${diffMins}m ago`;
  }
  if (diffHours < 24) {
    return `${diffHours}h ago`;
  }
  if (diffDays < 7) {
    return `${diffDays}d ago`;
  }
  return date.toLocaleDateString();
}

/**
 * Get parser type display name.
 */
export function getParserDisplayName(parser: LogSourceConfig["parser"]): string {
  if (typeof parser === "string") {
    return parser.charAt(0).toUpperCase() + parser.slice(1);
  }
  return "Custom";
}

// Re-export types for convenience
export type { LogSourceConfig } from "../types/errorMonitor";
