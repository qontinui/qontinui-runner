/**
 * useTaskRunRecap
 *
 * Hook for fetching recap data for a task run.
 * Used by the Session Recap page to display an overview of what happened.
 */

import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { RecapData, RecapResponse } from "../types/recap";

/**
 * Query key factory for recap queries.
 */
export const recapKeys = {
  all: ["recap"] as const,
  detail: (taskRunId: string) => [...recapKeys.all, taskRunId] as const,
};

/**
 * Fetch recap data for a task run.
 */
async function fetchTaskRunRecap(taskRunId: string): Promise<RecapData> {
  const response = await invoke<RecapResponse>("get_task_run_recap", {
    taskRunId,
  });

  if (!response.success || !response.data) {
    throw new Error(response.error || "Failed to fetch recap data");
  }

  return response.data;
}

/**
 * Hook to fetch and cache recap data for a task run.
 *
 * @param taskRunId - The ID of the task run to get recap for
 * @returns Query result with recap data
 */
export function useTaskRunRecap(taskRunId: string | null | undefined) {
  return useQuery({
    queryKey: recapKeys.detail(taskRunId ?? ""),
    queryFn: () => fetchTaskRunRecap(taskRunId!),
    enabled: !!taskRunId,
    staleTime: 30_000, // Consider data fresh for 30 seconds
    refetchInterval: (query) => {
      // Auto-refresh every 5 seconds if the run is still in progress
      const data = query.state.data;
      if (data?.status === "running") {
        return 5_000;
      }
      return false;
    },
  });
}

export default useTaskRunRecap;
