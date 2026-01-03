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
 * Task run from the task_runs table.
 * Matches TaskRun struct from database/mod.rs
 */
export interface TaskRun {
  id: string;
  task_name: string;
  prompt: string;
  status: TaskRunStatus;
  sessions_count: number;
  max_sessions?: number;
  output_log: string;
  error_message?: string;
  auto_continue: boolean;
  execution_steps_json?: string;
  log_sources_json?: string;
  created_at: string;
  updated_at: string;
  completed_at?: string;
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
// Re-export RunDetails from statistics (automation runs)
// =============================================================================

// RunDetails is already defined in statistics.ts, we re-export here for convenience
export type { RunDetails, RunStatus } from "./statistics";
