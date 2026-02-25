/**
 * TaskRun Types
 *
 * Re-exported from @qontinui/shared-types/task-run.
 * Utility functions re-exported from @qontinui/workflow-utils.
 */

// =============================================================================
// Types from shared-types package
// =============================================================================

export type {
  TaskRunStatus,
  TaskType,
  TaskRun,
  RunPromptResponse,
  RunPromptRequest,
  CreateTaskRunRequest,
} from "@qontinui/shared-types/task-run";

// =============================================================================
// Utility functions from workflow-utils package
// =============================================================================

export {
  isTaskRunning,
  isTaskComplete,
  isTaskFailed,
  isTaskFinished,
} from "@qontinui/workflow-utils";
