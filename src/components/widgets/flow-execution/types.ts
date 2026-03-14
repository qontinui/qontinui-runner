/**
 * Flow Execution Widget Types
 *
 * Types for the Flow Designer execution monitoring widget.
 */

import type { BaseWidgetProps } from "@/types/dashboard/widget-props";

/**
 * Status of a flow execution.
 */
export type FlowExecutionStatus =
  | "idle"
  | "running"
  | "paused"
  | "waiting_for_input"
  | "completed"
  | "failed";

/**
 * A single step execution entry in the flow history.
 */
export interface FlowStepEntry {
  /** Step ID */
  stepId: string;
  /** Step display name */
  stepName: string;
  /** Step type (agent, tool, conditional, etc.) */
  stepType: string;
  /** Whether the step succeeded */
  success: boolean;
  /** Duration in milliseconds */
  durationMs: number;
  /** Step outputs */
  outputs: Record<string, unknown>;
  /** Error message if failed */
  error: string | null;
  /** Timestamp when completed */
  timestamp: number;
}

/**
 * Statistics for flow execution.
 */
export interface FlowExecutionStats {
  /** Total steps in flow */
  totalSteps: number;
  /** Steps completed */
  completedSteps: number;
  /** Successful steps */
  successfulSteps: number;
  /** Failed steps */
  failedSteps: number;
  /** Total elapsed time in milliseconds */
  elapsedMs: number;
  /** Success rate percentage */
  successRate: number;
}

/**
 * Data provided by the useFlowExecutionData hook.
 */
export interface FlowExecutionData {
  /** Current execution instance ID (null if no execution) */
  instanceId: string | null;
  /** Flow ID being executed */
  flowId: string | null;
  /** Flow name */
  flowName: string | null;
  /** Current execution status */
  status: FlowExecutionStatus;
  /** Current step ID being executed */
  currentStepId: string | null;
  /** Execution start time */
  startTime: number | null;
  /** Total elapsed time in milliseconds */
  elapsedMs: number;
  /** Step execution history */
  stepHistory: FlowStepEntry[];
  /** Current context/variables */
  context: Record<string, unknown>;
  /** Error message if execution failed */
  error: string | null;
  /** Execution statistics */
  stats: FlowExecutionStats;
  /** Whether there's an active execution */
  isActive: boolean;
}

/**
 * Props for the full FlowExecutionWidget component.
 */
export interface FlowExecutionWidgetProps extends BaseWidgetProps {
  data: FlowExecutionData;
}

/**
 * Props for the FlowExecutionSummary component.
 */
export interface FlowExecutionSummaryProps extends BaseWidgetProps {
  data: FlowExecutionData;
}
