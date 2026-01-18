/**
 * useOrchestratorState Hook
 *
 * Fetches and tracks the orchestrator state for a running task.
 * Provides the current workflow stage (Setup, Agentic, Verification, Completion).
 */

import { useState, useEffect, useCallback } from "react";
import type { WorkflowStage } from "../../types/dashboard/activity-types";

const API_BASE = "http://localhost:9876";
const POLL_INTERVAL_MS = 1000;

/**
 * Response from the orchestrator state API endpoint.
 */
export interface OrchestratorStateResponse {
  task_run_id: string;
  current_state: string;
  workflow_stage: WorkflowStage;
  workflow_stage_display: string;
  iteration: number;
  max_iterations: number;
  has_verification_plan: boolean;
  is_complete: boolean;
  is_paused: boolean;
  is_stopped: boolean;
}

/**
 * State returned by the useOrchestratorState hook.
 */
export interface OrchestratorStateResult {
  /** Current workflow stage */
  workflowStage: WorkflowStage | null;
  /** Display name for the workflow stage */
  workflowStageDisplay: string | null;
  /** Current iteration */
  iteration: number;
  /** Maximum iterations */
  maxIterations: number;
  /** Whether the task has a verification plan */
  hasVerificationPlan: boolean;
  /** Whether the task is complete */
  isComplete: boolean;
  /** Whether the task is paused */
  isPaused: boolean;
  /** Whether the task is stopped */
  isStopped: boolean;
  /** Whether an orchestrated workflow is active */
  isOrchestrated: boolean;
  /** Whether we're loading */
  isLoading: boolean;
  /** Error message if any */
  error: string | null;
  /** Refresh the state */
  refresh: () => Promise<void>;
}

/**
 * Hook to fetch and track orchestrator state for a task.
 *
 * @param taskId - The ID of the task to track, or null if no task
 * @param isRunning - Whether the task is currently running
 */
export function useOrchestratorState(
  taskId: string | null,
  isRunning: boolean,
): OrchestratorStateResult {
  const [state, setState] = useState<OrchestratorStateResponse | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const fetchState = useCallback(async () => {
    if (!taskId) {
      setState(null);
      return;
    }

    try {
      const response = await fetch(`${API_BASE}/task-runs/${taskId}/orchestrator-state`);
      if (!response.ok) {
        throw new Error(`Failed to fetch orchestrator state: ${response.statusText}`);
      }
      const data: OrchestratorStateResponse | null = await response.json();
      setState(data);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Unknown error");
    }
  }, [taskId]);

  // Initial fetch
  useEffect(() => {
    if (taskId) {
      setIsLoading(true);
      fetchState().finally(() => setIsLoading(false));
    } else {
      setState(null);
    }
  }, [taskId, fetchState]);

  // Poll while running
  useEffect(() => {
    if (!taskId || !isRunning) return;

    const interval = setInterval(fetchState, POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [taskId, isRunning, fetchState]);

  return {
    workflowStage: state?.workflow_stage ?? null,
    workflowStageDisplay: state?.workflow_stage_display ?? null,
    iteration: state?.iteration ?? 0,
    maxIterations: state?.max_iterations ?? 0,
    hasVerificationPlan: state?.has_verification_plan ?? false,
    isComplete: state?.is_complete ?? false,
    isPaused: state?.is_paused ?? false,
    isStopped: state?.is_stopped ?? false,
    isOrchestrated: state !== null,
    isLoading,
    error,
    refresh: fetchState,
  };
}

export default useOrchestratorState;
