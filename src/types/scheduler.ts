/**
 * Scheduler Types
 *
 * TypeScript interfaces for the CI/CD scheduler system.
 */

// ============================================================================
// Schedule Expression Types
// ============================================================================

/**
 * Run once at a specific datetime (ISO 8601)
 */
export interface ScheduleOnce {
  type: "Once";
  value: string;
}

/**
 * Cron expression (e.g., "0 9 * * *" for 9 AM daily)
 */
export interface ScheduleCron {
  type: "Cron";
  value: string;
}

/**
 * Interval in seconds (for testing/debugging)
 */
export interface ScheduleInterval {
  type: "Interval";
  value: number;
}

/**
 * How a task should be scheduled
 */
export type ScheduleExpression = ScheduleOnce | ScheduleCron | ScheduleInterval;

// ============================================================================
// Schedule Conditions
// ============================================================================

/**
 * Condition that requires the runner to be idle
 */
export interface IdleCondition {
  enabled: boolean;
}

/**
 * A single repository to monitor for inactivity
 */
export interface RepositoryWatch {
  /** Path to the repository directory */
  path: string;
  /** Minutes of inactivity required before condition is met */
  inactive_minutes: number;
}

/**
 * Condition that requires repositories to have no file modifications
 */
export interface RepositoryInactiveCondition {
  enabled: boolean;
  /** List of repositories to watch (ALL must be inactive for condition to be met) */
  repositories: RepositoryWatch[];
}

/**
 * Conditions that must ALL be met before task execution
 */
export interface ScheduleConditions {
  /** Require runner to be idle (not executing workflows or AI tasks) */
  require_idle?: IdleCondition;
  /** Require repository file inactivity */
  require_repo_inactive?: RepositoryInactiveCondition;
  /** Maximum time to wait for conditions (minutes). Undefined = wait indefinitely */
  timeout_minutes?: number;
}

/**
 * Status of condition checking for a deferred task
 */
export interface ConditionStatus {
  /** Time when conditions started being checked (ISO 8601) */
  waiting_since: string;
  /** Current idle condition status */
  idle_met?: boolean;
  /** Current repo inactive status per repository: [path, is_inactive] */
  repo_inactive_met?: Array<[string, boolean]>;
  /** Whether timeout has been exceeded */
  timed_out: boolean;
}

// ============================================================================
// Task Type Definitions
// ============================================================================

/**
 * Run a workflow from loaded config
 */
export interface WorkflowTask {
  task_type: "Workflow";
  workflow_name: string;
  config_path?: string;
  monitor_index?: number;
}

/**
 * Run a prompt from Prompt Library
 */
export interface PromptTask {
  task_type: "Prompt";
  prompt_id: string;
  /** Optional override for max_sessions (null = use prompt's setting) */
  max_sessions?: number;
}

/**
 * Trigger auto-fix (check findings and fix auto-fixable items)
 */
export interface AutoFixTask {
  task_type: "AutoFix";
  check_findings: boolean;
  force_run: boolean;
}

/**
 * Type of task to schedule
 */
export type ScheduledTaskType = WorkflowTask | PromptTask | AutoFixTask;

// ============================================================================
// Task Status
// ============================================================================

/**
 * Status of a scheduled task execution
 */
export type ScheduledTaskStatus =
  | "pending"
  | "running"
  | "completed"
  | "failed"
  | "skipped"
  | "cancelled";

// ============================================================================
// Execution Record
// ============================================================================

/**
 * Record of a single task execution
 */
export interface TaskExecutionRecord {
  /** Unique ID for this execution */
  execution_id: string;
  /** Session ID if this triggered an AI session (for success tracking) */
  session_id?: string;
  /** ISO 8601 timestamp when execution started */
  started_at: string;
  /** ISO 8601 timestamp when execution ended */
  ended_at?: string;
  /** Current status */
  status: ScheduledTaskStatus;
  /** Whether the task succeeded (read from session checkpoint) */
  success: boolean;
  /** Error message if failed */
  error_message?: string;
  /** Whether auto-fix was triggered after this execution */
  triggered_auto_fix: boolean;
  /** Session ID of the auto-fix session if triggered */
  auto_fix_session_id?: string;
}

// ============================================================================
// Scheduled Task
// ============================================================================

/**
 * A scheduled task definition
 */
export interface ScheduledTask {
  /** Unique identifier (UUID v4) */
  id: string;
  /** Display name for the task */
  name: string;
  /** Optional description */
  description?: string;
  /** Whether the task is enabled */
  enabled: boolean;
  /** Schedule configuration */
  schedule: ScheduleExpression;
  /** Task type and configuration */
  task: ScheduledTaskType;
  /** Whether to skip if task has already succeeded */
  skip_if_completed: boolean;
  /** Auto-trigger auto-fix on failure */
  auto_fix_on_failure: boolean;
  /** Success criteria description (for reference) */
  success_criteria?: string;
  /** ISO 8601 timestamp of creation */
  created_at: string;
  /** ISO 8601 timestamp of last modification */
  modified_at: string;
  /** Last execution record */
  last_run?: TaskExecutionRecord;
  /** Next scheduled run time (computed) */
  next_run?: string;
  /** Optional conditions that must be met before execution */
  conditions?: ScheduleConditions;
  /** Status when task is waiting for conditions to be met */
  condition_status?: ConditionStatus;
}

// ============================================================================
// Scheduler Settings
// ============================================================================

/**
 * Global scheduler settings
 */
export interface SchedulerSettings {
  /** Scheduler enabled globally */
  enabled: boolean;
  /** Maximum concurrent scheduled tasks */
  max_concurrent: number;
  /** Default auto-fix on failure setting for new tasks */
  default_auto_fix_on_failure: boolean;
  /** Timezone for schedule interpretation (default: local) */
  timezone?: string;
}

// ============================================================================
// Scheduler Status
// ============================================================================

/**
 * Information about the next task to run
 */
export interface NextTaskInfo {
  id: string;
  name: string;
  next_run: string;
}

/**
 * Current scheduler status
 */
export interface SchedulerStatus {
  enabled: boolean;
  running_tasks: number;
  pending_tasks: number;
  next_task?: NextTaskInfo;
}

// ============================================================================
// API Request/Response Types
// ============================================================================

/**
 * Request to create a scheduled task
 */
export interface CreateScheduledTaskRequest {
  name: string;
  description?: string;
  schedule: ScheduleExpression;
  task: ScheduledTaskType;
  skip_if_completed?: boolean;
  auto_fix_on_failure?: boolean;
  success_criteria?: string;
  conditions?: ScheduleConditions;
}

/**
 * Request to update a scheduled task
 */
export interface UpdateScheduledTaskRequest {
  name?: string;
  description?: string | null;
  enabled?: boolean;
  schedule?: ScheduleExpression;
  task?: ScheduledTaskType;
  skip_if_completed?: boolean;
  auto_fix_on_failure?: boolean;
  success_criteria?: string | null;
  conditions?: ScheduleConditions | null;
}

// ============================================================================
// Helper Functions
// ============================================================================

/**
 * Get a human-readable description of a schedule expression
 */
export function describeSchedule(schedule: ScheduleExpression): string {
  switch (schedule.type) {
    case "Once":
      try {
        const date = new Date(schedule.value);
        return `Once at ${date.toLocaleString()}`;
      } catch {
        return `Once at ${schedule.value}`;
      }
    case "Cron":
      return describeCron(schedule.value);
    case "Interval":
      return describeInterval(schedule.value);
  }
}

/**
 * Get a human-readable description of a cron expression
 */
function describeCron(cron: string): string {
  // Common cron patterns
  const patterns: Record<string, string> = {
    "0 0 * * * *": "Every hour",
    "0 0 0 * * *": "Every day at midnight",
    "0 0 9 * * *": "Every day at 9 AM",
    "0 0 9 * * 1-5": "Weekdays at 9 AM",
    "0 0 0 * * 0": "Every Sunday at midnight",
    "0 0 0 1 * *": "First day of every month",
  };

  if (patterns[cron]) {
    return patterns[cron];
  }

  return `Cron: ${cron}`;
}

/**
 * Get a human-readable description of an interval
 */
function describeInterval(seconds: number): string {
  if (seconds < 60) {
    return `Every ${seconds} seconds`;
  } else if (seconds < 3600) {
    const minutes = Math.floor(seconds / 60);
    return `Every ${minutes} minute${minutes > 1 ? "s" : ""}`;
  } else if (seconds < 86400) {
    const hours = Math.floor(seconds / 3600);
    return `Every ${hours} hour${hours > 1 ? "s" : ""}`;
  } else {
    const days = Math.floor(seconds / 86400);
    return `Every ${days} day${days > 1 ? "s" : ""}`;
  }
}

/**
 * Get a human-readable description of a task type
 */
export function describeTaskType(task: ScheduledTaskType): string {
  switch (task.task_type) {
    case "Workflow":
      return `Workflow: ${task.workflow_name}`;
    case "Prompt":
      return `Prompt: ${task.prompt_id}`;
    case "AutoFix":
      return "Auto-Fix";
  }
}

/**
 * Get status color class for a task status
 */
export function getStatusColor(status: ScheduledTaskStatus): string {
  switch (status) {
    case "pending":
      return "text-muted-foreground";
    case "running":
      return "text-blue-500";
    case "completed":
      return "text-green-500";
    case "failed":
      return "text-red-500";
    case "skipped":
      return "text-yellow-500";
    case "cancelled":
      return "text-gray-500";
  }
}

/**
 * Check if a task is currently running
 */
export function isTaskRunning(task: ScheduledTask): boolean {
  return task.last_run?.status === "running";
}

/**
 * Check if a task has completed successfully at least once
 */
export function hasCompletedSuccessfully(task: ScheduledTask): boolean {
  return task.last_run?.success === true;
}

/**
 * Get time until next run in human-readable format
 */
export function getTimeUntilNextRun(task: ScheduledTask): string | null {
  if (!task.next_run) return null;

  try {
    const nextRun = new Date(task.next_run);
    const now = new Date();
    const diff = nextRun.getTime() - now.getTime();

    if (diff < 0) return "Overdue";

    const seconds = Math.floor(diff / 1000);
    if (seconds < 60) return `${seconds}s`;

    const minutes = Math.floor(seconds / 60);
    if (minutes < 60) return `${minutes}m`;

    const hours = Math.floor(minutes / 60);
    if (hours < 24) return `${hours}h ${minutes % 60}m`;

    const days = Math.floor(hours / 24);
    return `${days}d ${hours % 24}h`;
  } catch {
    return null;
  }
}

/**
 * Check if a task is currently waiting for conditions
 */
export function isWaitingForConditions(task: ScheduledTask): boolean {
  return task.condition_status !== undefined && task.condition_status !== null;
}

/**
 * Check if a task has any enabled conditions
 */
export function hasConditions(task: ScheduledTask): boolean {
  if (!task.conditions) return false;

  const idleEnabled = task.conditions.require_idle?.enabled ?? false;
  const repoEnabled =
    task.conditions.require_repo_inactive?.enabled === true &&
    (task.conditions.require_repo_inactive?.repositories?.length ?? 0) > 0;

  return idleEnabled || repoEnabled;
}

/**
 * Get a human-readable description of conditions
 */
export function describeConditions(conditions: ScheduleConditions): string {
  const parts: string[] = [];

  if (conditions.require_idle?.enabled) {
    parts.push("Wait for idle");
  }

  if (conditions.require_repo_inactive?.enabled) {
    const repos = conditions.require_repo_inactive.repositories;
    if (repos.length === 1) {
      parts.push(`Wait for repo inactive (${repos[0].inactive_minutes}min)`);
    } else if (repos.length > 1) {
      parts.push(`Wait for ${repos.length} repos inactive`);
    }
  }

  if (conditions.timeout_minutes) {
    parts.push(`timeout: ${conditions.timeout_minutes}min`);
  }

  return parts.length > 0 ? parts.join(", ") : "No conditions";
}

/**
 * Get condition status display text
 */
export function getConditionStatusText(status: ConditionStatus): string {
  if (status.timed_out) {
    return "Timed out";
  }

  const parts: string[] = [];

  if (status.idle_met !== undefined) {
    parts.push(status.idle_met ? "Idle" : "Waiting for idle");
  }

  if (status.repo_inactive_met) {
    const inactive = status.repo_inactive_met.filter(([, met]) => met).length;
    const total = status.repo_inactive_met.length;
    if (inactive === total) {
      parts.push("Repos inactive");
    } else {
      parts.push(`Repos: ${inactive}/${total} inactive`);
    }
  }

  return parts.length > 0 ? parts.join(", ") : "Checking conditions...";
}
