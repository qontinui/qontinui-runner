// Trigger system types

// ============================================================================
// Trigger Config (matches Rust TriggerConfig enum)
// ============================================================================

export interface WebhookConfig {
  type: "webhook";
  secret?: string;
  payload_filter?: string;
  variable_mapping: Record<string, string>;
}

export interface FileWatchConfig {
  type: "file_watch";
  paths: string[];
  patterns: string[];
  ignore_patterns: string[];
  recursive: boolean;
}

export interface WorkflowChainConfig {
  type: "workflow_chain";
  source_workflow_id: string;
  on_status: string; // "completed" | "failed" | "any"
  pass_context: boolean;
}

export interface GitEventConfig {
  type: "git_event";
  repo_path: string;
  events: string[]; // "commit" | "branch_switch" | "tag"
  branch_filter?: string;
}

export interface HealthCheckTarget {
  url: string;
  expected_status: number;
  timeout_seconds: number;
}

export interface HealthCheckConfig {
  type: "health_check";
  urls: HealthCheckTarget[];
  check_interval_seconds: number;
  consecutive_failures: number;
}

export type TriggerConfig =
  | WebhookConfig
  | FileWatchConfig
  | WorkflowChainConfig
  | GitEventConfig
  | HealthCheckConfig;

// ============================================================================
// Trigger Definition
// ============================================================================

export interface TriggerCondition {
  type: string;
  config: unknown;
}

export interface WorkflowTrigger {
  id: string;
  name: string;
  description?: string;
  trigger_type: string;
  trigger_config: TriggerConfig;
  workflow_id: string;
  workflow_overrides?: Record<string, unknown>;
  conditions: TriggerCondition[];
  debounce_ms: number;
  cooldown_seconds: number;
  max_concurrent: number;
  enabled: boolean;
  last_triggered_at?: string;
  last_execution_id?: string;
  trigger_count: number;
  created_at: string;
  updated_at: string;
}

// ============================================================================
// Trigger History
// ============================================================================

export interface TriggerHistoryEntry {
  id: string;
  trigger_id: string;
  event_type: string;
  event_data: Record<string, unknown>;
  action: string; // "executed" | "debounced" | "throttled" | "condition_failed" | "error"
  task_run_id?: string;
  error_message?: string;
  triggered_at: string;
}

// ============================================================================
// API Request/Response
// ============================================================================

export interface CreateTriggerRequest {
  name: string;
  description?: string;
  trigger_config: TriggerConfig;
  workflow_id: string;
  workflow_overrides?: Record<string, unknown>;
  conditions?: TriggerCondition[];
  debounce_ms?: number;
  cooldown_seconds?: number;
  max_concurrent?: number;
}

export interface UpdateTriggerRequest {
  name?: string;
  description?: string | null;
  trigger_config?: TriggerConfig;
  workflow_id?: string;
  workflow_overrides?: Record<string, unknown> | null;
  conditions?: TriggerCondition[];
  debounce_ms?: number;
  cooldown_seconds?: number;
  max_concurrent?: number;
}

export interface TriggerSystemStatus {
  running: boolean;
  total_triggers: number;
  enabled_triggers: number;
  active_watchers: number;
  active_executions: number;
}

// ============================================================================
// Helpers
// ============================================================================

export function getTriggerTypeLabel(type: string): string {
  switch (type) {
    case "webhook":
      return "Webhook";
    case "file_watch":
      return "File Watch";
    case "workflow_chain":
      return "Workflow Chain";
    case "git_event":
      return "Git Event";
    case "health_check":
      return "Health Check";
    default:
      return type;
  }
}

export function getActionColor(action: string): string {
  switch (action) {
    case "executed":
      return "text-green-400";
    case "debounced":
      return "text-yellow-400";
    case "throttled":
      return "text-orange-400";
    case "condition_failed":
      return "text-gray-400";
    case "error":
      return "text-red-400";
    default:
      return "text-gray-400";
  }
}
