/**
 * useAiData
 *
 * TanStack Query hooks for the AI Data Viewer.
 * Provides data fetching with caching, refetching, and error handling.
 */

import { useInfiniteQuery, useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { useEffect, useMemo, useRef } from "react";
import { aiDataService } from "../services/ai-data-service";
import { useRunnerEvents, useTaskRunProgress as useTaskRunProgressGql } from "./graphql";
import type {
  TaskRun,
  JsonlLogsResult,
  JsonlLogsSummary,
  JsonlLogType,
  TextLogsResult,
  TextLogsSummary,
  TextLogType,
  LoadedConfigInfo,
  AiPromptsResult,
  ContextsResult,
  ConsolidatedAiOutputResult,
  // SQLite migrated log types
  TaskRunEventsResult,
  BoundedReadMeta,
  TaskRunPlaywrightResultsDbResult,
  TaskRunPlaywrightResultDb,
  TaskRunMigratedLogsSummary,
  TaskRunApiRequestsDbResult,
  TaskRunApiRequestDb,
  TaskRunAwasStepsDbResult,
  TaskRunAwasStepDb,
  TaskRunVerificationResultsDbResult,
  // Process session types
  ProcessSession,
  ProcessSessionOutputLine,
} from "../types/aiData";
import type { TaskRunMcpCallsDbResult } from "../types/mcp-config";

// Thin wrappers for GraphQL subscriptions used for cache invalidation
function useRunnerEventsForInvalidation() {
  return useRunnerEvents(["TaskRunUpdate", "StepProgress"]);
}
function useTaskRunProgressForInvalidation(taskRunId: string, skip: boolean) {
  return useTaskRunProgressGql(taskRunId, skip);
}

// Keys for react-query cache
export const aiDataKeys = {
  all: ["aiData"] as const,
  taskRuns: () => [...aiDataKeys.all, "taskRuns"] as const,
  taskRun: (taskId: string) => [...aiDataKeys.all, "taskRun", taskId] as const,
  jsonlSummary: () => [...aiDataKeys.all, "jsonlSummary"] as const,
  jsonlLogs: (logType: JsonlLogType) => [...aiDataKeys.all, "jsonlLogs", logType] as const,
  consolidatedAiOutput: (taskRunId: string) =>
    [...aiDataKeys.all, "consolidatedAiOutput", taskRunId] as const,
  textSummary: () => [...aiDataKeys.all, "textSummary"] as const,
  textLogs: (logType: TextLogType) => [...aiDataKeys.all, "textLogs", logType] as const,
  loadedConfig: () => [...aiDataKeys.all, "loadedConfig"] as const,
  aiPrompts: (taskRunId: string) => [...aiDataKeys.all, "aiPrompts", taskRunId] as const,
  contexts: () => [...aiDataKeys.all, "contexts"] as const,
  // SQLite migrated logs
  taskRunEvents: (taskRunId: string, eventType?: string) =>
    [...aiDataKeys.all, "taskRunEvents", taskRunId, eventType] as const,
  taskRunPlaywrightResults: (taskRunId: string) =>
    [...aiDataKeys.all, "taskRunPlaywrightResults", taskRunId] as const,
  taskRunMigratedLogsSummary: (taskRunId: string) =>
    [...aiDataKeys.all, "taskRunMigratedLogsSummary", taskRunId] as const,
  taskRunApiRequests: (taskRunId: string, successFilter?: boolean) =>
    [...aiDataKeys.all, "taskRunApiRequests", taskRunId, successFilter] as const,
  taskRunAwasSteps: (taskRunId: string, stepType?: string) =>
    [...aiDataKeys.all, "taskRunAwasSteps", taskRunId, stepType] as const,
  taskRunMcpCalls: (taskRunId: string, successFilter?: boolean) =>
    [...aiDataKeys.all, "taskRunMcpCalls", taskRunId, successFilter] as const,
  taskRunVerificationResults: (taskRunId: string) =>
    [...aiDataKeys.all, "taskRunVerificationResults", taskRunId] as const,
  processSessions: (configId?: string) => [...aiDataKeys.all, "processSessions", configId] as const,
  processSessionOutput: (sessionId: string) =>
    [...aiDataKeys.all, "processSessionOutput", sessionId] as const,
};

/**
 * Hook to get recent task runs.
 * Uses GraphQL subscription for instant status updates when tasks are running,
 * with relaxed polling as fallback (was 3s, now 10s with sub / 30s without).
 */
export function useTaskRuns(limit?: number) {
  const queryClient = useQueryClient();

  // GraphQL subscription for real-time task updates — invalidates React Query cache
  // when any task status changes, replacing aggressive 3s polling
  const { data: eventData } = useRunnerEventsForInvalidation();
  const lastEventRef = useRef<string>("");
  useEffect(() => {
    const event = eventData?.runnerEvents;
    if (!event) return;
    const key = `${event.taskRunId}:${event.status}:${event.__typename}`;
    if (key !== lastEventRef.current) {
      lastEventRef.current = key;
      // Invalidate task runs list so React Query refetches
      queryClient.invalidateQueries({ queryKey: aiDataKeys.taskRuns() });
      if (event.taskRunId) {
        queryClient.invalidateQueries({
          queryKey: aiDataKeys.taskRun(event.taskRunId),
        });
      }
    }
  }, [eventData, queryClient]);

  return useQuery({
    queryKey: [...aiDataKeys.taskRuns(), limit],
    queryFn: async (): Promise<TaskRun[]> => {
      const response = await aiDataService.getTaskRuns(limit);
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to load task runs");
      }
      return response.data;
    },
    staleTime: 5000,
    // Relaxed polling — subscription handles real-time; this is fallback
    refetchInterval: (query) => {
      const hasRunningTask = query.state.data?.some((run) => run.status === "running");
      return hasRunningTask ? 10000 : 30000;
    },
  });
}

/**
 * Hook to get a specific task run.
 * Uses GraphQL taskRunProgress subscription for instant status updates,
 * with relaxed polling as fallback.
 */
export function useTaskRun(taskId: string | null) {
  const queryClient = useQueryClient();
  const query = useQuery({
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
    staleTime: 5000,
    // Relaxed polling — subscription handles real-time updates
    refetchInterval: (q) => {
      return q.state.data?.status === "running" ? 10000 : false;
    },
  });

  // Derive running state from query data (avoids setState-in-callback render loops)
  const taskIsRunning = query.data?.status === "running";

  // Subscribe to this task's progress when it's running
  const { data: progressData } = useTaskRunProgressForInvalidation(
    taskId ?? "",
    !taskId || !taskIsRunning,
  );

  // Invalidate cache when subscription delivers a status change
  useEffect(() => {
    const event = progressData?.taskRunProgress;
    if (!event || !taskId) return;
    if (event.__typename === "TaskRunUpdateEvent" || event.__typename === "StepProgressEvent") {
      queryClient.invalidateQueries({ queryKey: aiDataKeys.taskRun(taskId) });
      queryClient.invalidateQueries({ queryKey: aiDataKeys.taskRuns() });
    }
  }, [progressData, taskId, queryClient]);

  return query;
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

// =============================================================================
// SQLite Migrated Logs (replaces JSONL for historical queries)
// =============================================================================

/**
 * Hook to get task run events from SQLite database.
 * This replaces JSONL file reading for historical analysis.
 * @param taskRunId - Task run ID to get events for
 * @param eventType - Optional event type filter ('general', 'action', 'image_recognition', etc.)
 */
export function useTaskRunEvents(taskRunId: string | null, eventType?: string, limit?: number) {
  return useQuery({
    queryKey: [...aiDataKeys.taskRunEvents(taskRunId ?? "", eventType), limit],
    queryFn: async (): Promise<TaskRunEventsResult | null> => {
      if (!taskRunId) return null;
      const response = await aiDataService.getTaskRunEvents(taskRunId, eventType, limit);
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to load task run events");
      }
      return response.data;
    },
    enabled: !!taskRunId,
    staleTime: 10000, // 10 seconds - data is static after migration
  });
}

/**
 * A keyset walk over a bounded read, merged across every page loaded so far.
 *
 * Counts (`first.total`, `first.passed`, …) come from the FIRST page, whose
 * window counts cover the whole match set; the rows are every page's rows in
 * walk order; `remaining` is what the last page's count says lies beyond the
 * loaded rows (`null` when no exact count ran).
 */
export interface BoundedWalk<P extends BoundedReadMeta, R> {
  first: P;
  rows: R[];
  remaining: number | null;
  hasMore: boolean;
}

function mergeWalk<P extends BoundedReadMeta, R>(
  pages: P[] | undefined,
  rowsOf: (page: P) => R[],
): BoundedWalk<P, R> | null {
  if (!pages || pages.length === 0) return null;
  const last = pages[pages.length - 1];
  return {
    first: pages[0],
    rows: pages.flatMap(rowsOf),
    remaining: last.total === null ? null : last.total - last.shown,
    hasMore: last.next_cursor !== null,
  };
}

/**
 * Unwrap one page of a cursor-paged command. A refused cursor surfaces its
 * `error_code` (`cursor_malformed`) in the message.
 */
function pageOrThrow<P>(
  response: { success: boolean; data?: P; error?: string; error_code?: string },
  what: string,
): P {
  if (!response.success || !response.data) {
    const code = response.error_code ? ` [${response.error_code}]` : "";
    throw new Error((response.error || `Failed to load ${what}`) + code);
  }
  return response.data;
}

/**
 * Hook to walk a run's Playwright results by keyset cursor. `fetchNextPage`
 * loads the following page; the merged walk is in `data`.
 * @param taskRunId - Task run ID to get results for
 * @param limit - Page size (default 200, max 1000)
 */
export function useTaskRunPlaywrightResults(taskRunId: string | null, limit?: number) {
  const query = useInfiniteQuery({
    queryKey: [...aiDataKeys.taskRunPlaywrightResults(taskRunId ?? ""), limit],
    queryFn: async ({ pageParam }): Promise<TaskRunPlaywrightResultsDbResult> =>
      pageOrThrow(
        await aiDataService.getTaskRunPlaywrightResults(taskRunId ?? "", limit, pageParam),
        "Playwright results",
      ),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
    enabled: !!taskRunId,
    staleTime: 10000,
  });
  const data = useMemo(
    () =>
      mergeWalk<TaskRunPlaywrightResultsDbResult, TaskRunPlaywrightResultDb>(
        query.data?.pages,
        (p) => p.results,
      ),
    [query.data],
  );
  return { ...query, data };
}

/**
 * Hook to get summary of all migrated log data for a task run.
 * @param taskRunId - Task run ID to get summary for
 */
export function useTaskRunMigratedLogsSummary(taskRunId: string | null) {
  return useQuery({
    queryKey: aiDataKeys.taskRunMigratedLogsSummary(taskRunId ?? ""),
    queryFn: async (): Promise<TaskRunMigratedLogsSummary | null> => {
      if (!taskRunId) return null;
      const response = await aiDataService.getTaskRunMigratedLogsSummary(taskRunId);
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to load migrated logs summary");
      }
      return response.data;
    },
    enabled: !!taskRunId,
    staleTime: 10000,
  });
}

/**
 * Hook to walk a run's API requests by keyset cursor.
 * @param taskRunId - Task run ID to get API requests for
 * @param successFilter - Optional filter by success status
 * @param limit - Page size (default 200, max 1000)
 */
export function useTaskRunApiRequests(
  taskRunId: string | null,
  successFilter?: boolean,
  limit?: number,
) {
  const query = useInfiniteQuery({
    queryKey: [...aiDataKeys.taskRunApiRequests(taskRunId ?? "", successFilter), limit],
    queryFn: async ({ pageParam }): Promise<TaskRunApiRequestsDbResult> =>
      pageOrThrow(
        await aiDataService.getTaskRunApiRequests(taskRunId ?? "", successFilter, limit, pageParam),
        "API requests",
      ),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
    enabled: !!taskRunId,
    staleTime: 10000,
  });
  const data = useMemo(
    () =>
      mergeWalk<TaskRunApiRequestsDbResult, TaskRunApiRequestDb>(
        query.data?.pages,
        (p) => p.requests,
      ),
    [query.data],
  );
  return { ...query, data };
}

/**
 * Hook to walk a run's AWAS steps by keyset cursor.
 * @param taskRunId - Task run ID to get AWAS steps for
 * @param stepType - Optional filter by step type ('awas_discover', 'awas_execute', etc.)
 * @param limit - Page size (default 200, max 1000)
 */
export function useTaskRunAwasSteps(taskRunId: string | null, stepType?: string, limit?: number) {
  const query = useInfiniteQuery({
    queryKey: [...aiDataKeys.taskRunAwasSteps(taskRunId ?? "", stepType), limit],
    queryFn: async ({ pageParam }): Promise<TaskRunAwasStepsDbResult> =>
      pageOrThrow(
        await aiDataService.getTaskRunAwasSteps(taskRunId ?? "", stepType, limit, pageParam),
        "AWAS steps",
      ),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
    enabled: !!taskRunId,
    staleTime: 10000,
  });
  const data = useMemo(
    () => mergeWalk<TaskRunAwasStepsDbResult, TaskRunAwasStepDb>(query.data?.pages, (p) => p.steps),
    [query.data],
  );
  return { ...query, data };
}

/**
 * Hook to get MCP calls from SQLite database.
 * @param taskRunId - Task run ID to get MCP calls for
 * @param successFilter - Optional filter by success status
 */
export function useTaskRunMcpCalls(
  taskRunId: string | null,
  successFilter?: boolean,
  limit?: number,
  offset?: number,
) {
  return useQuery({
    queryKey: [...aiDataKeys.taskRunMcpCalls(taskRunId ?? "", successFilter), limit, offset],
    queryFn: async (): Promise<TaskRunMcpCallsDbResult | null> => {
      if (!taskRunId) return null;
      const response = await aiDataService.getTaskRunMcpCalls(
        taskRunId,
        successFilter,
        limit,
        offset,
      );
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to load MCP calls");
      }
      return response.data;
    },
    enabled: !!taskRunId,
    staleTime: 10000,
  });
}

/**
 * Hook to get verification phase results from SQLite database.
 * Returns results from all verification iterations including individual test/check results.
 * @param taskRunId - Task run ID to get verification results for
 */
export function useTaskRunVerificationResults(taskRunId: string | null) {
  return useQuery({
    queryKey: aiDataKeys.taskRunVerificationResults(taskRunId ?? ""),
    queryFn: async (): Promise<TaskRunVerificationResultsDbResult | null> => {
      if (!taskRunId) return null;
      const response = await aiDataService.getTaskRunVerificationResults(taskRunId);
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to load verification results");
      }
      return response.data;
    },
    enabled: !!taskRunId,
    staleTime: 10000,
  });
}

// =============================================================================
// Process Sessions (persistent process history)
// =============================================================================

/**
 * Hook to get process sessions from database.
 * @param configId - Optional process config ID to filter by
 * @param limit - Maximum number of sessions to return
 */
export function useProcessSessions(configId?: string, limit?: number) {
  return useQuery({
    queryKey: [...aiDataKeys.processSessions(configId), limit],
    queryFn: async (): Promise<ProcessSession[]> => {
      const response = await aiDataService.getProcessSessions(configId, limit);
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to load process sessions");
      }
      return response.data;
    },
    staleTime: 10000,
  });
}

/**
 * Hook to get process session output from database.
 * @param sessionId - Session ID to get output for (null to disable)
 * @param limit - Maximum number of lines to return
 * @param offset - Offset for pagination
 */
export function useProcessSessionOutput(sessionId: string | null, limit?: number, offset?: number) {
  return useQuery({
    queryKey: [...aiDataKeys.processSessionOutput(sessionId ?? ""), limit, offset],
    queryFn: async (): Promise<ProcessSessionOutputLine[]> => {
      if (!sessionId) return [];
      const response = await aiDataService.getProcessSessionOutput(sessionId, limit, offset);
      if (!response.success || !response.data) {
        throw new Error(response.error || "Failed to load process session output");
      }
      return response.data;
    },
    enabled: !!sessionId,
    staleTime: 30000,
  });
}
