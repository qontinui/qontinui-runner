/**
 * Checkpoint Service
 *
 * Provides Tauri invoke calls for the Checkpoint Browser (Time-Travel Debugging).
 * Wraps the checkpoint_browser Tauri commands from src-tauri/src/commands/checkpoint_browser.rs
 */

import { invoke } from "@tauri-apps/api/core";
import type {
  Checkpoint,
  CheckpointSummary,
  CheckpointDiff,
  ReplaySession,
  CheckpointStats,
  LineageInfo,
  LineageTree,
  ReplayFromCheckpointResponse,
  CheckpointFilter,
  PaginatedCheckpointResult,
} from "../types/checkpoint";

/**
 * Service for managing checkpoints and time-travel debugging via Tauri commands.
 */
export const checkpointService = {
  // ===========================================================================
  // Checkpoint Listing & Retrieval
  // ===========================================================================

  /**
   * List all checkpoints, optionally filtered by task ID.
   * @param taskId - Optional task ID to filter by.
   * @returns Array of checkpoint summaries.
   */
  async listCheckpoints(taskId?: string): Promise<CheckpointSummary[]> {
    return invoke("list_orchestrator_checkpoints", { taskId });
  },

  /**
   * Get a checkpoint by ID.
   * @param id - Checkpoint ID.
   * @returns The checkpoint or null if not found.
   */
  async getCheckpoint(id: string): Promise<Checkpoint | null> {
    return invoke("get_orchestrator_checkpoint", { id });
  },

  /**
   * Get the latest checkpoint for a task.
   * @param taskId - Task ID.
   * @returns The latest checkpoint or null if none exist.
   */
  async getLatestCheckpoint(taskId: string): Promise<Checkpoint | null> {
    return invoke("get_latest_checkpoint", { taskId });
  },

  /**
   * Find checkpoints by tag.
   * @param tag - Tag to search for.
   * @returns Array of matching checkpoint summaries.
   */
  async findCheckpointsByTag(tag: string): Promise<CheckpointSummary[]> {
    return invoke("find_checkpoints_by_tag", { tag });
  },

  // ===========================================================================
  // Checkpoint Management
  // ===========================================================================

  /**
   * Create a manual checkpoint for a task.
   * @param taskId - Task ID.
   * @param state - Current state name.
   * @param iteration - Current iteration.
   * @param name - Optional checkpoint name.
   * @param description - Optional description.
   * @returns The created checkpoint ID.
   */
  async createCheckpoint(
    taskId: string,
    state: string,
    iteration: number,
    name?: string,
    description?: string,
  ): Promise<string> {
    return invoke("create_orchestrator_checkpoint", {
      taskId,
      state,
      iteration,
      name,
      description,
    });
  },

  /**
   * Delete a checkpoint by ID.
   * @param id - Checkpoint ID to delete.
   * @returns True if deleted, false if not found.
   */
  async deleteCheckpoint(id: string): Promise<boolean> {
    return invoke("delete_orchestrator_checkpoint", { id });
  },

  // ===========================================================================
  // Comparison & Analysis
  // ===========================================================================

  /**
   * Compare two checkpoints and return the diff.
   * @param fromId - Source checkpoint ID.
   * @param toId - Target checkpoint ID.
   * @returns Diff showing changes between checkpoints.
   */
  async compareCheckpoints(fromId: string, toId: string): Promise<CheckpointDiff> {
    return invoke("compare_orchestrator_checkpoints", { fromId, toId });
  },

  /**
   * Get checkpoint statistics.
   * @returns Stats including counts and breakdown by trigger type.
   */
  async getCheckpointStats(): Promise<CheckpointStats> {
    return invoke("get_checkpoint_stats");
  },

  /**
   * Get total checkpoint count.
   * @returns Number of checkpoints.
   */
  async getCheckpointCount(): Promise<number> {
    return invoke("get_checkpoint_count");
  },

  /**
   * Get unique task IDs that have checkpoints.
   * @returns Array of task IDs.
   */
  async getCheckpointTaskIds(): Promise<string[]> {
    return invoke("get_checkpoint_task_ids");
  },

  // ===========================================================================
  // Replay
  // ===========================================================================

  /**
   * Start a replay session from a checkpoint (legacy method).
   * @param checkpointId - Checkpoint ID to replay from.
   * @returns The replay session.
   * @deprecated Use replayFromCheckpoint for full functionality.
   */
  async startReplaySession(checkpointId: string): Promise<ReplaySession> {
    return invoke("start_replay_session", { checkpointId });
  },

  /**
   * Start a full replay from a checkpoint.
   * Creates a new task run branched from the checkpoint's state.
   * @param checkpointId - Checkpoint ID to replay from.
   * @returns Full replay response with session, lineage, and restoration instructions.
   */
  async replayFromCheckpoint(checkpointId: string): Promise<ReplayFromCheckpointResponse> {
    return invoke("replay_from_checkpoint", { checkpointId });
  },

  /**
   * Get the replay lineage tree for a task.
   * @param taskRunId - Task run ID.
   * @returns The lineage tree or null if not tracked.
   */
  async getReplayLineage(taskRunId: string): Promise<LineageTree | null> {
    return invoke("get_replay_lineage", { taskRunId });
  },

  /**
   * Get lineage info for a specific task.
   * @param taskRunId - Task run ID.
   * @returns Lineage info or null if not tracked.
   */
  async getTaskLineageInfo(taskRunId: string): Promise<LineageInfo | null> {
    return invoke("get_task_lineage_info", { taskRunId });
  },

  /**
   * Register a task for lineage tracking (for original, non-replayed tasks).
   * @param taskRunId - Task run ID to register.
   */
  async registerTaskForLineage(taskRunId: string): Promise<void> {
    return invoke("register_task_for_lineage", { taskRunId });
  },

  /**
   * List all active replay sessions.
   * @returns Array of active replay sessions.
   */
  async listActiveReplaySessions(): Promise<ReplaySession[]> {
    return invoke("list_active_replay_sessions");
  },

  /**
   * Complete a replay session.
   * @param sessionId - Replay session ID.
   */
  async completeReplaySession(sessionId: string): Promise<void> {
    return invoke("complete_replay_session", { sessionId });
  },

  /**
   * Fail a replay session.
   * @param sessionId - Replay session ID.
   * @param error - Optional error message.
   */
  async failReplaySession(sessionId: string, error?: string): Promise<void> {
    return invoke("fail_replay_session", { sessionId, error });
  },

  // ===========================================================================
  // Sample Data & Reset
  // ===========================================================================

  /**
   * Add sample checkpoints for demonstration.
   */
  async addSampleCheckpoints(): Promise<void> {
    return invoke("add_sample_checkpoints");
  },

  /**
   * Clear all checkpoints (for testing/reset).
   */
  async clearAllCheckpoints(): Promise<void> {
    return invoke("clear_all_checkpoints");
  },

  // ===========================================================================
  // Enhanced Queries (Filtering, Pagination)
  // ===========================================================================

  /**
   * Get checkpoints with optional filtering.
   * @param filter - Filter options (task_id, trigger, since).
   * @returns Array of filtered checkpoint summaries.
   */
  async getCheckpointsFiltered(filter: CheckpointFilter = {}): Promise<CheckpointSummary[]> {
    return invoke("get_checkpoints_filtered", { filter });
  },

  /**
   * Get checkpoints with pagination.
   * @param taskId - Optional task ID to filter by.
   * @param offset - Number of records to skip.
   * @param limit - Maximum number of records to return.
   * @returns Paginated result with items, total count, offset, and limit.
   */
  async getCheckpointsPaginated(
    taskId: string | undefined,
    offset: number,
    limit: number,
  ): Promise<PaginatedCheckpointResult> {
    return invoke("get_checkpoints_paginated", { taskId, offset, limit });
  },

  /**
   * Get total count of checkpoints.
   * @param taskId - Optional task ID to filter by.
   * @returns Total number of checkpoint records.
   */
  async getCheckpointsCount(taskId?: string): Promise<number> {
    return invoke("get_checkpoints_count", { taskId });
  },

  /**
   * Get the most recent task ID that has checkpoints.
   * Used for auto-selecting in the checkpoint browser.
   * @returns The task ID or null if no tasks have checkpoints.
   */
  async getMostRecentTaskWithCheckpoints(): Promise<string | null> {
    return invoke("get_most_recent_task_with_checkpoints");
  },
};
