/**
 * Lifecycle Hooks Types
 *
 * TypeScript types for the lifecycle hooks system.
 * Matches the Rust types in orchestrator/hooks.rs and the database schema.
 */

// ============================================================================
// Hook Triggers
// ============================================================================

/**
 * Events that can trigger hook execution.
 */
export type HookTrigger =
  | "pre_execution"
  | "post_execution"
  | "on_error"
  | "on_verification_fail"
  | "on_complete"
  | "pre_iteration"
  | "post_iteration";

/**
 * Display names for hook triggers.
 */
export const HOOK_TRIGGER_DISPLAY_NAMES: Record<HookTrigger, string> = {
  pre_execution: "Pre-Execution",
  post_execution: "Post-Execution",
  on_error: "On Error",
  on_verification_fail: "On Verification Fail",
  on_complete: "On Complete",
  pre_iteration: "Pre-Iteration",
  post_iteration: "Post-Iteration",
};

/**
 * Descriptions for hook triggers.
 */
export const HOOK_TRIGGER_DESCRIPTIONS: Record<HookTrigger, string> = {
  pre_execution: "Runs before task execution starts",
  post_execution: "Runs after task execution completes (success or failure)",
  on_error: "Runs when an error occurs during execution",
  on_verification_fail: "Runs when verification fails",
  on_complete: "Runs when task completes successfully",
  pre_iteration: "Runs before each iteration",
  post_iteration: "Runs after each iteration",
};

// ============================================================================
// Hook Actions
// ============================================================================

/**
 * Types of actions a hook can perform.
 */
export type HookActionType = "command" | "webhook" | "log" | "notification";

/**
 * Command action configuration.
 */
export interface CommandAction {
  type: "command";
  command: string;
  working_dir?: string;
  timeout_seconds: number;
  env: Record<string, string>;
}

/**
 * Webhook action configuration.
 */
export interface WebhookAction {
  type: "webhook";
  url: string;
  method: string;
  headers: Record<string, string>;
  body?: string;
  timeout_seconds: number;
}

/**
 * Log action configuration.
 */
export interface LogAction {
  type: "log";
  level: string;
  message: string;
}

/**
 * Notification action configuration.
 */
export interface NotificationAction {
  type: "notification";
  title: string;
  body: string;
}

/**
 * Union type for all hook actions.
 */
export type HookAction = CommandAction | WebhookAction | LogAction | NotificationAction;

/**
 * Display names for hook action types.
 */
export const HOOK_ACTION_TYPE_DISPLAY_NAMES: Record<HookActionType, string> = {
  command: "Shell Command",
  webhook: "Webhook",
  log: "Log Message",
  notification: "System Notification",
};

/**
 * Descriptions for hook action types.
 */
export const HOOK_ACTION_TYPE_DESCRIPTIONS: Record<HookActionType, string> = {
  command: "Execute a shell command on the local system",
  webhook: "Send an HTTP request to a URL",
  log: "Log a message to the application logs",
  notification: "Send a system notification",
};

// ============================================================================
// Hook Conditions
// ============================================================================

/**
 * Operators for hook conditions.
 */
export type ConditionOperator =
  | "eq"
  | "ne"
  | "gt"
  | "lt"
  | "gte"
  | "lte"
  | "contains"
  | "starts_with"
  | "ends_with"
  | "matches";

/**
 * Display names for condition operators.
 */
export const CONDITION_OPERATOR_DISPLAY_NAMES: Record<ConditionOperator, string> = {
  eq: "equals",
  ne: "not equals",
  gt: "greater than",
  lt: "less than",
  gte: "greater than or equal",
  lte: "less than or equal",
  contains: "contains",
  starts_with: "starts with",
  ends_with: "ends with",
  matches: "matches (regex)",
};

/**
 * Available context variables for conditions.
 */
export const CONDITION_VARIABLES = [
  { name: "task_run_id", description: "Unique identifier for the task run" },
  { name: "task_name", description: "Name of the task" },
  { name: "iteration", description: "Current iteration number (1-based)" },
  { name: "status", description: "Current execution status" },
  { name: "error", description: "Error message (if any)" },
] as const;

/**
 * A condition for hook execution.
 */
export interface HookCondition {
  variable: string;
  operator: ConditionOperator;
  value: string | number | boolean;
}

// ============================================================================
// Hook Definition
// ============================================================================

/**
 * A lifecycle hook definition stored in the database.
 */
export interface Hook {
  id: string;
  name: string;
  description?: string;
  trigger: HookTrigger;
  action_type: HookActionType;
  action_config: HookAction;
  enabled: boolean;
  execution_order: number;
  continue_on_failure: boolean;
  conditions: HookCondition[];
  task_run_id?: string; // NULL = global hook
  created_at: string;
  updated_at: string;
}

/**
 * Request to create a new hook.
 */
export interface CreateHookRequest {
  name: string;
  description?: string;
  trigger: HookTrigger;
  action_type: HookActionType;
  action_config: HookAction;
  enabled?: boolean;
  execution_order?: number;
  continue_on_failure?: boolean;
  conditions?: HookCondition[];
  task_run_id?: string;
}

/**
 * Request to update an existing hook.
 */
export interface UpdateHookRequest {
  id: string;
  name?: string;
  description?: string;
  trigger?: HookTrigger;
  action_type?: HookActionType;
  action_config?: HookAction;
  enabled?: boolean;
  execution_order?: number;
  continue_on_failure?: boolean;
  conditions?: HookCondition[];
}

// ============================================================================
// Hook Execution Results
// ============================================================================

/**
 * Result of executing a hook.
 */
export interface HookExecutionResult {
  hook_id: string;
  hook_name: string;
  success: boolean;
  output?: string;
  error?: string;
  duration_ms: number;
  executed_at: string;
}

// ============================================================================
// Default Values
// ============================================================================

/**
 * Default command action configuration.
 */
export function createDefaultCommandAction(): CommandAction {
  return {
    type: "command",
    command: "",
    timeout_seconds: 30,
    env: {},
  };
}

/**
 * Default webhook action configuration.
 */
export function createDefaultWebhookAction(): WebhookAction {
  return {
    type: "webhook",
    url: "",
    method: "POST",
    headers: {},
    timeout_seconds: 30,
  };
}

/**
 * Default log action configuration.
 */
export function createDefaultLogAction(): LogAction {
  return {
    type: "log",
    level: "info",
    message: "",
  };
}

/**
 * Default notification action configuration.
 */
export function createDefaultNotificationAction(): NotificationAction {
  return {
    type: "notification",
    title: "",
    body: "",
  };
}

/**
 * Create a default action for a given type.
 */
export function createDefaultAction(type: HookActionType): HookAction {
  switch (type) {
    case "command":
      return createDefaultCommandAction();
    case "webhook":
      return createDefaultWebhookAction();
    case "log":
      return createDefaultLogAction();
    case "notification":
      return createDefaultNotificationAction();
  }
}

/**
 * Default hook for creation form.
 */
export function createDefaultHook(): Omit<Hook, "id" | "created_at" | "updated_at"> {
  return {
    name: "",
    trigger: "pre_execution",
    action_type: "log",
    action_config: createDefaultLogAction(),
    enabled: true,
    execution_order: 0,
    continue_on_failure: true,
    conditions: [],
  };
}
