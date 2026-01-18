/**
 * Type definitions for the AI Data Viewer
 *
 * These types mirror the Rust types in:
 * - src-tauri/src/commands/ai_data.rs
 * - src-tauri/src/database/mod.rs (TaskRun)
 * - src-tauri/src/tiered_info/types.rs (RunDetails)
 */

// =============================================================================
// Response Wrapper
// =============================================================================

/**
 * Response wrapper for AI data commands.
 * Matches AiDataResponse<T> from ai_data.rs
 */
export interface AiDataResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

// =============================================================================
// Task Run Types (from database/mod.rs)
// =============================================================================

/**
 * Task run status.
 */
export type TaskRunStatus = "running" | "complete" | "failed" | "stopped";

/**
 * Task type - defines the nature of the task.
 * - task: Standard AI task (default)
 * - automation: Pure GUI automation task
 * - scheduled: Scheduler-triggered task
 */
export type TaskType = "task" | "automation" | "scheduled";

/**
 * Task run from the task_runs table.
 * Matches TaskRun struct from database/mod.rs
 *
 * TaskRun is THE unified run concept for all execution:
 * - AI tasks (with prompt)
 * - Automation tasks (with config_id)
 * - Mixed tasks (AI with automation steps)
 */
export interface TaskRun {
  id: string;
  task_name: string;
  /** Task prompt - null for pure automation tasks */
  prompt?: string | null;
  status: TaskRunStatus;
  /** Type of task: 'task', 'automation', or 'scheduled' */
  task_type: TaskType;
  sessions_count: number;
  max_sessions?: number;
  output_log: string;
  error_message?: string;
  auto_continue: boolean;
  execution_steps_json?: string;
  log_sources_json?: string;
  /** Config ID for automation-enabled tasks */
  config_id?: string | null;
  /** Workflow name being executed */
  workflow_name?: string | null;
  /** AI-generated paragraph summary of the task run (unified field) */
  summary?: string | null;
  /** @deprecated Use summary instead - kept for backward compatibility */
  ai_summary?: string | null;
  /** Whether the stated goal was achieved (determined by AI after completion) */
  goal_achieved?: boolean | null;
  /** What remains to be done if goal was not achieved */
  remaining_work?: string | null;
  /** Timestamp when the summary was generated */
  summary_generated_at?: string | null;
  created_at: string;
  updated_at: string;
  completed_at?: string | null;
}

// =============================================================================
// JSONL Log Types
// =============================================================================

/**
 * Valid JSONL log types.
 */
export type JsonlLogType =
  | "general"
  | "actions"
  | "image-recognition"
  | "playwright"
  | "ai-output"
  | "api-requests";

/**
 * Result of reading JSONL logs.
 * Matches JsonlLogsResult from ai_data.rs
 */
export interface JsonlLogsResult {
  log_type: string;
  entries: unknown[];
  count: number;
  file_path: string;
  file_exists: boolean;
  task_run_id?: string | null;
  start_time?: string | null;
  end_time?: string | null;
}

// =============================================================================
// Consolidated AI Output Types
// =============================================================================

/**
 * A chunk of consolidated AI output from a single source.
 * Matches AiOutputChunk from ai_data.rs
 */
export interface AiOutputChunk {
  /** Source of the output (e.g., "claude", "prompt") */
  source: string;
  /** Start time of this chunk (formatted as HH:MM:SS) */
  start_time: string;
  /** End time of this chunk (formatted as HH:MM:SS), null if single entry */
  end_time?: string | null;
  /** Combined content from all entries in this chunk */
  content: string;
  /** Number of raw entries that were consolidated */
  entry_count: number;
}

/**
 * Result of reading consolidated AI output.
 * Matches ConsolidatedAiOutputResult from ai_data.rs
 */
export interface ConsolidatedAiOutputResult {
  chunks: AiOutputChunk[];
  total_entries: number;
  task_run_id: string;
  start_time: string;
  end_time?: string | null;
}

/**
 * Info about a single JSONL log file.
 */
export interface JsonlLogFileInfo {
  file_path: string;
  file_exists: boolean;
  entry_count: number;
}

/**
 * Summary of all JSONL log files.
 * Matches JsonlLogsSummary from ai_data.rs
 */
export interface JsonlLogsSummary {
  general: JsonlLogFileInfo;
  actions: JsonlLogFileInfo;
  image_recognition: JsonlLogFileInfo;
  playwright: JsonlLogFileInfo;
  ai_output: JsonlLogFileInfo;
  api_requests: JsonlLogFileInfo;
}

// =============================================================================
// General Log Entry Types (from file_logger.rs)
// =============================================================================

/**
 * General log entry.
 */
export interface GeneralLogEntry {
  id: string;
  timestamp: string;
  level: "info" | "warning" | "error" | "debug";
  message: string;
}

/**
 * Action log entry.
 */
export interface ActionLogEntry {
  id: string;
  timestamp: number;
  sequence: number;
  event_type: string;
  node?: unknown;
  path?: unknown[];
}

/**
 * Image recognition log entry.
 */
export interface ImageRecognitionLogEntry {
  id: string;
  timestamp: string;
  node: string;
  template: string;
  confidence: number;
  found: boolean;
  threshold: number;
  location?: {
    x: number;
    y: number;
    width: number;
    height: number;
  };
  annotated_screenshot_path?: string;
  matched_region_path?: string;
  debug?: unknown;
}

/**
 * Playwright log entry.
 */
export interface PlaywrightLogEntry {
  id: string;
  timestamp: string;
  script_id: string;
  script_name: string;
  passed: boolean;
  tests_passed: number;
  tests_failed: number;
  duration_ms: number;
  error?: string;
  specs?: unknown[];
  console_output?: string[];
  screenshot_paths?: string[];
  page_snapshot?: string;
}

/**
 * Variable extraction result from API response.
 */
export interface ApiExtractionResult {
  variable_name: string;
  json_path: string;
  extracted_value?: string;
  success: boolean;
  error?: string;
}

/**
 * Assertion result from API response verification.
 */
export interface ApiAssertionResult {
  type: string;
  expected: string;
  actual: string;
  passed: boolean;
  error?: string;
}

/**
 * API request log entry.
 */
export interface ApiRequestLogEntry {
  id: string;
  timestamp: string;
  step_id: string;
  step_name: string;

  // Request details
  method: string;
  url: string;
  resolved_url: string;
  headers: Record<string, string>;
  body?: string;
  content_type?: string;

  // Response details
  status_code: number;
  status_text: string;
  response_headers: Record<string, string>;
  response_time_ms: number;

  // Response body handling
  response_body_type: "json" | "text" | "binary";
  response_body?: string;
  response_file_path?: string;
  response_size_bytes: number;

  // Variable extractions performed
  extractions?: ApiExtractionResult[];

  // Assertion results
  assertions?: ApiAssertionResult[];

  // Overall result
  success: boolean;
  error?: string;
}

// =============================================================================
// Text Log Types (plain text, filtered by task run time range)
// =============================================================================

/**
 * Valid text log types for dev logs.
 * Only logs with parseable timestamps are supported.
 */
export type TextLogType = "backend" | "backend-err";

/**
 * Result of reading text logs for a task run.
 * Matches TextLogsResult from ai_data.rs
 */
export interface TextLogsResult {
  log_type: string;
  content: string;
  line_count: number;
  file_path: string;
  file_exists: boolean;
  task_run_id?: string | null;
  start_time?: string | null;
  end_time?: string | null;
}

/**
 * Info about a single text log file for a task run.
 */
export interface TextLogFileInfo {
  log_type: string;
  file_path: string;
  file_exists: boolean;
  line_count: number;
}

/**
 * Summary of all text log files for a task run.
 * Matches TextLogsSummary from ai_data.rs
 */
export interface TextLogsSummary {
  task_run_id: string;
  start_time: string;
  end_time?: string | null;
  logs: TextLogFileInfo[];
}

// =============================================================================
// Task Run Automation (child records for automation metrics)
// =============================================================================

/**
 * Automation status for task_run_automation records.
 */
export type AutomationStatus = "running" | "success" | "failed" | "timeout" | "cancelled";

/**
 * Task run automation record - child of TaskRun for GUI automation metrics.
 * Matches task_run_automation table in database.
 *
 * Some TaskRuns have ONLY automation (no AI sessions).
 * Some TaskRuns have ONLY AI sessions (no automation).
 * Some TaskRuns have BOTH (mixed execution).
 */
export interface TaskRunAutomation {
  id: string;
  task_run_id: string;
  workflow_name?: string | null;
  started_at: string;
  ended_at?: string | null;
  duration_ms?: number | null;
  automation_status: AutomationStatus;
  success?: boolean | null;
  error_type?: string | null;
  error_message?: string | null;
  /** JSON: {"total": N, "success": N, "failed": N, "skipped": N} */
  actions_summary?: string | null;
  /** JSON array of state names visited */
  states_visited?: string | null;
  /** JSON array of transition records */
  transitions_executed?: string | null;
  /** JSON array of template match records */
  template_matches?: string | null;
  /** JSON array of anomalies */
  anomalies?: string | null;
  /** Iteration number within the task run */
  iteration_number: number;
}

// =============================================================================
// Screenshots Types
// =============================================================================

/**
 * Screenshot file info.
 * Matches ScreenshotInfo from ai_data.rs
 */
export interface ScreenshotInfo {
  filename: string;
  path: string;
  size_bytes: number;
  modified?: string | null;
}

/**
 * Screenshots result containing annotated and playwright screenshots.
 * Matches ScreenshotsResult from ai_data.rs
 */
export interface ScreenshotsResult {
  annotated: ScreenshotInfo[];
  playwright: ScreenshotInfo[];
}

// =============================================================================
// Loaded Config Types
// =============================================================================

/**
 * Loaded config info.
 * Matches LoadedConfigInfo from ai_data.rs
 */
export interface LoadedConfigInfo {
  config_content?: string | null;
  config_path?: string | null;
  config_format?: string | null;
  meta?: {
    source_path?: string;
    loaded_at?: string;
    [key: string]: unknown;
  } | null;
}

// =============================================================================
// AI Prompts Types
// =============================================================================

/**
 * AI prompt info for a specific prompt file.
 * Matches AiPromptInfo from ai_data.rs
 */
export interface AiPromptInfo {
  prompt_file: string;
  content: string;
  iteration?: number | null;
}

/**
 * AI prompts result for a task run.
 * Matches AiPromptsResult from ai_data.rs
 */
export interface AiPromptsResult {
  task_run_id: string;
  prompts: AiPromptInfo[];
}

// =============================================================================
// Contexts Types
// =============================================================================

/**
 * Context info for display in the AI Data Viewer.
 * Matches ContextInfo from ai_data.rs
 */
export interface ContextInfo {
  id: string;
  name: string;
  context_type: "builtin" | "project" | "user";
  category?: string | null;
  tags: string[];
  content: string;
  enabled: boolean;
  auto_include?: {
    task_mentions?: string[] | null;
    action_types?: string[] | null;
    error_patterns?: string[] | null;
    file_patterns?: string[] | null;
  } | null;
}

/**
 * Contexts result containing all available contexts.
 * Matches ContextsResult from ai_data.rs
 */
export interface ContextsResult {
  contexts: ContextInfo[];
}

// =============================================================================
// SQLite Migrated Log Types
// =============================================================================

/**
 * Task run event from SQLite database.
 * Replaces JSONL files for historical queries.
 */
export interface TaskRunEvent {
  id: number;
  task_run_id: string;
  /** Event type: 'general', 'action', 'image_recognition', 'state_change', 'ai_output' */
  event_type: string;
  /** Event subtype: 'start', 'complete', 'error', 'match', 'transition', etc. */
  event_subtype?: string | null;
  /** Human-readable message */
  message: string;
  /** JSON payload with event-specific data */
  data?: string | null;
  /** Context */
  workflow_name?: string | null;
  state_name?: string | null;
  action_id?: string | null;
  /** Timing */
  timestamp: string;
  duration_ms?: number | null;
}

/**
 * Result of querying task run events from SQLite.
 */
export interface TaskRunEventsResult {
  task_run_id: string;
  events: TaskRunEvent[];
  count: number;
  event_type_filter?: string | null;
}

/**
 * Task run screenshot from SQLite database.
 */
export interface TaskRunScreenshotDb {
  id: string;
  task_run_id: string;
  event_id?: number | null;
  /** Path to PNG file */
  file_path: string;
  /** Screenshot type: 'annotated', 'raw', 'diff', 'failure' */
  screenshot_type: string;
  /** Context from image recognition */
  template_name?: string | null;
  confidence?: number | null;
  /** JSON {x, y, width, height} */
  match_location?: string | null;
  /** Metadata */
  width?: number | null;
  height?: number | null;
  file_size_bytes?: number | null;
  created_at: string;
}

/**
 * Result of querying task run screenshots from SQLite.
 */
export interface TaskRunScreenshotsDbResult {
  task_run_id: string;
  screenshots: TaskRunScreenshotDb[];
  count: number;
}

/**
 * Playwright test result from SQLite database.
 */
export interface TaskRunPlaywrightResultDb {
  id: string;
  task_run_id: string;
  /** Test identification */
  test_name: string;
  spec_file?: string | null;
  /** Status: 'passed', 'failed', 'skipped', 'timeout' */
  status: string;
  duration_ms?: number | null;
  /** Output */
  stdout?: string | null;
  stderr?: string | null;
  console_output?: string | null;
  page_snapshot?: string | null;
  /** Failure details */
  error_message?: string | null;
  failure_screenshot_path?: string | null;
  /** Assertion summary */
  assertions_passed: number;
  assertions_failed: number;
  created_at: string;
}

/**
 * Result of querying Playwright results from SQLite.
 */
export interface TaskRunPlaywrightResultsDbResult {
  task_run_id: string;
  results: TaskRunPlaywrightResultDb[];
  count: number;
  passed: number;
  failed: number;
}

/**
 * Summary of all migrated log data for a task run.
 */
export interface TaskRunMigratedLogsSummary {
  task_run_id: string;
  events_count: number;
  screenshots_count: number;
  playwright_results_count: number;
  events_by_type: Record<string, number>;
}

/**
 * API request record from SQLite database.
 * Stores API request execution results migrated from JSONL logs.
 */
export interface TaskRunApiRequestDb {
  id: string;
  task_run_id: string;
  /** Step identification */
  step_id: string;
  step_name?: string | null;
  /** Request details */
  method: string;
  url: string;
  resolved_url: string;
  /** JSON object {header: value} */
  request_headers?: string | null;
  request_body?: string | null;
  /** Response details */
  status_code: number;
  status_text?: string | null;
  /** JSON object {header: value} */
  response_headers?: string | null;
  response_time_ms: number;
  /** Response body handling */
  response_body_type: string;
  response_body?: string | null;
  response_size_bytes?: number | null;
  /** JSON array of extraction results */
  extractions?: string | null;
  /** JSON array of assertion results */
  assertions?: string | null;
  /** Overall result */
  success: boolean;
  error_message?: string | null;
  created_at: string;
}

/**
 * Result of querying API requests from SQLite.
 */
export interface TaskRunApiRequestsDbResult {
  task_run_id: string;
  requests: TaskRunApiRequestDb[];
  count: number;
  success_count: number;
  failed_count: number;
}

// =============================================================================
// AWAS Step Types
// =============================================================================

/**
 * AWAS (Automated Web Agent System) step record from SQLite database.
 * Stores AWAS step execution results.
 */
export interface TaskRunAwasStepDb {
  id: string;
  task_run_id: string;
  /** Step identification */
  step_id?: string | null;
  step_name?: string | null;
  /** Step type: 'awas_discover', 'awas_execute', 'awas_check_support', 'awas_list_actions', 'awas_extract_elements' */
  step_type: string;
  /** Context URL */
  url?: string | null;
  /** Execution parameters */
  action_id?: string | null;
  /** JSON: step-specific parameters */
  parameters?: string | null;
  /** Response data (JSON: contains manifest, actions, elements, etc. depending on step_type) */
  response_data?: string | null;
  /** Results */
  success: boolean;
  error_message?: string | null;
  duration_ms?: number | null;
  created_at: string;
}

/**
 * Result of querying AWAS steps from SQLite.
 */
export interface TaskRunAwasStepsDbResult {
  task_run_id: string;
  steps: TaskRunAwasStepDb[];
  count: number;
  success_count: number;
  failed_count: number;
}

// =============================================================================
// MCP Call Types
// =============================================================================

/**
 * MCP call record from SQLite database.
 * Stores MCP tool call execution results.
 */
export interface TaskRunMcpCallDb {
  id: string;
  task_run_id: string;

  /** Step identification */
  step_id: string;
  step_name?: string | null;

  /** Server identification */
  server_id: string;
  server_name?: string | null;

  /** Tool call details */
  tool_name: string;
  /** JSON: tool arguments */
  arguments?: string | null;
  /** JSON: resolved arguments after variable substitution */
  resolved_arguments?: string | null;

  /** Response details */
  /** JSON: tool response content */
  response?: string | null;
  response_type: string; // 'text', 'json', 'error'
  duration_ms: number;

  /** Variable extractions (JSON array of extraction results) */
  extractions?: string | null;
  /** Assertion results (JSON array of assertion results) */
  assertions?: string | null;

  /** Overall result */
  success: boolean;
  error_message?: string | null;

  created_at: string;
}

/**
 * Result of querying MCP calls from SQLite.
 */
export interface TaskRunMcpCallsDbResult {
  task_run_id: string;
  calls: TaskRunMcpCallDb[];
  count: number;
  success_count: number;
  failed_count: number;
}

// =============================================================================
// Re-export RunDetails from statistics (automation runs)
// =============================================================================

// RunDetails is already defined in statistics.ts, we re-export here for convenience
export type { RunDetails, RunStatus } from "./statistics";
