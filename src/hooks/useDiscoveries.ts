/**
 * React hooks for discovery sync data.
 * Uses TanStack Query for data fetching with caching and refetching.
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { discoveriesService } from "../services/discoveries-service";
import type {
  DiscoverySummary,
  SyncStatus,
  PendingDiscovery,
  SyncResult,
} from "../types/discoveries";

// Query keys for caching
export const discoveryKeys = {
  all: ["discoveries"] as const,
  summary: () => [...discoveryKeys.all, "summary"] as const,
  status: () => [...discoveryKeys.all, "status"] as const,
  pending: () => [...discoveryKeys.all, "pending"] as const,
};

/**
 * Hook to get discovery summary for dashboard display.
 */
export function useDiscoverySummary() {
  return useQuery({
    queryKey: discoveryKeys.summary(),
    queryFn: async (): Promise<DiscoverySummary | null> => {
      const response = await discoveriesService.getSummary();
      if (!response.success || !response.data) {
        return null;
      }
      return response.data;
    },
    staleTime: 30000, // 30 seconds
    refetchInterval: 60000, // Refetch every minute
  });
}

/**
 * Hook to get current sync status.
 */
export function useSyncStatus() {
  return useQuery({
    queryKey: discoveryKeys.status(),
    queryFn: async (): Promise<SyncStatus | null> => {
      const response = await discoveriesService.getSyncStatus();
      if (!response.success || !response.data) {
        return null;
      }
      return response.data;
    },
    staleTime: 10000, // 10 seconds
    refetchInterval: 30000, // Refetch every 30 seconds
  });
}

/**
 * Hook to get all pending discoveries.
 */
export function usePendingDiscoveries() {
  return useQuery({
    queryKey: discoveryKeys.pending(),
    queryFn: async (): Promise<PendingDiscovery[]> => {
      const response = await discoveriesService.getPendingDiscoveries();
      if (!response.success || !response.data) {
        return [];
      }
      return response.data;
    },
    staleTime: 30000,
  });
}

/**
 * Hook for syncing discoveries.
 */
export function useSyncDiscoveries() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (): Promise<SyncResult> => {
      const response = await discoveriesService.syncDiscoveries();
      if (!response.success || !response.data) {
        throw new Error(response.error || "Sync failed");
      }
      return response.data;
    },
    onSuccess: () => {
      // Invalidate all discovery queries to refresh data
      queryClient.invalidateQueries({ queryKey: discoveryKeys.all });
    },
  });
}

/**
 * Hook for clearing a single discovery.
 */
export function useClearDiscovery() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (id: string): Promise<boolean> => {
      const response = await discoveriesService.clearDiscovery(id);
      if (!response.success) {
        throw new Error(response.error || "Failed to clear discovery");
      }
      return response.data ?? false;
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: discoveryKeys.all });
    },
  });
}

/**
 * Hook for clearing all failed discoveries.
 */
export function useClearFailedDiscoveries() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (): Promise<number> => {
      const response = await discoveriesService.clearFailedDiscoveries();
      if (!response.success) {
        throw new Error(response.error || "Failed to clear failed discoveries");
      }
      return response.data ?? 0;
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: discoveryKeys.all });
    },
  });
}
