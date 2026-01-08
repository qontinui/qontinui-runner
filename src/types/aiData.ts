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
export type JsonlLogType = "general" | "actions" | "image-recognition" | "playwright" | "ai-output";

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

// =============================================================================
// Text Log Types (plain text, filtered by task run time range)
// =============================================================================

/**
 * Valid text log types for dev logs.
 * Only logs with parseable timestamps are supported.
 */
export type TextLogType = "backend" | "backend-err" | "qontinui-api" | "qontinui-api-err";

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
// Re-export RunDetails from statistics (automation runs)
// =============================================================================

// RunDetails is already defined in statistics.ts, we re-export here for convenience
export type { RunDetails, RunStatus } from "./statistics";
