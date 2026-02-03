/**
 * useWorkflowExecution Hooks
 *
 * Selector hooks for accessing specific parts of the WorkflowExecutionContext.
 * These provide backward compatibility with existing hooks while consuming
 * from the unified state store.
 */

import { useMemo } from "react";
import {
  useWorkflowExecution,
  useWorkflowExecutionOptional,
  type StepCheckpoint,
  type StepProgress,
  type ResumePoint,
} from "../contexts/WorkflowExecutionContext";
import type { WorkflowStage } from "../types/dashboard/activity-types";
import type {
  TimelineStep,
  ExecutionTimelineData,
} from "../components/active-dashboard/widgets/execution-timeline/types";
import type { StepStats } from "../components/active-dashboard/widgets/shared/types";

// Re-export context hooks
export { useWorkflowExecution, useWorkflowExecutionOptional };

// Re-export types
export type { StepCheckpoint, StepProgress, ResumePoint };

// =============================================================================
// Orchestrator State Selector
// =============================================================================

/**
 * Backward-compatible hook for orchestrator state.
 * @deprecated Use useWorkflowExecution() directly for new code.
 */
export interface OrchestratorStateResult {
  workflowStage: WorkflowStage | null;
  workflowStageDisplay: string | null;
  iteration: number;
  maxIterations: number;
  hasVerificationPlan: boolean;
  isComplete: boolean;
  isPaused: boolean;
  isStopped: boolean;
  isOrchestrated: boolean;
  isLoading: boolean;
  error: string | null;
  refresh: () => Promise<void>;
}

/**
 * Hook to get orchestrator state from the unified context.
 * Backward compatible with the original useOrchestratorState hook.
 */
export function useWorkflowOrchestratorState(): OrchestratorStateResult {
  const ctx = useWorkflowExecution();

  return useMemo(
    () => ({
      workflowStage: ctx.workflowStage,
      workflowStageDisplay: ctx.workflowStageDisplay,
      iteration: ctx.iteration,
      maxIterations: ctx.maxIterations,
      hasVerificationPlan: ctx.hasVerificationPlan,
      isComplete: ctx.isComplete,
      isPaused: ctx.isPaused,
      isStopped: ctx.isStopped,
      isOrchestrated: ctx.isOrchestrated,
      isLoading: ctx.isLoading,
      error: ctx.error,
      refresh: ctx.refresh,
    }),
    [
      ctx.workflowStage,
      ctx.workflowStageDisplay,
      ctx.iteration,
      ctx.maxIterations,
      ctx.hasVerificationPlan,
      ctx.isComplete,
      ctx.isPaused,
      ctx.isStopped,
      ctx.isOrchestrated,
      ctx.isLoading,
      ctx.error,
      ctx.refresh,
    ],
  );
}

// =============================================================================
// Timeline Data Selector
// =============================================================================

/**
 * Hook to get execution timeline data from the unified context.
 * Backward compatible with the original useExecutionTimelineData hook.
 */
export function useWorkflowTimelineData(): ExecutionTimelineData {
  const ctx = useWorkflowExecution();

  return useMemo(
    () => ({
      phaseGroups: ctx.phaseGroups,
      allSteps: ctx.steps,
      currentStep: ctx.currentStep,
      currentPhase: ctx.workflowStage,
      stats: ctx.timelineStats,
      stepStats: ctx.stepStats,
      isLoading: ctx.isLoading,
      error: ctx.error,
      workflowName: ctx.workflowName,
      taskRunId: ctx.taskRunId,
    }),
    [
      ctx.phaseGroups,
      ctx.steps,
      ctx.currentStep,
      ctx.workflowStage,
      ctx.timelineStats,
      ctx.stepStats,
      ctx.isLoading,
      ctx.error,
      ctx.workflowName,
      ctx.taskRunId,
    ],
  );
}

// =============================================================================
// Checkpoint Selectors
// =============================================================================

/**
 * Hook to get checkpoint data from the unified context.
 */
export interface CheckpointData {
  checkpoints: StepCheckpoint[];
  lastCompletedCheckpoint: StepCheckpoint | null;
  resumePoint: ResumePoint | null;
  currentStepProgress: StepProgress | null;
}

export function useWorkflowCheckpoints(): CheckpointData {
  const ctx = useWorkflowExecution();

  return useMemo(
    () => ({
      checkpoints: ctx.checkpoints,
      lastCompletedCheckpoint: ctx.lastCompletedCheckpoint,
      resumePoint: ctx.resumePoint,
      currentStepProgress: ctx.currentStepProgress,
    }),
    [ctx.checkpoints, ctx.lastCompletedCheckpoint, ctx.resumePoint, ctx.currentStepProgress],
  );
}

/**
 * Hook to get progress for a specific step.
 */
export function useStepProgress(checkpointId: string | null): StepProgress | null {
  const ctx = useWorkflowExecution();

  return useMemo(() => {
    if (!checkpointId) return null;
    if (ctx.currentStepProgress?.checkpointId === checkpointId) {
      return ctx.currentStepProgress;
    }
    return null;
  }, [checkpointId, ctx.currentStepProgress]);
}

// =============================================================================
// Simple Selectors
// =============================================================================

/**
 * Hook to get the current task run ID.
 */
export function useCurrentTaskRunIdFromContext(): string | null {
  const ctx = useWorkflowExecutionOptional();
  return ctx?.taskRunId ?? null;
}

/**
 * Hook to check if a task is running.
 */
export function useIsTaskRunningFromContext(): boolean {
  const ctx = useWorkflowExecutionOptional();
  return ctx?.status === "running";
}

/**
 * Hook to get connection status.
 */
export interface WorkflowConnectionStatus {
  isConnected: boolean;
  connectionMethod: "tauri" | "websocket" | null;
  isReconnecting: boolean;
  lastEventTimestamp: number | null;
}

export function useWorkflowConnectionStatus(): WorkflowConnectionStatus {
  const ctx = useWorkflowExecution();

  return useMemo(
    () => ({
      isConnected: ctx.isConnected,
      connectionMethod: ctx.connectionMethod,
      isReconnecting: ctx.isReconnecting,
      lastEventTimestamp: ctx.lastEventTimestamp,
    }),
    [ctx.isConnected, ctx.connectionMethod, ctx.isReconnecting, ctx.lastEventTimestamp],
  );
}

/**
 * Hook to get workflow status summary.
 */
export interface WorkflowStatusSummary {
  status: "idle" | "running" | "completed" | "failed" | "stopped";
  workflowStage: WorkflowStage | null;
  iteration: number;
  maxIterations: number;
  isComplete: boolean;
  isStopped: boolean;
  elapsedTime: number;
}

export function useWorkflowStatus(): WorkflowStatusSummary {
  const ctx = useWorkflowExecution();

  return useMemo(
    () => ({
      status: ctx.status,
      workflowStage: ctx.workflowStage,
      iteration: ctx.iteration,
      maxIterations: ctx.maxIterations,
      isComplete: ctx.isComplete,
      isStopped: ctx.isStopped,
      elapsedTime: ctx.elapsedTime,
    }),
    [
      ctx.status,
      ctx.workflowStage,
      ctx.iteration,
      ctx.maxIterations,
      ctx.isComplete,
      ctx.isStopped,
      ctx.elapsedTime,
    ],
  );
}

/**
 * Hook to get the current step.
 */
export function useCurrentStep(): TimelineStep | null {
  const ctx = useWorkflowExecutionOptional();
  return ctx?.currentStep ?? null;
}

/**
 * Hook to get step statistics.
 */
export function useWorkflowStepStats(): StepStats {
  const ctx = useWorkflowExecutionOptional();
  return (
    ctx?.stepStats ?? {
      total: 0,
      completed: 0,
      successful: 0,
      failed: 0,
      pending: 0,
      elapsedTime: 0,
      successRate: 0,
    }
  );
}

export default useWorkflowExecution;
