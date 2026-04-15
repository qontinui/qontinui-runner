/**
 * Scheduler Types
 *
 * Re-exported from @qontinui/shared-types/scheduler.
 * Helper functions re-exported from @qontinui/workflow-utils.
 */

// =============================================================================
// Types from shared-types package
// =============================================================================

// The generator (qontinui-schemas/rust/src/scheduler.rs →
// json-schema-to-typescript) emits only the parent tagged-union types; the
// per-variant aliases (`ScheduleOnce`, `WorkflowTask`, etc.) no longer exist
// as standalone names. Consumers narrow against the `type` / `task_type`
// discriminant instead.
export type {
  ScheduleExpression,
  IdleCondition,
  RepositoryWatch,
  RepositoryInactiveCondition,
  ScheduleConditions,
  ConditionStatus,
  ScheduledTaskType,
  ScheduledTaskStatus,
  TaskExecutionRecord,
  ScheduledTask,
  SchedulerSettings,
  NextTaskInfo,
  SchedulerStatus,
  CreateScheduledTaskRequest,
  UpdateScheduledTaskRequest,
} from "@qontinui/shared-types/scheduler";

// =============================================================================
// Helper functions from workflow-utils package
// =============================================================================

export {
  describeSchedule,
  describeTaskType,
  hasCompletedSuccessfully,
  getTimeUntilNextRun,
  isWaitingForConditions,
  hasConditions,
  describeConditions,
  getConditionStatusText,
} from "@qontinui/workflow-utils";

// Re-export with original local names for backward compatibility
export {
  getSchedulerStatusColor as getStatusColor,
  isScheduledTaskRunning as isTaskRunning,
} from "@qontinui/workflow-utils";
