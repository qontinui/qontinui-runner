/**
 * useLogSourceOperations
 *
 * Hook for CRUD operations on log sources.
 * Manages adding, updating, removing, and toggling log sources.
 */

import { useCallback } from "react";
import { createLogSource } from "../../types/projectLogs";
import type { LogSource, ProjectLogConfig, UseLogSourceOperationsReturn } from "./types";

interface UseLogSourceOperationsProps {
  setConfig: React.Dispatch<React.SetStateAction<ProjectLogConfig | null>>;
  markUnsavedChanges: () => void;
}

/**
 * Hook for managing log source CRUD operations.
 */
export function useLogSourceOperations({
  setConfig,
  markUnsavedChanges,
}: UseLogSourceOperationsProps): UseLogSourceOperationsReturn {
  /**
   * Add a new log source
   */
  const addLogSource = useCallback(
    (partial?: Partial<LogSource>) => {
      setConfig((prev) => {
        if (!prev) return prev;
        const newSource = createLogSource(partial);
        markUnsavedChanges();
        return {
          ...prev,
          logSources: [...prev.logSources, newSource],
        };
      });
    },
    [setConfig, markUnsavedChanges],
  );

  /**
   * Update an existing log source
   */
  const updateLogSource = useCallback(
    (id: string, updates: Partial<LogSource>) => {
      setConfig((prev) => {
        if (!prev) return prev;
        markUnsavedChanges();
        return {
          ...prev,
          logSources: prev.logSources.map((s) => (s.id === id ? { ...s, ...updates } : s)),
        };
      });
    },
    [setConfig, markUnsavedChanges],
  );

  /**
   * Remove a log source
   */
  const removeLogSource = useCallback(
    (id: string) => {
      setConfig((prev) => {
        if (!prev) return prev;
        markUnsavedChanges();
        return {
          ...prev,
          logSources: prev.logSources.filter((s) => s.id !== id),
        };
      });
    },
    [setConfig, markUnsavedChanges],
  );

  /**
   * Toggle a log source enabled/disabled
   */
  const toggleLogSource = useCallback(
    (id: string) => {
      setConfig((prev) => {
        if (!prev) return prev;
        markUnsavedChanges();
        return {
          ...prev,
          logSources: prev.logSources.map((s) => (s.id === id ? { ...s, enabled: !s.enabled } : s)),
        };
      });
    },
    [setConfig, markUnsavedChanges],
  );

  /**
   * Set all log sources at once
   */
  const setLogSources = useCallback(
    (sources: LogSource[]) => {
      setConfig((prev) => {
        if (!prev) return prev;
        markUnsavedChanges();
        return {
          ...prev,
          logSources: sources,
        };
      });
    },
    [setConfig, markUnsavedChanges],
  );

  return {
    addLogSource,
    updateLogSource,
    removeLogSource,
    toggleLogSource,
    setLogSources,
  };
}
