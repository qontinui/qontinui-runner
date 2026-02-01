/**
 * Workflow Queue Types
 *
 * Types for the workflow queue page that allows users to
 * select, order, and execute unified workflows as a sequence.
 */

import type { UnifiedWorkflow } from "../../types/unified-workflow";

/**
 * Execution statistics for a workflow
 */
export interface WorkflowStats {
  totalRuns: number;
  successCount: number;
  failureCount: number;
  lastRunAt: string | null;
  lastRunStatus: "complete" | "failed" | "stopped" | null;
  avgDurationMs: number | null;
}

/**
 * Workflow with execution statistics
 */
export interface WorkflowWithStats extends UnifiedWorkflow {
  stats: WorkflowStats;
}

/**
 * Queue item with position
 */
export interface QueuedWorkflow {
  /** Unique ID for this queue entry (allows same workflow multiple times) */
  queueId: string;
  /** The workflow data with stats */
  workflow: WorkflowWithStats;
  /** Position in the queue (0-indexed) */
  position: number;
}

/**
 * Queue execution options
 */
export interface QueueExecutionOptions {
  /** Whether to stop the sequence if a workflow fails */
  stopOnFailure: boolean;
}
