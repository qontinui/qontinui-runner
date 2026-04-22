/**
 * SharedStepDataContext
 *
 * Centralized data provider for the runner dashboard that fetches
 * /current-execution/steps once and shares it across all step widgets.
 * Eliminates duplicate API calls from independent widget polling.
 *
 * Uses a 3-second fallback polling interval combined with Tauri
 * "step-progress" event-driven refetches (debounced at 200ms).
 */

import React, {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { CurrentExecutionStepsResponse } from "@/components/widgets/shared/utils";
import type { StepStats } from "@/components/widgets/shared/types";
import type { WorkflowStage } from "@/types/dashboard/activity-types";
import { calculateStepStats, detectStartTime } from "@/components/widgets/shared/utils";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

// =============================================================================
// Constants
// =============================================================================

const POLL_INTERVAL_MS = 3000;
const DEBOUNCE_MS = 200;

// =============================================================================
// Types
// =============================================================================

interface SharedStepDataContextValue {
  data: CurrentExecutionStepsResponse | null;
  isLoading: boolean;
  error: string | null;
  lastFetchedAt: number | null;
  refetch: () => Promise<void>;
}

// =============================================================================
// Context
// =============================================================================

const SharedStepDataCtx = createContext<SharedStepDataContextValue | null>(null);

// =============================================================================
// Provider
// =============================================================================

interface SharedStepDataProviderProps {
  children: React.ReactNode;
}

export function SharedStepDataProvider({ children }: SharedStepDataProviderProps) {
  const [data, setData] = useState<CurrentExecutionStepsResponse | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [lastFetchedAt, setLastFetchedAt] = useState<number | null>(null);
  const mountedRef = useRef(true);
  const debounceTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Fetch all steps via batch endpoint (single round-trip for steps + metadata)
  const fetchSteps = useCallback(async () => {
    try {
      const response = await tracedFetch(`${getApiBase()}/current-execution/batch`);
      if (!response.ok) {
        if (mountedRef.current) {
          setError(`HTTP ${response.status}`);
        }
        return;
      }
      const json: CurrentExecutionStepsResponse = await response.json();
      if (mountedRef.current) {
        setData(json);
        setError(null);
        setLastFetchedAt(Date.now());
      }
    } catch {
      // Silently ignore - API may not be available yet
    } finally {
      if (mountedRef.current) {
        setIsLoading(false);
      }
    }
  }, []);

  // Debounced refetch triggered by Tauri events
  const debouncedRefetch = useCallback(() => {
    if (debounceTimerRef.current) {
      clearTimeout(debounceTimerRef.current);
    }
    debounceTimerRef.current = setTimeout(() => {
      fetchSteps();
    }, DEBOUNCE_MS);
  }, [fetchSteps]);

  // Initial fetch + fallback polling
  useEffect(() => {
    mountedRef.current = true;

    fetchSteps();
    const interval = setInterval(fetchSteps, POLL_INTERVAL_MS);
    return () => {
      mountedRef.current = false;
      clearInterval(interval);
      if (debounceTimerRef.current) {
        clearTimeout(debounceTimerRef.current);
      }
    };
  }, [fetchSteps]);

  // Listen for Tauri "step-progress" events to trigger immediate refetch
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;

    const setup = async () => {
      try {
        unlisten = await listen("step-progress", () => {
          debouncedRefetch();
        });
      } catch {
        // Tauri events may not be available in all environments
      }
    };

    setup();

    return () => {
      if (unlisten) {
        unlisten();
      }
    };
  }, [debouncedRefetch]);

  const value = useMemo<SharedStepDataContextValue>(
    () => ({
      data,
      isLoading,
      error,
      lastFetchedAt,
      refetch: fetchSteps,
    }),
    [data, isLoading, error, lastFetchedAt, fetchSteps],
  );

  return <SharedStepDataCtx.Provider value={value}>{children}</SharedStepDataCtx.Provider>;
}

// =============================================================================
// Consumer Hooks
// =============================================================================

/**
 * Raw context accessor. Throws if used outside the provider.
 */
function useSharedStepDataContext(): SharedStepDataContextValue {
  const ctx = useContext(SharedStepDataCtx);
  if (!ctx) {
    throw new Error("useSharedStepData must be used within SharedStepDataProvider");
  }
  return ctx;
}

/**
 * Consumer hook with optional client-side filtering by step_type.
 *
 * Returns the executions (optionally filtered), pre-computed stats,
 * start time, and metadata fields so widgets do not need to duplicate
 * this logic.
 */
export function useSharedStepData(filter?: { stepType?: string }): {
  executions: CurrentExecutionStepsResponse["executions"];
  stats: StepStats;
  startTime: number | null;
  isLoading: boolean;
  error: string | null;
  currentStage: WorkflowStage | null;
  workflowName: string | null;
  taskRunId: string | null;
  lastFetchedAt: number | null;
} {
  const { data, isLoading, error, lastFetchedAt } = useSharedStepDataContext();
  const stepType = filter?.stepType;

  return useMemo(() => {
    if (!data || !data.success) {
      return {
        executions: [],
        stats: calculateStepStats([], 0),
        startTime: null,
        isLoading,
        error,
        currentStage: null,
        workflowName: null,
        taskRunId: null,
        lastFetchedAt,
      };
    }

    // Apply client-side filter if provided
    const executions = stepType
      ? data.executions.filter((exec) => exec.step_type === stepType)
      : data.executions;

    const startTime = detectStartTime(executions);
    const stats = calculateStepStats(executions, 0);

    return {
      executions,
      stats,
      startTime,
      isLoading,
      error,
      currentStage: data.current_stage ?? null,
      workflowName: data.workflow_name ?? null,
      taskRunId: data.task_run_id,
      lastFetchedAt,
    };
  }, [data, isLoading, error, lastFetchedAt, stepType]);
}

/**
 * Low-level hook that returns the full unfiltered response plus metadata.
 * Useful for complex consumers (e.g., execution timeline) that need
 * access to all executions and the raw response fields.
 */
export function useSharedStepDataRaw(): SharedStepDataContextValue {
  return useSharedStepDataContext();
}
