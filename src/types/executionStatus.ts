/**
 * Execution Status Types
 *
 * Re-exported from @qontinui/shared-types/execution.
 * Default factory functions and helpers re-exported from @qontinui/workflow-utils.
 */

// =============================================================================
// Types from shared-types package
// =============================================================================

export type {
  TaskComplexity,
  RoutingFactor,
  RoutingDecision,
  RoutingStatus,
  RetryAttempt,
  RetryState,
  RetryStatus,
  TokenCount,
  CompressionResult,
  CompressionStatus,
  HookTrigger,
  HookExecutionResult,
  HookDefinition,
  HookStatus,
  SubStepStatus,
  SubStepInfo,
  ExecutionStatus,
  // Raw event types
  RawExecutionStatusEventBase,
  RawRoutingDecisionPayload,
  RawRetryAttemptPayload,
  RawRetryStatePayload,
  RawTokenCountPayload,
  RawCompressionResultPayload,
  RawHookExecutionPayload,
  RawRoutingDecisionEvent,
  RawRetryAttemptEvent,
  RawCompressionEvent,
  RawTokenCountUpdateEvent,
  RawHookExecutionEvent,
  RawHookStartedEvent,
  RawStatusChangeEvent,
  RawExecutionStatusEvent,
  // Sub-step raw events
  RawSubStepCompleteEvent,
  RawSubStepStartedEvent,
} from "@qontinui/shared-types/execution";

// Re-export SubStepStatusDisplay as SubStepStatus_Display for backward compatibility
export type { SubStepStatusDisplay as SubStepStatus_Display } from "@qontinui/shared-types/execution";

// =============================================================================
// Default factory functions and helpers from workflow-utils package
// =============================================================================

export {
  createDefaultRoutingStatus,
  createDefaultRetryStatus,
  createDefaultCompressionStatus,
  createDefaultHookStatus,
  createDefaultSubStepStatus,
  createDefaultExecutionStatus,
  getComplexityDisplayName,
  getHookTriggerDisplayName,
  calculateCompressionSavings,
  formatTokenCount,
} from "@qontinui/workflow-utils";

// Re-export formatDuration as formatDurationMs for backward compatibility
export { formatDuration as formatDurationMs } from "@qontinui/workflow-utils";
