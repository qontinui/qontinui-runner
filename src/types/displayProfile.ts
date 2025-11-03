/**
 * Display Profile Types
 *
 * TypeScript types matching the Rust display profile architecture.
 * These types correspond to the structs defined in src-tauri/src/display/profiles/action_log.rs
 */

/**
 * View data for Action Log display profile.
 * Contains the processed list of actions and metadata for rendering.
 */
export interface ActionLogViewData {
  /** List of actions to display */
  actions: ActionLogEntry[];

  /** Workflow start time (Unix epoch) for calculating relative timestamps */
  workflow_start_time: number | null;

  /** Total number of actions before filtering */
  total_count: number;

  /** Number of visible actions after filtering */
  visible_count: number;
}

/**
 * Single action entry in the action log.
 * Represents a processed action event with all relevant metadata.
 */
export interface ActionLogEntry {
  /** Unique action identifier */
  id: string;

  /** Action type (FIND, CLICK, TYPE, WORKFLOW, etc.) */
  action_type: string;

  /** Start timestamp (Unix epoch in seconds) */
  timestamp: number;

  /** End timestamp (Unix epoch in seconds) */
  end_timestamp: number | null;

  /** Duration in seconds */
  duration: number | null;

  /** Status: "pending" | "running" | "success" | "failed" */
  status: string;

  /** Error message if status is "failed" */
  error: string | null;

  /** Parent action ID for hierarchical relationships */
  parent_action_id: string | null;

  /** Whether this action has children and can be expanded */
  is_expandable: boolean;

  /** Additional metadata as key-value pairs */
  metadata: Record<string, unknown>;
}

/**
 * Standard Tauri command response wrapper.
 * All Tauri commands return this structure.
 */
export interface CommandResponse<T = unknown> {
  /** Whether the command executed successfully */
  success: boolean;

  /** Optional message about the command execution */
  message?: string;

  /** Command response data (type varies by command) */
  data?: T;
}
