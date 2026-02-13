/**
 * Execution Timeline Widget Types
 *
 * Type definitions for the execution timeline widget.
 * Shows all workflow steps chronologically, grouped by phase.
 */

import type { BaseWidgetProps } from "../../../../types/dashboard/widget-props";
import type { StepExecutionStatus, StepStats } from "../shared/types";
import type { WorkflowStage } from "../../../../types/dashboard/activity-types";

/**
 * Step type categories for grouping and icons.
 * Aligned with backend StepType enum in src-tauri/src/step_types.rs
 */
export type StepType =
  // GUI Automation
  | "workflow"
  | "state"
  | "action"
  | "screenshot"
  | "gui_action" // Alias for backward compatibility
  | "workflow_ref" // Alias for backward compatibility
  // Verification
  | "playwright"
  | "test"
  | "check"
  | "check_group"
  // Command
  | "shell"
  | "api_request"
  | "mcp_call"
  // AI
  | "prompt"
  | "ai_session"
  // AWAS (Web Automation)
  | "awas"
  // Utility
  | "macro"
  | "script" // Alias for backward compatibility
  // Unknown fallback
  | "unknown";

/**
 * Individual step in the execution timeline.
 */
export interface TimelineStep {
  /** Unique step ID */
  id: string;
  /** Checkpoint ID for fetching progress markers */
  checkpointId?: string;
  /** Step type */
  type: StepType;
  /** Display name of the step */
  name: string;
  /** Execution status */
  status: StepExecutionStatus;
  /** Phase this step belongs to */
  phase: WorkflowStage;
  /** Step index within the phase */
  stepIndex: number;
  /** Iteration number for verification/agentic phases (1-indexed) */
  iteration?: number;
  /** Start timestamp (ms since epoch) */
  startTime?: number;
  /** End timestamp (ms since epoch) */
  endTime?: number;
  /** Duration in milliseconds */
  durationMs?: number;
  /** Error message if failed */
  error?: string;
  /** Brief output/result preview */
  outputPreview?: string;
  /** Progress marker data (if available) */
  progress?: {
    current: number;
    total: number | null;
    type: string;
    description?: string;
  };
}

/**
 * Group of steps within an iteration (for verification/agentic phases).
 */
export interface IterationGroup {
  /** Iteration number (1-indexed) */
  iteration: number;
  /** Steps in this iteration */
  steps: TimelineStep[];
  /** Whether this iteration is currently active */
  isActive: boolean;
  /** Whether this iteration is complete */
  isComplete: boolean;
  /** Iteration statistics */
  stats: {
    total: number;
    completed: number;
    successful: number;
    failed: number;
  };
}

/**
 * Verification result for an iteration.
 * Tracks tests passed/total for improvement calculation.
 */
export interface VerificationResult {
  /** Iteration number (1-indexed) */
  iteration: number;
  /** Number of tests/checks that passed */
  passed: number;
  /** Total number of tests/checks */
  total: number;
  /** Duration of the verification phase in ms */
  durationMs?: number;
  /** Whether all check steps in this iteration have completed (success or failed) */
  isComplete?: boolean;
}

/**
 * Timeline-specific statistics.
 * Shows iteration progress and improvement metrics.
 */
export interface TimelineStats {
  /** Total elapsed time in seconds */
  elapsedTime: number;
  /** Current iteration number (1-indexed, null if not in iteration loop) */
  currentIteration: number | null;
  /** Maximum iteration seen so far */
  maxIteration: number;
  /** Average duration of complete iterations (verification + agentic) in ms */
  avgIterationDurationMs: number | null;
  /** Verification results per iteration for improvement tracking */
  verificationResults: VerificationResult[];
  /** Improvement from previous iteration (e.g., "+2/12 tests" = 2 more passed) */
  improvement: {
    /** Change in passed tests from previous iteration */
    delta: number;
    /** Total tests in verification */
    total: number;
    /** Percentage improvement (can be negative) */
    percentage: number;
  } | null;
}

/**
 * Group of steps in a phase.
 */
export interface PhaseGroup {
  /** Phase name */
  phase: WorkflowStage;
  /** Steps in this phase (flat list, sorted by start time) */
  steps: TimelineStep[];
  /** Steps grouped by iteration (for verification/agentic phases) */
  iterationGroups: IterationGroup[];
  /** Whether this phase has iterations (verification/agentic) */
  hasIterations: boolean;
  /** Current iteration number (for verification/agentic phases) */
  currentIteration?: number;
  /** Whether this phase is currently active */
  isActive: boolean;
  /** Whether this phase is complete */
  isComplete: boolean;
  /** Whether this phase hasn't started yet (defined in workflow but no steps executed) */
  isUpcoming: boolean;
  /** Phase statistics */
  stats: {
    total: number;
    completed: number;
    successful: number;
    failed: number;
  };
}

/**
 * Data provided by the useExecutionTimelineData hook.
 */
export interface ExecutionTimelineData {
  /** All steps grouped by phase */
  phaseGroups: PhaseGroup[];
  /** Flat list of all steps (chronological) */
  allSteps: TimelineStep[];
  /** Currently running step (if any) */
  currentStep: TimelineStep | null;
  /** Current active phase */
  currentPhase: WorkflowStage | null;
  /** Timeline-specific statistics (iteration progress, improvement) */
  stats: TimelineStats;
  /** Step execution statistics (counts, success rate) */
  stepStats: StepStats;
  /** Whether data is loading */
  isLoading: boolean;
  /** Error message if fetch failed */
  error: string | null;
  /** Workflow name */
  workflowName: string | null;
  /** Task run ID */
  taskRunId: string | null;
}

/**
 * Props for the full ExecutionTimelineWidget component.
 */
export interface ExecutionTimelineWidgetProps extends BaseWidgetProps {
  data: ExecutionTimelineData;
}

/**
 * Props for the ExecutionTimelineSummary component.
 */
export interface ExecutionTimelineSummaryProps extends BaseWidgetProps {
  data: ExecutionTimelineData;
}

/**
 * Default empty state for timeline data.
 */
export const DEFAULT_TIMELINE_DATA: ExecutionTimelineData = {
  phaseGroups: [],
  allSteps: [],
  currentStep: null,
  currentPhase: null,
  stats: {
    elapsedTime: 0,
    currentIteration: null,
    maxIteration: 0,
    avgIterationDurationMs: null,
    verificationResults: [],
    improvement: null,
  },
  stepStats: {
    total: 0,
    completed: 0,
    successful: 0,
    failed: 0,
    pending: 0,
    elapsedTime: 0,
    successRate: 0,
  },
  isLoading: true,
  error: null,
  workflowName: null,
  taskRunId: null,
};
