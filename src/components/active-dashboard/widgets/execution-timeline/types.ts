/**
 * Execution Timeline Widget Types
 *
 * Type definitions for the execution timeline widget.
 * Shows all workflow steps chronologically, grouped by phase.
 */

import type { BaseWidgetProps } from "../../../../types/dashboard/widget-props";
import type { StepStats, StepExecutionStatus } from "../shared/types";
import type { WorkflowStage } from "../../../../types/dashboard/activity-types";

/**
 * Step type categories for grouping and icons.
 */
export type StepType =
  | "shell"
  | "check_group"
  | "check"
  | "prompt"
  | "api_request"
  | "script"
  | "workflow_ref"
  | "mcp_call"
  | "gui_action"
  | "playwright"
  | "unknown";

/**
 * Individual step in the execution timeline.
 */
export interface TimelineStep {
  /** Unique step ID */
  id: string;
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
}

/**
 * Group of steps in a phase.
 */
export interface PhaseGroup {
  /** Phase name */
  phase: WorkflowStage;
  /** Steps in this phase */
  steps: TimelineStep[];
  /** Whether this phase is currently active */
  isActive: boolean;
  /** Whether this phase is complete */
  isComplete: boolean;
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
  /** Overall execution statistics */
  stats: StepStats;
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
    total: 0,
    completed: 0,
    successful: 0,
    failed: 0,
    pending: 0,
    elapsedTime: 0,
    successRate: 100,
  },
  isLoading: true,
  error: null,
  workflowName: null,
  taskRunId: null,
};
