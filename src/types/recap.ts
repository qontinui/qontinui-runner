/**
 * Type definitions for the Session Recap page.
 *
 * These types match the Rust structs in commands/recap.rs
 */

/**
 * A step in the recap timeline.
 */
export interface RecapStep {
  /** Step name */
  name: string;
  /** Step type: "workflow", "action", "ai_session", "test", "check" */
  step_type: string;
  /** Status: "success", "failed", "running", "skipped" */
  status: string;
  /** Brief summary of what happened */
  summary?: string;
  /** Duration in milliseconds */
  duration_ms?: number;
  /** Error message if failed */
  error?: string;
  /** Nested steps (for workflows containing actions) */
  children: RecapStep[];
}

/**
 * Information about why a run failed.
 */
export interface FailureInfo {
  /** Primary reason for failure */
  reason: string;
  /** Name of the step that failed */
  failed_step?: string;
  /** Detailed error message or stack trace */
  error_details?: string;
  /** Error type category */
  error_type?: string;
}

/**
 * Quick statistics about the run.
 */
export interface RecapStats {
  /** Total number of actions executed */
  total_actions: number;
  /** Number of successful actions */
  successful_actions: number;
  /** Number of failed actions */
  failed_actions: number;
  /** Number of skipped actions */
  skipped_actions: number;
  /** Total number of AI sessions */
  ai_sessions: number;
  /** Number of tests run */
  tests_run: number;
  /** Number of tests passed */
  tests_passed: number;
}

/**
 * A stage in the recap timeline, grouping related steps.
 * Stages represent the 4 workflow phases: setup, agentic, verification, completion.
 */
export interface StageRecap {
  /** Stage identifier: "setup", "agentic", "verification", "completion" */
  stage: "setup" | "agentic" | "verification" | "completion";
  /** Display name: "Setup", "Agentic", "Verification", "Completion" */
  display_name: string;
  /** Status: "success", "failed", "running", "skipped", "pending" */
  status: "success" | "failed" | "running" | "skipped" | "pending";
  /** When this stage started (ISO 8601) */
  started_at?: string;
  /** When this stage ended (ISO 8601) */
  ended_at?: string;
  /** Duration in milliseconds */
  duration_ms?: number;
  /** Steps in this stage */
  steps: RecapStep[];
  /** Iteration number (for agentic/verification in loop) */
  iteration?: number;
}

/**
 * Complete recap data for a task run.
 */
export interface RecapData {
  /** Task run ID */
  task_run_id: string;
  /** Task name */
  task_name: string;
  /** Overall status: "running", "complete", "failed", "stopped" */
  status: string;
  /** Total duration in milliseconds */
  duration_ms?: number;
  /** When the run started (ISO 8601) */
  created_at: string;
  /** When the run completed (ISO 8601) */
  completed_at?: string;

  /** Failure info (prominent if failed) */
  failure_info?: FailureInfo;

  /** AI-generated or extracted summary */
  summary?: string;

  /** Whether the goal was achieved (from orchestrator) */
  goal_achieved?: boolean;

  /** Steps grouped by stage (with timing from transition_history) */
  stages: StageRecap[];

  /** Steps overview (timeline) - flat list for backwards compatibility */
  steps: RecapStep[];

  /** Quick statistics */
  stats: RecapStats;
}

/**
 * Response wrapper from the recap command.
 */
export interface RecapResponse {
  success: boolean;
  data?: RecapData;
  error?: string;
}
