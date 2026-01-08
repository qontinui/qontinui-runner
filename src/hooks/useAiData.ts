/**
 * useAiData
 *
 * TanStack Query hooks for the AI Data Viewer.
 * Provides data fetching with caching, refetching, and error handling.
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { aiDataService } from "../services/ai-data-service";
import type {
  TaskRun,
  JsonlLogsResult,
  JsonlLogsSummary,
  JsonlLogType,
  TextLogsResult,
  TextLogsSummary,
  TextLogType,
  ScreenshotsResult,
  LoadedConfigInfo,
  AiPromptsResult,
  ContextsResult,
  ConsolidatedAiOutputResult,
} from "../types/aiData";
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
  consolidatedAiOutput: (taskRunId: string) =>
    [...aiDataKeys.all, "consolidatedAiOutput", taskRunId] as const,
  textSummary: () => [...aiDataKeys.all, "textSummary"] as const,
  textLogs: (logType: TextLogType) => [...aiDataKeys.all, "textLogs", logType] as const,
  screenshots: () => [...aiDataKeys.all, "screenshots"] as const,
  loadedConfig: () => [...aiDataKeys.all, "loadedConfig"] as const,
  aiPrompts: (taskRunId: string) => [...aiDataKeys.all, "aiPrompts", taskRunId] as const,
  contexts: () => [...aiDataKeys.all, "contexts"] as const,
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
 * Hook to read JSONL log entries (unfiltered)
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

/**
 * Hook to read JSONL log entries filtered by task run time range
 */
export function useJsonlLogsForTaskRun(logType: JsonlLogType, taskRunId: string | null) {
  return useQuery({
    queryKey: [...aiDataKeys.jsonlLogs(logType), "taskRun", taskRunId],
    queryFn: async (): Promise<JsonlLogsResult | null> => {
      if (!taskRunId) return null;
      const response = await aiDataService.readJsonlLogsForTaskRun(logType, taskRunId);
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to load logs");
      }
      return response.data;
    },
    enabled: !!taskRunId,
    staleTime: 5000,
    refetchInterval: 10000,
  });
}

/**
 * Hook to get consolidated AI output for a task run
 * Groups consecutive log entries by source into readable chunks
 */
export function useConsolidatedAiOutput(taskRunId: string | null) {
  return useQuery({
    queryKey: aiDataKeys.consolidatedAiOutput(taskRunId ?? ""),
    queryFn: async (): Promise<ConsolidatedAiOutputResult | null> => {
      if (!taskRunId) return null;
      const response = await aiDataService.getConsolidatedAiOutput(taskRunId);
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to load consolidated AI output");
      }
      return response.data;
    },
    enabled: !!taskRunId,
    staleTime: 5000,
    refetchInterval: 10000,
  });
}

/**
 * Hook to reopen a finished task run with additional iterations
 */
export function useReopenTaskRun() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({
      taskId,
      additionalSessions,
    }: {
      taskId: string;
      additionalSessions: number;
    }): Promise<TaskRun> => {
      const response = await aiDataService.reopenTaskRun(taskId, additionalSessions);
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to reopen task run");
      }
      return response.data;
    },
    onSuccess: (updatedRun) => {
      // Invalidate task runs list to refresh
      queryClient.invalidateQueries({ queryKey: aiDataKeys.taskRuns() });
      // Update the specific task run in cache
      queryClient.setQueryData(aiDataKeys.taskRun(updatedRun.id), updatedRun);
    },
  });
}

// =============================================================================
// Text Logs (plain text, filtered by task run time range)
// =============================================================================

/**
 * Hook to get text logs summary for a task run
 */
export function useTextLogsSummary(taskRunId: string | null) {
  return useQuery({
    queryKey: [...aiDataKeys.textSummary(), taskRunId],
    queryFn: async (): Promise<TextLogsSummary | null> => {
      if (!taskRunId) return null;
      const response = await aiDataService.getTextLogsSummary(taskRunId);
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to load text logs summary");
      }
      return response.data;
    },
    enabled: !!taskRunId,
    staleTime: 5000, // 5 seconds
    refetchInterval: 15000, // Refetch every 15 seconds
  });
}

/**
 * Hook to read text log content for a task run
 */
export function useTextLogs(logType: TextLogType, taskRunId: string | null) {
  return useQuery({
    queryKey: [...aiDataKeys.textLogs(logType), taskRunId],
    queryFn: async (): Promise<TextLogsResult | null> => {
      if (!taskRunId) return null;
      const response = await aiDataService.readTextLogs(logType, taskRunId);
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to load text logs");
      }
      return response.data;
    },
    enabled: !!taskRunId,
    staleTime: 5000,
    refetchInterval: 10000, // Refetch every 10 seconds
  });
}

// =============================================================================
// Screenshots
// =============================================================================

/**
 * Hook to get screenshots (annotated and playwright)
 */
export function useScreenshots() {
  return useQuery({
    queryKey: aiDataKeys.screenshots(),
    queryFn: async (): Promise<ScreenshotsResult | null> => {
      const response = await aiDataService.getScreenshots();
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to load screenshots");
      }
      return response.data;
    },
    staleTime: 5000,
    refetchInterval: 15000, // Refetch every 15 seconds
  });
}

// =============================================================================
// Loaded Config
// =============================================================================

/**
 * Hook to get the currently loaded workflow config
 */
export function useLoadedConfig() {
  return useQuery({
    queryKey: aiDataKeys.loadedConfig(),
    queryFn: async (): Promise<LoadedConfigInfo | null> => {
      const response = await aiDataService.getLoadedConfig();
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to load config");
      }
      return response.data;
    },
    staleTime: 10000, // 10 seconds
    refetchInterval: 30000, // Refetch every 30 seconds
  });
}

// =============================================================================
// AI Prompts
// =============================================================================

/**
 * Hook to get AI prompts for a task run
 */
export function useAiPrompts(taskRunId: string | null) {
  return useQuery({
    queryKey: aiDataKeys.aiPrompts(taskRunId ?? ""),
    queryFn: async (): Promise<AiPromptsResult | null> => {
      if (!taskRunId) return null;
      const response = await aiDataService.getAiPrompts(taskRunId);
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to load AI prompts");
      }
      return response.data;
    },
    enabled: !!taskRunId,
    staleTime: 10000,
  });
}

// =============================================================================
// Contexts
// =============================================================================

/**
 * Hook to get all available contexts
 */
export function useContexts() {
  return useQuery({
    queryKey: aiDataKeys.contexts(),
    queryFn: async (): Promise<ContextsResult | null> => {
      const response = await aiDataService.getContexts();
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to load contexts");
      }
      return response.data;
    },
    staleTime: 30000, // 30 seconds - contexts change less frequently
    refetchInterval: 60000, // Refetch every 60 seconds
  });
}
