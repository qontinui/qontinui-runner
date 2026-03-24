/**
 * Unified Execution Schema Types
 *
 * Re-exported from @qontinui/shared-types/execution.
 * Type definitions for the unified execution reporting API.
 */

// =============================================================================
// Enums
// =============================================================================

export {
  RunType,
  RunStatus,
  ActionStatus,
  ActionType,
  ErrorType,
  IssueSeverity,
  ScreenshotType,
} from "@qontinui/shared-types/execution";

// =============================================================================
// Interfaces
// =============================================================================

export type {
  LLMMetrics,
  RunnerMetadata,
  WorkflowMetadata,
  ExecutionStats,
  CoverageData,
  ExecutionRunCreate,
  ExecutionRunResponse,
  ActionExecutionCreate,
  ActionExecutionResponse,
  ExecutionScreenshotCreate,
  ExecutionScreenshotResponse,
  ExecutionIssueCreate,
  ExecutionIssueResponse,
  ExecutionRunComplete,
  ExecutionRunCompleteResponse,
} from "@qontinui/shared-types/execution";
