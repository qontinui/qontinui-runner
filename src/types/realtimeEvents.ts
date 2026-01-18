/**
 * Realtime Event Types
 *
 * Types for real-time UI updates via Tauri events.
 * These events enable live dashboard updates without polling.
 */

// ============================================================================
// Event Names (must match Rust event constants)
// ============================================================================

/** Event channel for learning updates */
export const EVENT_LEARNING_UPDATE = "learning-update";

/** Event channel for checkpoint creation */
export const EVENT_CHECKPOINT_CREATED = "checkpoint-created";

/** Event channel for task status changes */
export const EVENT_TASK_STATUS_CHANGE = "task-status-change";

// ============================================================================
// Learning Update Event
// ============================================================================

/** Payload for learning update events */
export interface LearningUpdatePayload {
  /** Task ID that generated this outcome */
  task_id: string;
  /** Outcome status (success, failure, partial, abandoned) */
  status: string;
  /** Duration in seconds if completed */
  duration_secs: number | null;
  /** Number of iterations */
  iterations: number | null;
  /** Strategy used */
  strategy: string | null;
  /** Tools used during the task */
  tools_used: string[];
  /** Timestamp when this outcome was recorded */
  timestamp: number;
}

// ============================================================================
// Checkpoint Created Event
// ============================================================================

/** Payload for checkpoint creation events */
export interface CheckpointCreatedPayload {
  /** Unique checkpoint ID */
  checkpoint_id: string;
  /** Task run ID this checkpoint belongs to */
  task_run_id: string;
  /** Iteration number at the time of checkpoint */
  iteration: number;
  /** What triggered this checkpoint */
  trigger: string;
  /** Optional human-readable name */
  name: string | null;
  /** Current state name */
  state: string;
  /** Timestamp when checkpoint was created */
  timestamp: number;
}

// ============================================================================
// Task Status Change Event
// ============================================================================

/** Possible task statuses */
export type TaskStatus =
  | "started"
  | "planning"
  | "executing"
  | "verifying"
  | "completed"
  | "failed"
  | "stopped"
  | "paused";

/** Payload for task status change events */
export interface TaskStatusChangePayload {
  /** Task run ID */
  task_run_id: string;
  /** New status */
  status: TaskStatus;
  /** Current iteration number */
  iteration: number;
  /** Maximum iterations allowed */
  max_iterations: number;
  /** Optional task name */
  task_name: string | null;
  /** Optional reason for status change */
  reason: string | null;
  /** Whether verification passed (for completed/failed) */
  verification_passed: boolean | null;
  /** Timestamp of status change */
  timestamp: number;
}

// ============================================================================
// Union Type for All Realtime Events
// ============================================================================

export type RealtimeEventPayload =
  | { type: "learning-update"; payload: LearningUpdatePayload }
  | { type: "checkpoint-created"; payload: CheckpointCreatedPayload }
  | { type: "task-status-change"; payload: TaskStatusChangePayload };
