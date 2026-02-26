/**
 * Scheduler Types
 *
 * Re-exported from @qontinui/shared-types/scheduler.
 * Helper functions re-exported from @qontinui/workflow-utils.
 */

// =============================================================================
// Types from shared-types package
// =============================================================================

export type {
  ScheduleOnce,
  ScheduleCron,
  ScheduleInterval,
  ScheduleState,
  ScheduleExpression,
  IdleCondition,
  RepositoryWatch,
  RepositoryInactiveCondition,
  ScheduleConditions,
  ConditionStatus,
  WorkflowTask,
  PromptTask,
  AutoFixTask,
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
