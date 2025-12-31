/**
 * TaskRun Types
 *
 * Type definitions for the async AI task system.
 * Tasks are triggered via /prompts/run and polled for completion.
 */

/**
 * Status of a task run
 */
export type TaskRunStatus = "running" | "complete" | "failed" | "stopped";

/**
 * Task run data structure from the backend
 */
export interface TaskRun {
  /** Unique task run ID */
  id: string;
  /** Human-readable task name */
  task_name: string;
  /** The original prompt/instructions */
  prompt: string;
  /** Current status */
  status: TaskRunStatus;
  /** Number of Claude sessions spawned */
  sessions_count: number;
  /** Maximum sessions before giving up (null = unlimited) */
  max_sessions?: number | null;
  /** Accumulated output with session markers */
  output_log: string;
  /** Error message if failed */
  error_message?: string | null;
  /** ISO 8601 creation timestamp */
  created_at: string;
  /** ISO 8601 last update timestamp */
  updated_at: string;
  /** ISO 8601 completion timestamp */
  completed_at?: string | null;
}

/**
 * Response from /prompts/run endpoint (async mode)
 */
export interface RunPromptResponse {
  success: boolean;
  /** Task run ID for polling */
  task_run_id?: string;
  /** Session ID */
  session_id?: string;
  /** Path to state file */
  state_file?: string;
  /** Path to log file */
  log_file?: string;
  /** Process ID if available */
  pid?: number;
  /** Error message if failed */
  error?: string;
  /** Legacy: Direct output (for backward compatibility during transition) */
  output?: string;
  /** Legacy: Data wrapper */
  data?: {
    output?: string;
    response?: string;
  };
}

/**
 * Request body for /prompts/run endpoint
 */
export interface RunPromptRequest {
  /** Prompt name (e.g., "ai-analysis", "custom-prompt") */
  name: string;
  /** Prompt content (the actual prompt text) */
  content: string;
  /** Maximum number of sessions to spawn (default: 1) */
  max_sessions?: number;
  /** Prompt to display in UI (shorter than full prompt) */
  display_prompt?: string;
  /** Timeout in seconds */
  timeout_seconds?: number;
  /** Context identifier */
  context?: string;
  /** Image paths to include */
  image_paths?: string[];
  /** Video paths to include */
  video_paths?: string[];
  /** Trace path to include */
  trace_path?: string;
  /** Max video frames to extract */
  max_video_frames?: number;
  /** Max trace screenshots to include */
  max_trace_screenshots?: number;
}

/**
 * Check if a task run is still in progress
 */
export function isTaskRunning(task: TaskRun): boolean {
  return task.status === "running";
}

/**
 * Check if a task run completed successfully
 */
export function isTaskComplete(task: TaskRun): boolean {
  return task.status === "complete";
}

/**
 * Check if a task run failed
 */
export function isTaskFailed(task: TaskRun): boolean {
  return task.status === "failed" || task.status === "stopped";
}

/**
 * Check if a task run is finished (complete, failed, or stopped)
 */
export function isTaskFinished(task: TaskRun): boolean {
  return task.status !== "running";
}
