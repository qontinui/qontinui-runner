/**
 * RunSelectionContext
 *
 * Provides shared run selection state across all run-specific pages in the Observe menu.
 * Uses TaskRuns (AI sessions) from the task_runs table, not automation runs.
 */

import {
  createContext,
  useContext,
  ReactNode,
  useState,
  useEffect,
  useCallback,
  useMemo,
  useRef,
} from "react";
import { useExecution } from "./ExecutionContext";
import { useTaskRuns, useTaskRun } from "../hooks/useAiData";
import type { TaskRun } from "../types/aiData";

// Store context in window to survive HMR
declare global {
  interface Window {
    __RUN_SELECTION_CONTEXT__?: React.Context<RunSelectionContextValue | null>;
  }
}

const STORAGE_KEY = "qontinui-selected-task-run-id";

interface RunSelectionContextValue {
  /** Currently selected run ID */
  selectedRunId: string | null;
  /** Full details of the selected run */
  selectedRun: TaskRun | null;
  /** Set the selected run by ID */
  setSelectedRunId: (id: string | null) => void;
  /** List of recent task runs */
  recentRuns: TaskRun[];
  /** Whether runs are being loaded */
  isLoadingRuns: boolean;
  /** Whether the selected run details are being loaded */
  isLoadingDetails: boolean;
  /** Config ID from ExecutionContext (for reference) */
  configId: string | null;
  /** Clear selection */
  clearSelection: () => void;
  /** Select the most recent run */
  selectMostRecent: () => void;
  /** Whether a run is currently in progress */
  hasRunInProgress: boolean;
}

// Create context once and store in window to survive HMR reloads
const RunSelectionContext: React.Context<RunSelectionContextValue | null> =
  window.__RUN_SELECTION_CONTEXT__ ||
  (window.__RUN_SELECTION_CONTEXT__ = createContext<RunSelectionContextValue | null>(null));

interface RunSelectionProviderProps {
  children: ReactNode;
}

/**
 * RunSelectionProvider - Manages run selection state for Observe pages
 */
export function RunSelectionProvider({ children }: RunSelectionProviderProps) {
  const { config, executionActive } = useExecution();

  // Get configId from loaded config (for reference, not used for filtering)
  const configId = config?.name && config.name.length > 0 ? config.name : (config?.path ?? null);

  // Load saved selection from localStorage (global, not config-scoped)
  const [selectedRunId, setSelectedRunIdState] = useState<string | null>(() => {
    try {
      const stored = localStorage.getItem(STORAGE_KEY);
      return stored ? JSON.parse(stored) : null;
    } catch {
      return null;
    }
  });

  // Fetch recent task runs (not filtered by config)
  const { data: recentRuns = [], isLoading: isLoadingRuns } = useTaskRuns(50);

  // Fetch details for the selected task run
  const { data: selectedRun, isLoading: isLoadingDetails } = useTaskRun(selectedRunId);

  // Check if there's a run in progress
  const hasRunInProgress = useMemo(() => {
    return recentRuns.some((run) => run.status === "running") || executionActive;
  }, [recentRuns, executionActive]);

  // Track previously known run IDs to detect new runs
  const prevRunIdsRef = useRef<Set<string>>(new Set());

  // Auto-select new running runs when execution starts
  useEffect(() => {
    if (recentRuns.length === 0) return;

    const currentIds = new Set(recentRuns.map((r) => r.id));
    const runningRun = recentRuns.find((r) => r.status === "running");

    // If there's a running run that wasn't in previous list, auto-select it
    if (runningRun && !prevRunIdsRef.current.has(runningRun.id)) {
      setSelectedRunIdState(runningRun.id);
      // Also persist to localStorage
      try {
        localStorage.setItem(STORAGE_KEY, JSON.stringify(runningRun.id));
      } catch {
        // Ignore storage errors
      }
    }

    // Update the ref with current IDs
    prevRunIdsRef.current = currentIds;
  }, [recentRuns]);

  // Persist selection to localStorage
  const setSelectedRunId = useCallback((id: string | null) => {
    setSelectedRunIdState(id);
    try {
      if (id) {
        localStorage.setItem(STORAGE_KEY, JSON.stringify(id));
      } else {
        localStorage.removeItem(STORAGE_KEY);
      }
    } catch {
      // Ignore storage errors
    }
  }, []);

  // Clear selection
  const clearSelection = useCallback(() => {
    setSelectedRunId(null);
  }, [setSelectedRunId]);

  // Select the most recent run
  const selectMostRecent = useCallback(() => {
    if (recentRuns.length > 0) {
      setSelectedRunId(recentRuns[0].id);
    }
  }, [recentRuns, setSelectedRunId]);

  // Auto-select most recent if no selection and runs available
  useEffect(() => {
    if (!selectedRunId && recentRuns.length > 0 && !isLoadingRuns) {
      setSelectedRunIdState(recentRuns[0].id);
    }
  }, [recentRuns, isLoadingRuns, selectedRunId]);

  // Validate selection when runs are loaded
  useEffect(() => {
    if (selectedRunId && recentRuns.length > 0 && !isLoadingRuns) {
      const runExists = recentRuns.some((run) => run.id === selectedRunId);
      if (!runExists) {
        // Selected run no longer exists, select most recent
        setSelectedRunId(recentRuns[0].id);
      }
    }
  }, [selectedRunId, recentRuns, isLoadingRuns, setSelectedRunId]);

  const value: RunSelectionContextValue = {
    selectedRunId,
    selectedRun: selectedRun ?? null,
    setSelectedRunId,
    recentRuns,
    isLoadingRuns,
    isLoadingDetails,
    configId,
    clearSelection,
    selectMostRecent,
    hasRunInProgress,
  };

  return <RunSelectionContext.Provider value={value}>{children}</RunSelectionContext.Provider>;
}

/**
 * Hook to access run selection context
 */
export function useRunSelection() {
  const context = useContext(RunSelectionContext);
  if (!context) {
    throw new Error("useRunSelection must be used within RunSelectionProvider");
  }
  return context;
}

/**
 * Hook to check if run selection context is available
 * Useful for components that may render outside the provider
 */
export function useRunSelectionOptional() {
  return useContext(RunSelectionContext);
}
