/**
 * useOrchestratorState Hook
 *
 * @deprecated Use useWorkflowOrchestratorState() from useWorkflowExecution.ts instead.
 * This hook now delegates to the unified WorkflowExecutionContext for consistency.
 *
 * Fetches and tracks the orchestrator state for a running task.
 * Provides the current workflow stage (Setup, Agentic, Verification, Completion).
 *
 * Uses real-time events (Tauri or WebSocket) for instant updates with polling as fallback.
 * Automatically detects the environment and uses:
 * - Tauri events when running in Tauri desktop app
 * - WebSocket events when running in browser or external client
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { WorkflowStage } from "../../types/dashboard/activity-types";
import {
  useWebSocketEvents,
  isTauriEnvironment,
  type OrchestratorStateChangePayload,
} from "../useWebSocketEvents";
import { useWorkflowExecutionOptional } from "../../contexts/WorkflowExecutionContext";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { createLogger } from "@/lib/logger";

const logger = createLogger("OrchestratorState");

// Polling is now fallback only - real-time events provide instant updates
const POLL_INTERVAL_MS = 5000;

/**
 * Event payload from backend orchestrator-state-change events.
 * Re-exported from useWebSocketEvents for type compatibility.
 */
type OrchestratorStateChangeEvent = OrchestratorStateChangePayload;

/**
 * Response from the orchestrator state API endpoint.
 */
export interface OrchestratorStateResponse {
  task_run_id: string;
  workflow_type: string;
  current_state: string;
  phase: string | null;
  iteration: number | null;
  max_iterations: number | null;
  is_complete: boolean;
  is_stopped: boolean;
  is_paused: boolean;
  has_verification_plan: boolean;
  workflow_start_time: string;
  state_data: unknown;
  // Mapped workflow stage for UI
  workflow_stage: WorkflowStage | null;
  workflow_stage_display: string | null;
  // Plan-specific fields
  plan_phase_name?: string | null;
  plan_phase_index?: number | null;
  plan_total_phases?: number | null;
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
  /** Plan phase name (only for plan workflows) */
  planPhaseName: string | null;
  /** Plan phase index, zero-based (only for plan workflows) */
  planPhaseIndex: number | null;
  /** Total number of plan phases (only for plan workflows) */
  planTotalPhases: number | null;
  /** Current stage index, zero-based (for multi-stage workflows) */
  currentStageIndex: number | null;
  /** Current stage name (for multi-stage workflows) */
  currentStageName: string | null;
  /** Total number of stages (for multi-stage workflows) */
  totalStages: number | null;
}

/**
 * Hook to fetch and track orchestrator state for a task.
 *
 * @deprecated Use useWorkflowOrchestratorState() from useWorkflowExecution.ts instead.
 * This hook now attempts to use the unified WorkflowExecutionContext when available,
 * falling back to direct API calls when outside the provider.
 *
 * Uses real-time Tauri events for instant updates with polling as fallback.
 *
 * @param taskId - The ID of the task to track, or null if no task
 * @param isRunning - Whether the task is currently running
 */
export function useOrchestratorState(
  taskId: string | null,
  isRunning: boolean,
): OrchestratorStateResult {
  // Try to use unified context if available
  const unifiedContext = useWorkflowExecutionOptional();

  // Check if we should use the unified context
  const useUnifiedContext = unifiedContext && unifiedContext.taskRunId === taskId;

  // ALL hooks must be called unconditionally (React rules of hooks)
  // These are used for the legacy fallback implementation
  const [state, setState] = useState<OrchestratorStateResponse | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const unlistenRef = useRef<UnlistenFn | null>(null);
  const isTauri = isTauriEnvironment();

  const fetchState = useCallback(async () => {
    if (!taskId) {
      setState(null);
      return;
    }

    try {
      const response = await tracedFetch(`${getApiBase()}/task-runs/${taskId}/orchestrator-state`);
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

  // Handle WebSocket events (when not in Tauri)
  const handleWsOrchestratorChange = useCallback(
    (payload: OrchestratorStateChangePayload) => {
      if (payload.data?.task_run_id === taskId) {
        // Trigger full fetch to get complete state
        fetchState();
      }
    },
    [taskId, fetchState],
  );

  // Subscribe to WebSocket events for non-Tauri environments
  // Skip when using unified context (it has its own event handling)
  useWebSocketEvents({
    enabled: !useUnifiedContext && !isTauri && isRunning && !!taskId,
    onOrchestratorStateChange: handleWsOrchestratorChange,
  });

  // Subscribe to Tauri events for Tauri environments
  // Skip when using unified context (it has its own event handling)
  useEffect(() => {
    if (useUnifiedContext || !isTauri || !taskId || !isRunning) return;

    let mounted = true;

    const setupListener = async () => {
      try {
        const unlisten = await listen<OrchestratorStateChangeEvent>(
          "orchestrator-state-change",
          (event) => {
            if (!mounted) return;

            const payload = event.payload;
            // Only update if this event is for our task
            if (payload.data?.task_run_id === taskId) {
              // Update state with event data - trigger a full fetch for complete data
              // The event gives us quick notification, fetch gives us full state
              fetchState();
            }
          },
        );
        unlistenRef.current = unlisten;
      } catch (e) {
        // Tauri events not available - rely on WebSocket/polling
        logger.debug("Tauri events not available:", e);
      }
    };

    setupListener();

    return () => {
      mounted = false;
      if (unlistenRef.current) {
        unlistenRef.current();
        unlistenRef.current = null;
      }
    };
  }, [useUnifiedContext, isTauri, taskId, isRunning, fetchState]);

  // Initial fetch - skip when using unified context
  useEffect(() => {
    if (useUnifiedContext) return;
    if (taskId) {
      setIsLoading(true);
      fetchState().finally(() => setIsLoading(false));
    } else {
      setState(null);
    }
  }, [useUnifiedContext, taskId, fetchState]);

  // Fallback polling while running (reduced frequency since we have real-time events)
  // Skip when using unified context
  useEffect(() => {
    if (useUnifiedContext || !taskId || !isRunning) return;

    const interval = setInterval(fetchState, POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [useUnifiedContext, taskId, isRunning, fetchState]);

  // If using unified context, return its values
  if (useUnifiedContext && unifiedContext) {
    return {
      workflowStage: unifiedContext.workflowStage,
      workflowStageDisplay: unifiedContext.workflowStageDisplay,
      iteration: unifiedContext.iteration,
      maxIterations: unifiedContext.maxIterations,
      hasVerificationPlan: unifiedContext.hasVerificationPlan,
      isComplete: unifiedContext.isComplete,
      isPaused: unifiedContext.isPaused,
      isStopped: unifiedContext.isStopped,
      isOrchestrated: unifiedContext.isOrchestrated,
      isLoading: unifiedContext.isLoading,
      error: unifiedContext.error,
      refresh: unifiedContext.refresh,
      planPhaseName: unifiedContext.planPhaseName ?? null,
      planPhaseIndex: unifiedContext.planPhaseIndex ?? null,
      planTotalPhases: unifiedContext.planTotalPhases ?? null,
      currentStageIndex: unifiedContext.currentStageIndex ?? null,
      currentStageName: unifiedContext.currentStageName ?? null,
      totalStages: unifiedContext.totalStages ?? null,
    };
  }

  // Return legacy implementation values
  return {
    workflowStage: state?.workflow_stage ?? null,
    workflowStageDisplay: state?.workflow_stage_display ?? null,
    iteration: state?.iteration ?? 1,
    maxIterations: state?.max_iterations ?? 10,
    hasVerificationPlan: state?.has_verification_plan ?? false,
    isComplete: state?.is_complete ?? false,
    isPaused: state?.is_paused ?? false,
    isStopped: state?.is_stopped ?? false,
    // Consider orchestrated if we have a valid response with workflow stage
    isOrchestrated:
      state !== null && (state.workflow_type === "unified" || state.workflow_type === "plan"),
    isLoading,
    error,
    refresh: fetchState,
    planPhaseName: state?.plan_phase_name ?? null,
    planPhaseIndex: state?.plan_phase_index ?? null,
    planTotalPhases: state?.plan_total_phases ?? null,
    currentStageIndex: null,
    currentStageName: null,
    totalStages: null,
  };
}

export default useOrchestratorState;
