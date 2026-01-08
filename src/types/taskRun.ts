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
 * Type of task (for unified architecture)
 */
export type TaskType = "task" | "automation" | "scheduled";

/**
 * Task run data structure from the backend
 */
export interface TaskRun {
  /** Unique task run ID */
  id: string;
  /** Human-readable task name */
  task_name: string;
  /** The original prompt/instructions (optional for pure automation tasks) */
  prompt?: string | null;
  /** Task type: 'task', 'automation', or 'scheduled' */
  task_type: TaskType;
  /** Config ID for automation-enabled tasks */
  config_id?: string | null;
  /** Workflow name being executed */
  workflow_name?: string | null;
  /** Current status */
  status: TaskRunStatus;
  /** Number of Claude sessions spawned */
  sessions_count: number;
  /** Maximum sessions before giving up (null = unlimited) */
  max_sessions?: number | null;
  /** Whether to auto-continue on session completion */
  auto_continue: boolean;
  /** Accumulated output with session markers */
  output_log: string;
  /** Error message if failed */
  error_message?: string | null;
  /** Post-completion summary (AI generated) */
  summary?: string | null;
  /** Whether the task goal was achieved */
  goal_achieved?: boolean | null;
  /** Remaining work description */
  remaining_work?: string | null;
  /** When the summary was generated */
  summary_generated_at?: string | null;
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
 * Request body for creating a task run directly (via /task-runs endpoint)
 */
export interface CreateTaskRunRequest {
  /** Human-readable task name */
  task_name: string;
  /** The prompt/instructions (optional for pure automation tasks) */
  prompt?: string;
  /** Task type: 'task', 'automation', or 'scheduled' (defaults to 'task') */
  task_type?: TaskType;
  /** Config ID for automation-enabled tasks */
  config_id?: string;
  /** Workflow name being executed */
  workflow_name?: string;
  /** Maximum number of sessions before giving up */
  max_sessions?: number;
  /** Whether to auto-continue on session completion */
  auto_continue?: boolean;
  /** JSON-encoded execution steps */
  execution_steps_json?: string;
  /** JSON-encoded log sources */
  log_sources_json?: string;
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
