/**
 * useStatistics
 *
 * TanStack Query hooks for the statistics dashboard.
 * Provides data fetching with caching, refetching, and error handling.
 */

import { useQuery } from "@tanstack/react-query";
import { statisticsService } from "../services/statistics-service";
import type {
  ConfigStatistics,
  FlakyItem,
  DebuggingContext,
  RunDetails,
  FlakinessSummary,
} from "../types/statistics";

// Keys for react-query cache
export const statisticsKeys = {
  all: ["statistics"] as const,
  config: (configId: string) => [...statisticsKeys.all, "config", configId] as const,
  flaky: (configId: string) => [...statisticsKeys.all, "flaky", configId] as const,
  runs: (configId: string) => [...statisticsKeys.all, "runs", configId] as const,
  runDetail: (runId: string) => [...statisticsKeys.all, "run", runId] as const,
  debugging: (configId: string) => [...statisticsKeys.all, "debugging", configId] as const,
};

/**
 * Hook to get config statistics
 */
export function useConfigStatistics(configId: string | null) {
  return useQuery({
    queryKey: statisticsKeys.config(configId ?? ""),
    queryFn: async (): Promise<ConfigStatistics | null> => {
      if (!configId) return null;
      const response = await statisticsService.getConfigStatistics(configId);
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to load statistics");
      }
      return response.data;
    },
    enabled: !!configId,
    staleTime: 30000, // 30 seconds
    refetchInterval: 60000, // Refetch every minute
  });
}

/**
 * Hook to get flaky items (combined transitions and templates)
 */
export function useFlakyItems(configId: string | null, threshold?: number) {
  return useQuery({
    queryKey: [...statisticsKeys.flaky(configId ?? ""), threshold],
    queryFn: async (): Promise<FlakyItem[]> => {
      if (!configId) return [];

      const [transitionsRes, templatesRes] = await Promise.all([
        statisticsService.getFlakyTransitions(configId, threshold),
        statisticsService.getFlakyTemplates(configId, threshold),
      ]);

      const items: FlakyItem[] = [];
      if (transitionsRes.success && transitionsRes.data) {
        items.push(...transitionsRes.data);
      }
      if (templatesRes.success && templatesRes.data) {
        items.push(...templatesRes.data);
      }

      // Sort by success rate (most problematic first)
      return items.sort((a, b) => a.success_rate - b.success_rate);
    },
    enabled: !!configId,
    staleTime: 30000,
  });
}

/**
 * Hook to get recent runs
 */
export function useRecentRuns(configId: string | null, limit = 20) {
  return useQuery({
    queryKey: [...statisticsKeys.runs(configId ?? ""), limit],
    queryFn: async (): Promise<RunDetails[]> => {
      if (!configId) return [];
      const response = await statisticsService.getRecentRuns(configId, limit);
      if (!response.success || !response.data) {
        return [];
      }
      return response.data;
    },
    enabled: !!configId,
    staleTime: 10000, // 10 seconds - runs change more frequently
  });
}

/**
 * Hook to get failed runs
 */
export function useFailedRuns(configId: string | null, limit = 5) {
  return useQuery({
    queryKey: [...statisticsKeys.runs(configId ?? ""), "failed", limit],
    queryFn: async (): Promise<RunDetails[]> => {
      if (!configId) return [];
      const response = await statisticsService.getFailedRuns(configId, limit);
      if (!response.success || !response.data) {
        return [];
      }
      return response.data;
    },
    enabled: !!configId,
    staleTime: 10000,
  });
}

/**
 * Hook to get single run details
 */
export function useRunDetails(runId: string | null) {
  return useQuery({
    queryKey: statisticsKeys.runDetail(runId ?? ""),
    queryFn: async (): Promise<RunDetails | null> => {
      if (!runId) return null;
      const response = await statisticsService.getRunDetails(runId);
      if (!response.success || !response.data) {
        return null;
      }
      return response.data;
    },
    enabled: !!runId,
    staleTime: 60000, // Run details don't change
  });
}

/**
 * Hook to get debugging context
 */
export function useDebuggingContext(configId: string | null, configName?: string) {
  return useQuery({
    queryKey: statisticsKeys.debugging(configId ?? ""),
    queryFn: async (): Promise<DebuggingContext | null> => {
      if (!configId) return null;
      const response = await statisticsService.getDebuggingContext(configId, configName);
      if (!response.success || !response.data) {
        return null;
      }
      return response.data;
    },
    enabled: !!configId,
    staleTime: 30000,
  });
}

/**
 * Hook to get flakiness summary (lightweight)
 */
export function useFlakinessSummary(configId: string | null) {
  return useQuery({
    queryKey: [...statisticsKeys.flaky(configId ?? ""), "summary"],
    queryFn: async (): Promise<FlakinessSummary | null> => {
      if (!configId) return null;
      const response = await statisticsService.getFlakinessSummary(configId);
      if (!response.success || !response.data) {
        return null;
      }
      return response.data;
    },
    enabled: !!configId,
    staleTime: 30000,
  });
}
