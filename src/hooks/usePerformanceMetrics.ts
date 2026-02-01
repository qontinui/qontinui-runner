/**
 * usePerformanceMetrics
 *
 * TanStack Query hooks for the Performance Metrics Dashboard.
 * Provides data fetching with caching, refetching, and error handling.
 */

import { useQuery } from "@tanstack/react-query";
import { performanceMetricsService } from "../services/performance-metrics-service";
import type {
  PerformanceDashboardData,
  ActionPerformanceMetrics,
  StateTransitionMetrics,
  ElementResolutionMetrics,
  TimeSeriesDataPoint,
  TimeRange,
} from "../types/performance-metrics";

// Keys for react-query cache
export const performanceMetricsKeys = {
  all: ["performance-metrics"] as const,
  dashboard: (configId: string, timeRange: TimeRange) =>
    [...performanceMetricsKeys.all, "dashboard", configId, timeRange] as const,
  actions: (configId: string, timeRange?: TimeRange) =>
    [...performanceMetricsKeys.all, "actions", configId, timeRange] as const,
  transitions: (configId: string) =>
    [...performanceMetricsKeys.all, "transitions", configId] as const,
  elements: (configId: string) =>
    [...performanceMetricsKeys.all, "elements", configId] as const,
  successTrend: (configId: string, timeRange?: TimeRange) =>
    [...performanceMetricsKeys.all, "success-trend", configId, timeRange] as const,
};

/**
 * Hook to get complete performance dashboard data.
 */
export function usePerformanceDashboard(configId: string | null, timeRange: TimeRange = "7d") {
  return useQuery({
    queryKey: performanceMetricsKeys.dashboard(configId ?? "", timeRange),
    queryFn: async (): Promise<PerformanceDashboardData | null> => {
      if (!configId) return null;
      const response = await performanceMetricsService.getPerformanceDashboard(configId, timeRange);
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to load performance dashboard");
      }
      return response.data;
    },
    enabled: !!configId,
    staleTime: 30000, // 30 seconds
    refetchInterval: 60000, // Refetch every minute
  });
}

/**
 * Hook to get action performance metrics.
 */
export function useActionPerformance(configId: string | null, timeRange?: TimeRange) {
  return useQuery({
    queryKey: performanceMetricsKeys.actions(configId ?? "", timeRange),
    queryFn: async (): Promise<ActionPerformanceMetrics[]> => {
      if (!configId) return [];
      const response = await performanceMetricsService.getActionPerformance(configId, timeRange);
      if (!response.success || !response.data) {
        return [];
      }
      return response.data;
    },
    enabled: !!configId,
    staleTime: 30000,
  });
}

/**
 * Hook to get transition reliability metrics.
 */
export function useTransitionReliability(configId: string | null) {
  return useQuery({
    queryKey: performanceMetricsKeys.transitions(configId ?? ""),
    queryFn: async (): Promise<StateTransitionMetrics[]> => {
      if (!configId) return [];
      const response = await performanceMetricsService.getTransitionReliability(configId);
      if (!response.success || !response.data) {
        return [];
      }
      return response.data;
    },
    enabled: !!configId,
    staleTime: 30000,
  });
}

/**
 * Hook to get element resolution metrics.
 */
export function useElementResolution(configId: string | null) {
  return useQuery({
    queryKey: performanceMetricsKeys.elements(configId ?? ""),
    queryFn: async (): Promise<ElementResolutionMetrics[]> => {
      if (!configId) return [];
      const response = await performanceMetricsService.getElementResolutionMetrics(configId);
      if (!response.success || !response.data) {
        return [];
      }
      return response.data;
    },
    enabled: !!configId,
    staleTime: 30000,
  });
}

/**
 * Hook to get success rate trend.
 */
export function useSuccessRateTrend(configId: string | null, timeRange?: TimeRange) {
  return useQuery({
    queryKey: performanceMetricsKeys.successTrend(configId ?? "", timeRange),
    queryFn: async (): Promise<TimeSeriesDataPoint[]> => {
      if (!configId) return [];
      const response = await performanceMetricsService.getSuccessRateTrend(configId, timeRange);
      if (!response.success || !response.data) {
        return [];
      }
      return response.data;
    },
    enabled: !!configId,
    staleTime: 30000,
  });
}
