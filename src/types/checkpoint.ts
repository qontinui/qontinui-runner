/**
 * Checkpoint Browser types (Time-Travel Debugging)
 *
 * Types for checkpointing, state snapshots, and replay capabilities.
 * Enables saving execution state at key points and replaying from any checkpoint.
 */

/** Snapshot of a channel's state */
export interface ChannelSnapshot {
  name: string;
  value: unknown;
  version: number;
}

/** Snapshot of a knowledge entry */
export interface KnowledgeEntry {
  id: string;
  category: string;
  content: string;
  iteration: number;
}

/** Result of a verification criterion */
export interface CriterionResult {
  criterion_id: string;
  passed: boolean;
  reason: string | null;
  verified_at: string;
}

/** Snapshot of verification state */
export interface VerificationSnapshot {
  criteria_results: Record<string, CriterionResult>;
  overall_passed: boolean;
}

/** Snapshot of a finding */
export interface FindingSnapshot {
  id: string;
  category: string;
  severity: string;
  description: string;
  resolved: boolean;
}

/** A snapshot of execution state */
export interface StateSnapshot {
  /** Current orchestrator state name */
  state: string;
  /** Current iteration */
  iteration: number;
  /** Channel values with versions */
  channels: Record<string, ChannelSnapshot>;
  /** Knowledge base entries */
  knowledge: KnowledgeEntry[];
  /** Verification status */
  verification: VerificationSnapshot;
  /** Active findings */
  findings: FindingSnapshot[];
  /** Files modified */
  files_modified: string[];
  /** Custom state data */
  custom_data: Record<string, unknown>;
}

/** What triggered the checkpoint */
export type CheckpointTrigger =
  | { type: "automatic"; reason: string }
  | { type: "manual" }
  | { type: "before_operation"; operation: string }
  | { type: "after_success"; operation: string }
  | { type: "after_failure"; error: string }
  | { type: "verification_boundary" }
  | { type: "iteration_boundary"; iteration: number }
  | { type: "custom"; trigger: string };

/** A checkpoint of execution state */
export interface Checkpoint {
  /** Unique checkpoint ID */
  id: string;
  /** Task ID this checkpoint belongs to */
  task_id: string;
  /** Human-readable name */
  name: string | null;
  /** Description of why this checkpoint was created */
  description: string | null;
  /** The state snapshot */
  state: StateSnapshot;
  /** When this checkpoint was created */
  created_at: string;
  /** Event that triggered this checkpoint */
  trigger: CheckpointTrigger;
  /** Tags for categorization */
  tags: string[];
  /** Parent checkpoint ID (for branching) */
  parent_id: string | null;
  /** Checkpoint metadata */
  metadata: Record<string, unknown>;
}

/** Summary of a checkpoint for listing */
export interface CheckpointSummary {
  id: string;
  task_id: string;
  name: string | null;
  state: string;
  iteration: number;
  created_at: string;
  trigger_type: string;
  tags: string[];
}

/** Settings for automatic checkpointing */
export interface AutoCheckpointSettings {
  /** Checkpoint every N iterations */
  every_n_iterations: number | null;
  /** Checkpoint on state changes */
  on_state_change: boolean;
  /** Checkpoint before verification */
  before_verification: boolean;
  /** Checkpoint on failures */
  on_failure: boolean;
}

/** A change to a channel */
export interface ChannelChange {
  channel: string;
  from_version: number;
  to_version: number;
}

/** Diff between two checkpoints */
export interface CheckpointDiff {
  /** From checkpoint ID */
  from_id: string;
  /** To checkpoint ID */
  to_id: string;
  /** State difference: [from_state, to_state] */
  state_change: [string, string] | null;
  /** Iteration difference */
  iteration_diff: number;
  /** Changed channels */
  channel_changes: ChannelChange[];
  /** Added findings (IDs) */
  added_findings: string[];
  /** Resolved findings (IDs) */
  resolved_findings: string[];
}

/** Status of a replay session */
export type ReplayStatus = "pending" | "running" | "completed" | "failed" | "cancelled";

/** A modification made during replay */
export type ModificationType =
  | { type: "channel_change"; channel: string; old_value: unknown; new_value: unknown }
  | { type: "state_change"; old_state: string; new_state: string }
  | { type: "knowledge_add"; entry_id: string }
  | { type: "knowledge_remove"; entry_id: string }
  | { type: "custom"; data: unknown };

/** A modification record during replay */
export interface ReplayModification {
  /** Type of modification */
  modification_type: ModificationType;
  /** Description of the modification */
  description: string;
  /** When the modification was applied */
  applied_at: string;
}

/** A replay session from a checkpoint */
export interface ReplaySession {
  /** Session ID */
  id: string;
  /** Original task ID */
  original_task_id: string;
  /** New task ID for this replay */
  replay_task_id: string;
  /** Starting checkpoint ID */
  checkpoint_id: string;
  /** Current state */
  state: StateSnapshot;
  /** Modifications made during replay */
  modifications: ReplayModification[];
  /** When the replay started */
  started_at: string;
  /** Replay status */
  status: ReplayStatus;
}

/** Checkpoint statistics for dashboard */
export interface CheckpointStats {
  total_checkpoints: number;
  tasks_with_checkpoints: number;
  checkpoints_by_trigger: Record<string, number>;
}

// ============================================================================
// Lineage Types (Time-Travel Debugging)
// ============================================================================

/** Information about a task's replay lineage */
export interface LineageInfo {
  /** The task run ID */
  task_run_id: string;
  /** The checkpoint this task was replayed from (if any) */
  source_checkpoint_id: string | null;
  /** The original task ID (root of the lineage tree) */
  original_task_id: string;
  /** Parent task ID (immediate parent) */
  parent_task_id: string | null;
  /** Depth in the replay tree (0 = original, 1 = first replay, etc.) */
  replay_depth: number;
  /** When this lineage entry was created */
  created_at: string;
  /** IDs of tasks replayed from this task */
  child_task_ids: string[];
}

/** A node in the lineage tree */
export interface LineageNode {
  /** Task run ID */
  task_id: string;
  /** Checkpoint ID this was replayed from (None for original) */
  checkpoint_id: string | null;
  /** Parent task ID (None for root) */
  parent_task_id: string | null;
  /** Depth in the tree (0 = root) */
  depth: number;
  /** When this task was created */
  created_at: string;
  /** Whether this is the current/queried task */
  is_current: boolean;
}

/** A tree representation of task lineage */
export interface LineageTree {
  /** The root (original) task ID */
  root_task_id: string;
  /** All nodes in the tree */
  nodes: LineageNode[];
}

/** Instructions for restoring orchestrator state */
export interface RestorationInstructions {
  /** The target state to transition to */
  target_state: string;
  /** The iteration to resume from */
  resume_iteration: number;
  /** Channel values to restore */
  channels: Record<string, unknown>;
  /** Knowledge entries to restore */
  knowledge: KnowledgeEntry[];
  /** Findings to restore */
  findings: FindingSnapshot[];
  /** Verification state to restore */
  verification: VerificationSnapshot;
  /** Custom data to restore */
  custom_data: Record<string, unknown>;
  /** Files that were modified */
  files_modified: string[];
  /** The checkpoint this restoration is from */
  source_checkpoint_id: string;
  /** The original task ID */
  original_task_id: string;
}

/** Response from replay_from_checkpoint command */
export interface ReplayFromCheckpointResponse {
  /** The replay session */
  session: ReplaySession;
  /** Lineage information for the new task */
  lineage: LineageInfo;
  /** The new task run ID */
  new_task_run_id: string;
  /** Instructions for restoring state */
  restoration_instructions: RestorationInstructions;
}

/** Trigger type display labels */
export const TRIGGER_TYPE_LABELS: Record<string, string> = {
  automatic: "Auto",
  manual: "Manual",
  before_operation: "Before Op",
  after_success: "Success",
  after_failure: "Failure",
  verification_boundary: "Verification",
  iteration_boundary: "Iteration",
  custom: "Custom",
};

/** Trigger type colors for visualization */
export const TRIGGER_TYPE_COLORS: Record<string, string> = {
  automatic: "#6b7280", // gray
  manual: "#3b82f6", // blue
  before_operation: "#f59e0b", // amber
  after_success: "#22c55e", // green
  after_failure: "#ef4444", // red
  verification_boundary: "#a855f7", // purple
  iteration_boundary: "#06b6d4", // cyan
  custom: "#ec4899", // pink
};

/** Format a timestamp for display */
export function formatTimestamp(timestamp: string): string {
  const date = new Date(timestamp);
  return date.toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

/** Format full datetime for display */
export function formatDateTime(timestamp: string): string {
  const date = new Date(timestamp);
  return date.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

// ============================================================================
// Enhanced Query Types (Filtering, Pagination)
// ============================================================================

/** Filter options for checkpoint query */
export interface CheckpointFilter {
  task_id?: string;
  trigger?: string;
  since?: string;
}

/** Paginated checkpoint result */
export interface PaginatedCheckpointResult {
  items: CheckpointSummary[];
  total: number;
  offset: number;
  limit: number;
}
