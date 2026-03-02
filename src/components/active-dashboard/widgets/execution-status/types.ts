/**
 * Execution Status Widget Types
 *
 * Type definitions for the redesigned execution status dashboard widget.
 * Shows session budget, stage progress, iteration trend, current activity, and timing.
 */

export interface SessionBudget {
  sessionsUsed: number;
  maxSessions: number | null;
}

export interface StageResult {
  index: number;
  name: string;
  status: "pending" | "running" | "completed" | "failed";
}

export interface StageProgress {
  currentStageIndex: number;
  totalStages: number;
  currentStageName: string | null;
  stageResults: StageResult[];
}

export interface IterationResult {
  iteration: number;
  passed: number;
  total: number;
  isComplete: boolean;
}

export interface IterationTrend {
  results: IterationResult[];
  currentIteration: number;
  maxIterations: number;
}

export interface WorkflowTiming {
  elapsedSeconds: number;
  startTime: number | null;
  etaSeconds: number | null;
  avgIterationDurationMs: number | null;
}

/**
 * Data structure for the redesigned execution status widget.
 */
export interface ExecutionStatusWidgetData {
  /** Session budget info (sessions used vs max) */
  sessionBudget: SessionBudget;
  /** Stage progress for multi-stage workflows (null if single-stage) */
  stageProgress: StageProgress | null;
  /** Iteration trend with verification pass rates */
  iterationTrend: IterationTrend;
  /** Current activity description */
  currentActivity: string;
  /** Timing information (elapsed, ETA) */
  timing: WorkflowTiming;
  /** Overall workflow status */
  status: "idle" | "running" | "completed" | "failed" | "stopped";
  /** Workflow name */
  workflowName: string | null;
  /** Whether connected to live events */
  isConnected: boolean;
}
