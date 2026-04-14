/**
 * useExecutionStatusWidgetData Hook
 *
 * Data hook for the redesigned execution status widget.
 * Fetches from WorkflowExecutionContext for most data and polls
 * the runner HTTP API for session budget information.
 */

import { useState, useEffect, useCallback, useMemo } from "react";
import { useWorkflowExecutionOptional } from "@/contexts/WorkflowExecutionContext";
import { useCurrentTaskRunId, useIsTaskRunning } from "@/contexts/TaskContext";
import type {
  ExecutionStatusWidgetData,
  SessionBudget,
  StageProgress,
  IterationTrend,
  WorkflowTiming,
} from "./types";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

const POLL_INTERVAL_MS = 3000;

/**
 * Derive a human-readable current activity string from workflow state.
 */
function deriveCurrentActivity(
  status: string,
  workflowStage: string | null,
  currentStepName: string | null,
  iteration: number,
  /** `null` = unlimited; rendered as "∞" in iteration counters. */
  maxIterations: number | null,
  stepStats: { total: number; completed: number; failed: number } | null,
  currentStageIndex: number | null,
  totalStages: number | null,
): string {
  if (status === "idle") return "Waiting for workflow";
  if (status === "completed") return "Workflow completed";
  if (status === "failed") return "Workflow failed";
  if (status === "stopped") return "Workflow stopped";

  if (!workflowStage) return "Preparing workflow...";

  const stageLabel =
    totalStages && totalStages > 1 && currentStageIndex !== null
      ? ` (Stage ${currentStageIndex + 1}/${totalStages})`
      : "";

  switch (workflowStage) {
    case "setup":
      return currentStepName
        ? `Setup: ${currentStepName}${stageLabel}`
        : `Running setup${stageLabel}`;

    case "verification": {
      if (stepStats && stepStats.total > 0) {
        const stepNum = Math.min(stepStats.completed + 1, stepStats.total);
        return `Running verification step ${stepNum}/${stepStats.total}`;
      }
      return `Running verification (iteration ${iteration})`;
    }

    case "agentic": {
      const failedCount = stepStats?.failed ?? 0;
      if (failedCount > 0) {
        return `AI fixing ${failedCount} failed check${failedCount !== 1 ? "s" : ""}`;
      }
      return `AI session (iteration ${iteration}/${maxIterations ?? "∞"})`;
    }

    case "completion":
      return currentStepName
        ? `Completion: ${currentStepName}${stageLabel}`
        : `Running completion steps${stageLabel}`;

    default:
      return "Processing...";
  }
}

/**
 * Hook to fetch execution status data for the dashboard widget.
 * Combines WorkflowExecutionContext data with session budget from the runner API.
 */
export function useExecutionStatusWidgetData(): ExecutionStatusWidgetData {
  const context = useWorkflowExecutionOptional();
  const taskRunId = useCurrentTaskRunId();
  const isTaskRunning = useIsTaskRunning();
  const [sessionBudget, setSessionBudget] = useState<SessionBudget>({
    sessionsUsed: 0,
    maxSessions: null,
  });

  // Poll session budget from task-runs API
  const fetchSessionBudget = useCallback(async () => {
    if (!taskRunId) return;
    try {
      const res = await tracedFetch(`${getApiBase()}/task-runs/${taskRunId}`);
      if (res.ok) {
        const data = await res.json();
        setSessionBudget({
          sessionsUsed: data.sessions_count ?? 0,
          maxSessions: data.max_sessions ?? null,
        });
      }
    } catch {
      // Silently ignore fetch errors - API may be temporarily unavailable
    }
  }, [taskRunId]);

  useEffect(() => {
    if (!taskRunId) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- data fetching
      setSessionBudget({ sessionsUsed: 0, maxSessions: null });
      return;
    }

    fetchSessionBudget();

    if (!isTaskRunning) return;

    const interval = setInterval(fetchSessionBudget, POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [isTaskRunning, taskRunId, fetchSessionBudget]);

  // Derive stage progress from context
  const stageProgress = useMemo((): StageProgress | null => {
    if (!context || context.totalStages === null || context.totalStages <= 1) return null;

    const stageResults = [];
    for (let i = 0; i < context.totalStages; i++) {
      const currentIdx = context.currentStageIndex ?? 0;
      const isPast = i < currentIdx;
      const isCurrent = i === currentIdx;

      stageResults.push({
        index: i,
        name: context.currentStageName && isCurrent ? context.currentStageName : `Stage ${i + 1}`,
        status: isPast
          ? ("completed" as const)
          : isCurrent
            ? ("running" as const)
            : ("pending" as const),
      });
    }

    return {
      currentStageIndex: context.currentStageIndex ?? 0,
      totalStages: context.totalStages,
      currentStageName: context.currentStageName,
      stageResults,
    };
  }, [context]);

  // Derive iteration trend from timeline stats
  const iterationTrend = useMemo((): IterationTrend => {
    const results = context?.timelineStats?.verificationResults ?? [];
    return {
      results: results.map((r) => ({
        iteration: r.iteration,
        passed: r.passed,
        total: r.total,
        isComplete: r.isComplete,
      })),
      currentIteration: context?.iteration ?? 0,
      // null = unlimited; pass through verbatim so trend charts can render "∞".
      maxIterations: context?.maxIterations ?? null,
    };
  }, [context]);

  // Derive current activity string
  const currentActivity = useMemo(() => {
    if (!context) return isTaskRunning ? "Preparing workflow..." : "Waiting for workflow";

    return deriveCurrentActivity(
      context.status,
      context.workflowStage,
      context.currentStep?.name ?? null,
      context.iteration,
      context.maxIterations,
      context.stepStats,
      context.currentStageIndex,
      context.totalStages,
    );
  }, [context, isTaskRunning]);

  // Derive timing with ETA
  const timing = useMemo((): WorkflowTiming => {
    const avgIterMs = context?.timelineStats?.avgIterationDurationMs ?? null;
    let etaSeconds: number | null = null;

    // ETA is only meaningful when the loop has a known upper bound. For
    // uncapped runs (`maxIterations == null`) the ETA stays null.
    if (
      context?.status === "running" &&
      avgIterMs &&
      context.maxIterations != null &&
      context.maxIterations > 0 &&
      context.iteration > 0
    ) {
      const remainingIterations = context.maxIterations - context.iteration;
      if (remainingIterations > 0) {
        etaSeconds = Math.round((remainingIterations * avgIterMs) / 1000);
      }
    }

    return {
      elapsedSeconds: context?.elapsedTime ?? 0,
      startTime: context?.startTime ?? null,
      etaSeconds,
      avgIterationDurationMs: avgIterMs,
    };
  }, [context]);

  return {
    sessionBudget,
    stageProgress,
    iterationTrend,
    currentActivity,
    timing,
    status: context?.status ?? (isTaskRunning ? "running" : "idle"),
    workflowName: context?.workflowName ?? null,
    isConnected: context?.isConnected ?? false,
  };
}
