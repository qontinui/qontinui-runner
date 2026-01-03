/**
 * RunSelectionContext
 *
 * Provides shared run selection state across all run-specific pages in the Observe menu.
 * Uses the config from ExecutionContext to fetch runs and maintains selection state.
 */

import {
  createContext,
  useContext,
  ReactNode,
  useState,
  useEffect,
  useCallback,
  useMemo,
} from "react";
import { useExecution } from "./ExecutionContext";
import { useRecentRuns, useRunDetails } from "../hooks/useStatistics";
import type { RunDetails } from "../types/statistics";

// Store context in window to survive HMR
declare global {
  interface Window {
    __RUN_SELECTION_CONTEXT__?: React.Context<RunSelectionContextValue | null>;
  }
}

const STORAGE_KEY = "qontinui-selected-run-id";

interface RunSelectionContextValue {
  /** Currently selected run ID */
  selectedRunId: string | null;
  /** Full details of the selected run */
  selectedRun: RunDetails | null;
  /** Set the selected run by ID */
  setSelectedRunId: (id: string | null) => void;
  /** List of recent runs for the current config */
  recentRuns: RunDetails[];
  /** Whether runs are being loaded */
  isLoadingRuns: boolean;
  /** Whether the selected run details are being loaded */
  isLoadingDetails: boolean;
  /** Config ID from ExecutionContext */
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

  // Get configId from loaded config - use same logic as Rust backend:
  // config.metadata.name if not empty, otherwise path
  const configId = config?.name && config.name.length > 0 ? config.name : (config?.path ?? null);

  // Load saved selection from localStorage (scoped to config)
  const [selectedRunId, setSelectedRunIdState] = useState<string | null>(() => {
    if (!configId) return null;
    try {
      const stored = localStorage.getItem(`${STORAGE_KEY}-${configId}`);
      return stored ? JSON.parse(stored) : null;
    } catch {
      return null;
    }
  });

  // Fetch recent runs for the config
  const { data: recentRuns = [], isLoading: isLoadingRuns } = useRecentRuns(configId, 50);

  // Fetch details for the selected run
  const { data: selectedRun, isLoading: isLoadingDetails } = useRunDetails(selectedRunId);

  // Check if there's a run in progress
  const hasRunInProgress = useMemo(() => {
    return recentRuns.some((run) => run.status === "running") || executionActive;
  }, [recentRuns, executionActive]);

  // Persist selection to localStorage
  const setSelectedRunId = useCallback(
    (id: string | null) => {
      setSelectedRunIdState(id);
      if (configId) {
        try {
          if (id) {
            localStorage.setItem(`${STORAGE_KEY}-${configId}`, JSON.stringify(id));
          } else {
            localStorage.removeItem(`${STORAGE_KEY}-${configId}`);
          }
        } catch {
          // Ignore storage errors
        }
      }
    },
    [configId],
  );

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

  // When config changes, try to restore saved selection or auto-select most recent
  useEffect(() => {
    if (!configId) {
      setSelectedRunIdState(null);
      return;
    }

    // Try to restore saved selection
    try {
      const stored = localStorage.getItem(`${STORAGE_KEY}-${configId}`);
      if (stored) {
        const savedId = JSON.parse(stored);
        // Verify the saved run still exists in recent runs (once loaded)
        if (recentRuns.length > 0) {
          const runExists = recentRuns.some((run) => run.id === savedId);
          if (runExists) {
            setSelectedRunIdState(savedId);
            return;
          }
        } else if (!isLoadingRuns) {
          // Runs loaded but empty, clear invalid selection
          setSelectedRunIdState(null);
          return;
        }
        // Keep the saved ID while runs are loading
        setSelectedRunIdState(savedId);
        return;
      }
    } catch {
      // Ignore parse errors
    }

    // No saved selection - auto-select most recent if available
    if (recentRuns.length > 0 && !selectedRunId) {
      setSelectedRunIdState(recentRuns[0].id);
    }
  }, [configId, recentRuns, isLoadingRuns, selectedRunId]);

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
