/**
 * Shared Utility Functions for Step Widgets
 *
 * Extracted from duplicated patterns across widget data hooks:
 * - useCommandData
 * - useExecutionTimelineData
 * - useVerificationData
 * - useUiBridgeData
 */

import type { StepStats, StepExecutionStatus } from "./types";
import type { StepType } from "../execution-timeline/types";
import type { WorkflowStage } from "@/types/dashboard/activity-types";
import { getStatusColors, type StatusType, type StatusColorClasses } from "@/design-system/colors";

// ---------------------------------------------------------------------------
// Duration Formatting
// ---------------------------------------------------------------------------

/**
 * Format a duration in milliseconds to a human-readable string.
 *
 * Handles the full range from milliseconds to hours.
 * Returns an empty string for null/undefined inputs.
 * Returns "0ms" for a zero value (not an empty string).
 *
 * Extracted from ExecutionTimelineWidget and TimelineStatsBar to
 * eliminate duplication.
 *
 * @param ms - Duration in milliseconds (may be null/undefined)
 * @returns Formatted string like "150ms", "3.2s", "2m 15s", or "1h 30m"
 */
export function formatDuration(ms: number | null | undefined): string {
  if (ms == null) return "";
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  const mins = Math.floor(ms / 60000);
  const secs = Math.floor((ms % 60000) / 1000);
  if (mins < 60) return `${mins}m ${secs}s`;
  const hours = Math.floor(mins / 60);
  const remainingMins = mins % 60;
  return `${hours}h ${remainingMins}m`;
}

// ---------------------------------------------------------------------------
// Step Status to Design System Mapping
// ---------------------------------------------------------------------------

/**
 * Maps StepExecutionStatus values to the design system's StatusType.
 *
 * This provides a single source of truth so that all widgets use the
 * same colors for a given step status.
 */
const STEP_STATUS_TO_DESIGN_STATUS: Record<StepExecutionStatus, StatusType> = {
  success: "success",
  failed: "error",
  running: "running",
  pending: "pending",
  skipped: "cancelled",
  unknown: "idle",
};

/**
 * Get design-system status colors for a StepExecutionStatus.
 *
 * Prefer this over calling getStatusColors() directly when you have
 * a StepExecutionStatus value, since the mapping from step status
 * to design status is non-trivial (e.g. "failed" -> "error",
 * "skipped" -> "cancelled").
 */
export function getStepStatusColors(status: StepExecutionStatus): StatusColorClasses {
  return getStatusColors(STEP_STATUS_TO_DESIGN_STATUS[status]);
}

// ---------------------------------------------------------------------------
// API Response Type
// ---------------------------------------------------------------------------

/**
 * Response from the current-execution/steps API.
 *
 * This is the superset of all fields used by the various widget hooks.
 * Individual hooks may only use a subset of these fields.
 */
export interface CurrentExecutionStepsResponse {
  success: boolean;
  task_run_id: string | null;
  workflow_name?: string;
  workflow_type?: string;
  workflow_start_time?: string;
  current_stage?: WorkflowStage;
  executions: Array<{
    id: string;
    step_type: string;
    step_name: string;
    status: string;
    command_mode?: string;
    phase?: string;
    step_index?: number;
    iteration?: number;
    /** Stage index for multi-stage workflows (0-based). Null/undefined for single-stage. */
    stage_index?: number;
    start_time?: number;
    end_time?: number;
    duration_ms?: number;
    error?: string;
    output?: string;
    command?: string;
    working_directory?: string;
    exit_code?: number;
    stdout?: string;
    stderr?: string;
    template_command?: string;
    resolved_variables?: Record<string, unknown>;
  }>;
  count: number;
  /** Batch endpoint fields — present when fetching from /current-execution/batch */
  completed_iterations?: number[];
  has_setup?: boolean;
  has_verification?: boolean;
  has_agentic?: boolean;
  /** Multi-stage workflow fields */
  current_stage_index?: number;
  total_stages?: number;
  stage_names?: string[];
}

// ---------------------------------------------------------------------------
// Step Stats Calculation
// ---------------------------------------------------------------------------

/**
 * Default success rate when there are no completed items.
 *
 * Most widgets default to 100 (optimistic: nothing has failed yet).
 * The execution timeline defaults to 0 (conservative: nothing has passed yet).
 */
export type EmptySuccessRate = 0 | 100;

/**
 * Calculate step execution statistics from an array of items with a status field.
 *
 * Extracted from the identical pattern found in useCommandData, useVerificationData,
 * useUiBridgeData, and useExecutionTimelineData.
 *
 * @param items - Array of items with a `status` string field
 * @param elapsedTime - Total elapsed time in seconds (defaults to 0)
 * @param emptySuccessRate - Success rate to use when no items are completed.
 *   100 (default) = optimistic (used by command, verification, ui-bridge widgets).
 *   0 = conservative (used by execution-timeline widget).
 */
export function calculateStepStats(
  items: ReadonlyArray<{ status: string }>,
  elapsedTime: number = 0,
  emptySuccessRate: EmptySuccessRate = 100,
): StepStats {
  const total = items.length;
  const completed = items.filter(
    (item) => item.status === "success" || item.status === "failed",
  ).length;
  const successful = items.filter((item) => item.status === "success").length;
  const failed = items.filter((item) => item.status === "failed").length;
  const pending = total - completed;
  const successRate = completed > 0 ? (successful / completed) * 100 : emptySuccessRate;

  return { total, completed, successful, failed, pending, elapsedTime, successRate };
}

// ---------------------------------------------------------------------------
// Start Time Detection
// ---------------------------------------------------------------------------

/**
 * Find the earliest start_time from a list of items.
 *
 * Extracted from the identical pattern found in useCommandData, useVerificationData,
 * and useUiBridgeData.
 *
 * @param items - Array of items with an optional `start_time` or `startTime` field
 * @returns The earliest timestamp in ms since epoch, or null if none found
 */
export function detectStartTime(
  items: ReadonlyArray<{ start_time?: number; startTime?: number }>,
): number | null {
  const times = items
    .map((item) => item.start_time ?? item.startTime)
    .filter((t): t is number => t !== undefined && t !== null);

  if (times.length === 0) return null;

  const earliest = Math.min(...times);
  return Number.isFinite(earliest) ? earliest : null;
}

// ---------------------------------------------------------------------------
// Step Type Mapping
// ---------------------------------------------------------------------------

/**
 * Map from API step_type strings to the display StepType enum.
 *
 * Aligned with backend StepType enum in src-tauri/src/step_types.rs.
 * Extracted from useExecutionTimelineData.
 */
const STEP_TYPE_MAP: Record<string, StepType> = {
  // GUI Automation
  workflow: "workflow",
  gui_workflow: "workflow",
  state: "state",
  action: "action",
  screenshot: "screenshot",
  gui_action: "gui_action",
  gui_automation: "gui_action",
  workflow_ref: "workflow_ref",

  // Verification
  playwright: "playwright",
  test: "test",
  check: "check",
  check_group: "check",
  error_check: "check",
  log_check: "check",

  // Command (unified: shell commands, API requests, MCP calls)
  shell: "shell",
  shell_command: "shell",
  command: "shell",
  api_request: "shell",
  api: "shell",
  http: "shell",
  mcp_call: "shell",
  mcp: "shell",

  // AI
  prompt: "prompt",
  ai_prompt: "prompt",
  ai_session: "ai_session",
  ai_analysis: "ai_session",
  agentic: "ai_session",

  // AWAS (Web Automation)
  awas_discover: "awas",
  awas_execute: "awas",
  awas_check_support: "awas",
  awas_list_actions: "awas",
  awas_extract_elements: "awas",

  // Utility
  macro: "macro",
  script: "script",
};

/**
 * Map an API step_type string to the display StepType.
 *
 * Extracted from useExecutionTimelineData.
 *
 * @param apiType - The raw step_type string from the API
 * @returns The corresponding StepType, or "unknown" if not recognized
 */
export function mapStepType(apiType: string): StepType {
  return STEP_TYPE_MAP[apiType.toLowerCase()] || "unknown";
}

// ---------------------------------------------------------------------------
// Step Status Inference
// ---------------------------------------------------------------------------

/**
 * Infer whether a step is currently running based on its timestamps and status.
 *
 * If the API reports a non-terminal status and the step has started but not finished,
 * we infer it as "running". Terminal statuses (success, failed, skipped) are never
 * overridden.
 *
 * Extracted from useExecutionTimelineData.
 *
 * @param execution - Object with optional timestamps and a status string
 * @returns The inferred StepExecutionStatus
 */
const KNOWN_STATUSES: Set<string> = new Set([
  "success",
  "failed",
  "skipped",
  "running",
  "pending",
  "unknown",
]);

export function inferStepStatus(execution: {
  start_time?: number;
  end_time?: number;
  duration_ms?: number;
  status: string;
}): StepExecutionStatus {
  const status = execution.status as StepExecutionStatus;

  // Terminal statuses are never overridden
  const isTerminalStatus = status === "success" || status === "failed" || status === "skipped";

  if (!isTerminalStatus && execution.start_time && !execution.end_time && !execution.duration_ms) {
    return "running";
  }

  // Fall back to "unknown" for unrecognized status values
  return KNOWN_STATUSES.has(status) ? status : "unknown";
}

// ---------------------------------------------------------------------------
// Phase Mapping
// ---------------------------------------------------------------------------

/**
 * Map from API phase strings to WorkflowStage values.
 */
const PHASE_MAP: Record<string, WorkflowStage> = {
  setup: "setup",
  setup_steps: "setup",
  agentic: "agentic",
  agentic_steps: "agentic",
  verification: "verification",
  verification_steps: "verification",
  completion: "completion",
  completion_steps: "completion",
};

/**
 * Map an API phase string to a WorkflowStage.
 *
 * If no phase is provided or the phase is not recognized, returns the fallback stage.
 * Extracted from useExecutionTimelineData.
 *
 * @param apiPhase - The raw phase string from the API (may be undefined)
 * @param fallbackStage - The WorkflowStage to use when apiPhase is missing or unrecognized.
 *   Defaults to "setup".
 * @returns The corresponding WorkflowStage
 */
export function mapPhase(
  apiPhase: string | undefined,
  fallbackStage: WorkflowStage = "setup",
): WorkflowStage {
  if (!apiPhase) return fallbackStage;
  return PHASE_MAP[apiPhase.toLowerCase()] || fallbackStage;
}
