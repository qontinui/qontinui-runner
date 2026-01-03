/**
 * useAiData
 *
 * TanStack Query hooks for the AI Data Viewer.
 * Provides data fetching with caching, refetching, and error handling.
 */

import { useQuery } from "@tanstack/react-query";
import { aiDataService } from "../services/ai-data-service";
import type { TaskRun, JsonlLogsResult, JsonlLogsSummary, JsonlLogType } from "../types/aiData";
import type { RunDetails } from "../types/statistics";

// Keys for react-query cache
export const aiDataKeys = {
  all: ["aiData"] as const,
  taskRuns: () => [...aiDataKeys.all, "taskRuns"] as const,
  taskRun: (taskId: string) => [...aiDataKeys.all, "taskRun", taskId] as const,
  automationRuns: (configId: string) => [...aiDataKeys.all, "automationRuns", configId] as const,
  automationRun: (runId: string) => [...aiDataKeys.all, "automationRun", runId] as const,
  jsonlSummary: () => [...aiDataKeys.all, "jsonlSummary"] as const,
  jsonlLogs: (logType: JsonlLogType) => [...aiDataKeys.all, "jsonlLogs", logType] as const,
};

/**
 * Hook to get recent task runs
 */
export function useTaskRuns(limit?: number) {
  return useQuery({
    queryKey: [...aiDataKeys.taskRuns(), limit],
    queryFn: async (): Promise<TaskRun[]> => {
      const response = await aiDataService.getTaskRuns(limit);
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to load task runs");
      }
      return response.data;
    },
    staleTime: 10000, // 10 seconds
    refetchInterval: 30000, // Refetch every 30 seconds
  });
}

/**
 * Hook to get a specific task run
 */
export function useTaskRun(taskId: string | null) {
  return useQuery({
    queryKey: aiDataKeys.taskRun(taskId ?? ""),
    queryFn: async (): Promise<TaskRun | null> => {
      if (!taskId) return null;
      const response = await aiDataService.getTaskRun(taskId);
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to load task run");
      }
      return response.data;
    },
    enabled: !!taskId,
    staleTime: 10000,
  });
}

/**
 * Hook to get automation runs for a config
 */
export function useAutomationRuns(configId: string | null, limit?: number) {
  return useQuery({
    queryKey: [...aiDataKeys.automationRuns(configId ?? ""), limit],
    queryFn: async (): Promise<RunDetails[]> => {
      if (!configId) return [];
      const response = await aiDataService.getAutomationRuns(configId, limit);
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to load automation runs");
      }
      return response.data;
    },
    enabled: !!configId,
    staleTime: 10000,
    refetchInterval: 30000,
  });
}

/**
 * Hook to get a specific automation run
 */
export function useAutomationRun(runId: string | null) {
  return useQuery({
    queryKey: aiDataKeys.automationRun(runId ?? ""),
    queryFn: async (): Promise<RunDetails | null> => {
      if (!runId) return null;
      const response = await aiDataService.getAutomationRun(runId);
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to load automation run");
      }
      return response.data;
    },
    enabled: !!runId,
    staleTime: 10000,
  });
}

/**
 * Hook to get JSONL logs summary
 */
export function useJsonlLogsSummary() {
  return useQuery({
    queryKey: aiDataKeys.jsonlSummary(),
    queryFn: async (): Promise<JsonlLogsSummary | null> => {
      const response = await aiDataService.getJsonlLogsSummary();
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to load logs summary");
      }
      return response.data;
    },
    staleTime: 5000, // 5 seconds
    refetchInterval: 15000, // Refetch every 15 seconds
  });
}

/**
 * Hook to read JSONL log entries
 */
export function useJsonlLogs(logType: JsonlLogType, limit?: number) {
  return useQuery({
    queryKey: [...aiDataKeys.jsonlLogs(logType), limit],
    queryFn: async (): Promise<JsonlLogsResult | null> => {
      const response = await aiDataService.readJsonlLogs(logType, limit);
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to load logs");
      }
      return response.data;
    },
    staleTime: 5000,
    refetchInterval: 10000, // Refetch every 10 seconds
  });
}
