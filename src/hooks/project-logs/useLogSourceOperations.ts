/**
 * useLogSourceOperations
 *
 * Hook for selecting/deselecting global log sources for a project.
 * No CRUD — sources are managed globally in Settings > Log Sources.
 */

import { useCallback } from "react";
import type { ProjectLogConfig, UseLogSourceOperationsReturn } from "./types";

interface UseLogSourceOperationsProps {
  setConfig: React.Dispatch<React.SetStateAction<ProjectLogConfig | null>>;
  saveConfig: () => Promise<void>;
}

/**
 * Hook for managing which global log sources are selected for a project.
 */
export function useLogSourceOperations({
  setConfig,
  saveConfig,
}: UseLogSourceOperationsProps): UseLogSourceOperationsReturn {
  /**
   * Set the selected global source IDs for this project
   */
  const setSelectedSources = useCallback(
    (sourceIds: string[]) => {
      setConfig((prev) => {
        if (!prev) return prev;
        return {
          ...prev,
          selectedSourceIds: sourceIds,
          // Clear profile when explicitly selecting sources
          globalProfileId: undefined,
        };
      });
      // Auto-save after update
      setTimeout(() => saveConfig(), 0);
    },
    [setConfig, saveConfig],
  );

  /**
   * Set the global profile ID for this project
   */
  const setGlobalProfile = useCallback(
    (profileId: string | undefined) => {
      setConfig((prev) => {
        if (!prev) return prev;
        return {
          ...prev,
          globalProfileId: profileId,
          // Clear explicit selections when using a profile
          selectedSourceIds: [],
        };
      });
      // Auto-save after update
      setTimeout(() => saveConfig(), 0);
    },
    [setConfig, saveConfig],
  );

  return {
    setSelectedSources,
    setGlobalProfile,
  };
}
