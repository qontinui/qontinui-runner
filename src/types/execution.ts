/**
 * Unified Execution Schema Types
 *
 * Type definitions for the unified execution reporting API that supports
 * multiple run types: QA testing, integration testing, live automation,
 * recording sessions, and debug runs.
 */

// ============================================================================
// Enums
// ============================================================================

/** Type of execution run */
export enum RunType {
  QA_TEST = "qa_test",
  INTEGRATION_TEST = "integration_test",
  LIVE_AUTOMATION = "live_automation",
  RECORDING = "recording",
  DEBUG = "debug",
}

/** Status of an execution run */
export enum RunStatus {
  PENDING = "pending",
  RUNNING = "running",
  COMPLETED = "completed",
  FAILED = "failed",
  TIMEOUT = "timeout",
  CANCELLED = "cancelled",
  PAUSED = "paused",
}

/** Status of an individual action */
export enum ActionStatus {
  SUCCESS = "success",
  FAILED = "failed",
  TIMEOUT = "timeout",
  SKIPPED = "skipped",
  ERROR = "error",
  PENDING = "pending",
}

/** Type of action executed */
export enum ActionType {
  // Vision actions
  FIND = "find",
  FIND_ALL = "find_all",
  WAIT_FOR = "wait_for",
  WAIT_UNTIL_GONE = "wait_until_gone",

  // Input actions
  CLICK = "click",
  DOUBLE_CLICK = "double_click",
  RIGHT_CLICK = "right_click",
  TYPE = "type",
  PRESS_KEY = "press_key",
  HOTKEY = "hotkey",
  SCROLL = "scroll",
  DRAG = "drag",

  // State machine actions
  GO_TO_STATE = "go_to_state",
  TRANSITION = "transition",
  VERIFY_STATE = "verify_state",

  // Control flow
  CONDITIONAL = "conditional",
  LOOP = "loop",
  PARALLEL = "parallel",
  SEQUENCE = "sequence",

  // Utility
  WAIT = "wait",
  SCREENSHOT = "screenshot",
  LOG = "log",
  ASSERT = "assert",

  // Custom/plugin
  CUSTOM = "custom",
}

/** Type of error that occurred */
export enum ErrorType {
  ELEMENT_NOT_FOUND = "element_not_found",
  TIMEOUT = "timeout",
  ASSERTION_FAILED = "assertion_failed",
  CRASH = "crash",
  NETWORK_ERROR = "network_error",
  VALIDATION_ERROR = "validation_error",
  OTHER = "other",
}

/** Severity of an issue */
export enum IssueSeverity {
  CRITICAL = "critical",
  HIGH = "high",
  MEDIUM = "medium",
  LOW = "low",
  INFORMATIONAL = "informational",
}

/** Type of screenshot */
export enum ScreenshotType {
  ERROR = "error",
  SUCCESS = "success",
  MANUAL = "manual",
  PERIODIC = "periodic",
  ACTION_RESULT = "action_result",
  STATE_VERIFICATION = "state_verification",
}

// ============================================================================
// Metadata Types
// ============================================================================

/** Metadata about the runner environment */
export interface RunnerMetadata {
  runner_version: string;
  os: string;
  hostname: string;
  screen_resolution?: string;
  cpu_info?: string;
  memory_mb?: number;
  extra?: Record<string, unknown>;
}

/** Metadata about the workflow being executed */
export interface WorkflowMetadata {
  workflow_id: string;
  workflow_name: string;
  workflow_version?: string;
  total_states?: number;
  total_transitions?: number;
  tags?: string[];
  description?: string;
  /** Initial active states when workflow starts (resolved from config) */
  initial_state_ids?: string[];
}

/** Execution statistics */
export interface ExecutionStats {
  total_actions: number;
  successful_actions: number;
  failed_actions: number;
  timeout_actions: number;
  skipped_actions: number;
  total_duration_ms: number;
  avg_action_duration_ms?: number;
}

/** Coverage data for test runs */
export interface CoverageData {
  coverage_percentage: number;
  states_covered: number;
  total_states: number;
  transitions_covered: number;
  total_transitions: number;
  uncovered_states?: string[];
  uncovered_transitions?: string[];
  state_visit_counts?: Record<string, number>;
  transition_execution_counts?: Record<string, number>;
}

// ============================================================================
// Request/Create Types (sent to backend)
// ============================================================================

/** Input for creating an execution run */
export interface ExecutionRunCreate {
  project_id: string;
  run_type: RunType;
  run_name: string;
  description?: string;
  runner_metadata: RunnerMetadata;
  workflow_metadata?: WorkflowMetadata;
  configuration?: Record<string, unknown>;
}

/** Response from creating an execution run */
export interface ExecutionRunResponse {
  run_id: string;
  project_id: string;
  run_type: RunType;
  run_name: string;
  status: RunStatus;
  started_at: string;
  ended_at?: string;
  duration_seconds?: number;
}

/** Input for creating an action execution record */
export interface ActionExecutionCreate {
  sequence_number: number;
  action_type: ActionType;
  action_name: string;
  status: ActionStatus;
  started_at: string;
  completed_at: string;
  duration_ms: number;

  // State context
  from_state?: string;
  to_state?: string;
  active_states?: string[];

  // Vision/pattern matching details
  pattern_id?: string;
  pattern_name?: string;
  confidence_score?: number;
  match_location?: {
    x: number;
    y: number;
    width?: number;
    height?: number;
  };

  // Error information
  error_message?: string;
  error_type?: ErrorType;
  error_stack?: string;

  // References
  screenshot_id?: string;
  parent_action_id?: string;

  // Additional data
  input_data?: Record<string, unknown>;
  output_data?: Record<string, unknown>;
  metadata?: Record<string, unknown>;
}

/** Response from reporting action executions */
export interface ActionExecutionResponse {
  recorded: number;
  run_id: string;
  action_ids?: string[];
}

/** Input for creating an execution screenshot */
export interface ExecutionScreenshotCreate {
  screenshot_id: string;
  sequence_number: number;
  screenshot_type: ScreenshotType;
  timestamp: string;
  width: number;
  height: number;

  // Context
  action_sequence_number?: number;
  state?: string;
  active_states?: string[];

  // Annotations
  annotations?: Array<{
    type: "box" | "circle" | "arrow" | "text";
    x: number;
    y: number;
    width?: number;
    height?: number;
    label?: string;
    color?: string;
  }>;

  metadata?: Record<string, unknown>;
}

/** Response from uploading a screenshot */
export interface ExecutionScreenshotResponse {
  screenshot_id: string;
  run_id: string;
  image_url: string;
  thumbnail_url?: string;
  uploaded_at: string;
  file_size_bytes: number;
}

/** Input for creating an execution issue */
export interface ExecutionIssueCreate {
  title: string;
  description: string;
  severity: IssueSeverity;
  issue_type: string;

  // Context
  action_sequence_number?: number;
  state?: string;
  screenshot_ids?: string[];

  // Reproduction
  reproduction_steps?: string[];
  expected_behavior?: string;
  actual_behavior?: string;

  metadata?: Record<string, unknown>;
}

/** Response from reporting issues */
export interface ExecutionIssueResponse {
  recorded: number;
  run_id: string;
  issue_ids?: string[];
}

/** Input for completing an execution run */
export interface ExecutionRunComplete {
  status: RunStatus;
  ended_at: string;
  stats: ExecutionStats;
  coverage?: CoverageData;
  summary?: string;
  error_message?: string;
}

/** Response from completing an execution run */
export interface ExecutionRunCompleteResponse {
  run_id: string;
  status: RunStatus;
  started_at: string;
  ended_at: string;
  duration_seconds: number;
  stats: ExecutionStats;
  coverage?: CoverageData;
}
