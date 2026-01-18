/**
 * useMobileData
 *
 * TanStack Query hooks for mobile development feedback.
 * Provides data fetching with caching, refetching, and error handling.
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { mobileService } from "../services/mobile-service";
import type {
  MobileDevice,
  MobileState,
  MobileLog,
  MobileScreenshotResult,
  LogcatResult,
  MobileFeedbackResult,
} from "../types/mobile";

// Keys for react-query cache
export const mobileDataKeys = {
  all: ["mobileData"] as const,
  devices: () => [...mobileDataKeys.all, "devices"] as const,
  states: (taskRunId: string) => [...mobileDataKeys.all, "states", taskRunId] as const,
  latestState: (taskRunId: string) => [...mobileDataKeys.all, "latestState", taskRunId] as const,
  logs: (taskRunId: string, logSource?: string, errorsOnly?: boolean) =>
    [...mobileDataKeys.all, "logs", taskRunId, logSource, errorsOnly] as const,
  errors: (taskRunId: string) => [...mobileDataKeys.all, "errors", taskRunId] as const,
};

// ===========================================================================
// Device Hooks
// ===========================================================================

/**
 * Hook to list connected mobile devices.
 * Polls every 10 seconds to detect device connections/disconnections.
 */
export function useMobileDevices() {
  return useQuery({
    queryKey: mobileDataKeys.devices(),
    queryFn: async (): Promise<MobileDevice[]> => {
      const response = await mobileService.listDevices();
      if (!response.success || !response.data) {
        throw new Error(response.message || "Failed to list devices");
      }
      return response.data;
    },
    staleTime: 5000,
    refetchInterval: 10000, // Poll every 10 seconds
  });
}

// ===========================================================================
// Mobile State Hooks
// ===========================================================================

/**
 * Hook to get mobile states for a task run.
 * @param taskRunId - Task run ID to query
 * @param limit - Maximum number of results
 */
export function useMobileStates(taskRunId: string | null, limit?: number) {
  return useQuery({
    queryKey: [...mobileDataKeys.states(taskRunId ?? ""), limit],
    queryFn: async (): Promise<MobileState[]> => {
      if (!taskRunId) return [];
      const response = await mobileService.getMobileStates(taskRunId, limit);
      if (!response.success || !response.data) {
        throw new Error(response.message || "Failed to get mobile states");
      }
      return response.data;
    },
    enabled: !!taskRunId,
    staleTime: 5000,
  });
}

/**
 * Hook to get the latest mobile state for a task run.
 * @param taskRunId - Task run ID to query
 */
export function useLatestMobileState(taskRunId: string | null) {
  return useQuery({
    queryKey: mobileDataKeys.latestState(taskRunId ?? ""),
    queryFn: async (): Promise<MobileState | null> => {
      if (!taskRunId) return null;
      const response = await mobileService.getLatestMobileState(taskRunId);
      if (!response.success) {
        throw new Error(response.message || "Failed to get latest mobile state");
      }
      return response.data ?? null;
    },
    enabled: !!taskRunId,
    staleTime: 5000,
  });
}

// ===========================================================================
// Mobile Log Hooks
// ===========================================================================

/**
 * Hook to get mobile logs for a task run.
 * @param taskRunId - Task run ID to query
 * @param logSource - Optional filter by log source
 * @param errorsOnly - If true, only return error logs
 * @param limit - Maximum number of results
 */
export function useMobileLogs(
  taskRunId: string | null,
  logSource?: string,
  errorsOnly?: boolean,
  limit?: number,
) {
  return useQuery({
    queryKey: [...mobileDataKeys.logs(taskRunId ?? "", logSource, errorsOnly), limit],
    queryFn: async (): Promise<MobileLog[]> => {
      if (!taskRunId) return [];
      const response = await mobileService.getMobileLogs(taskRunId, logSource, errorsOnly, limit);
      if (!response.success || !response.data) {
        throw new Error(response.message || "Failed to get mobile logs");
      }
      return response.data;
    },
    enabled: !!taskRunId,
    staleTime: 5000,
  });
}

/**
 * Hook to get mobile error logs for a task run.
 * @param taskRunId - Task run ID to query
 * @param limit - Maximum number of results
 */
export function useMobileErrors(taskRunId: string | null, limit?: number) {
  return useQuery({
    queryKey: [...mobileDataKeys.errors(taskRunId ?? ""), limit],
    queryFn: async (): Promise<MobileLog[]> => {
      if (!taskRunId) return [];
      const response = await mobileService.getMobileErrors(taskRunId, limit);
      if (!response.success || !response.data) {
        throw new Error(response.message || "Failed to get mobile errors");
      }
      return response.data;
    },
    enabled: !!taskRunId,
    staleTime: 5000,
  });
}

// ===========================================================================
// Capture Mutations
// ===========================================================================

/**
 * Hook to capture a mobile screenshot.
 */
export function useCaptureScreenshot() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({
      taskRunId,
      deviceId,
    }: {
      taskRunId?: string;
      deviceId?: string;
    }): Promise<MobileScreenshotResult> => {
      const response = await mobileService.captureScreenshot(taskRunId, deviceId);
      if (!response.success || !response.data) {
        throw new Error(response.message || "Failed to capture screenshot");
      }
      return response.data;
    },
    onSuccess: (_, variables) => {
      // Invalidate mobile states if we have a task run ID
      if (variables.taskRunId) {
        queryClient.invalidateQueries({
          queryKey: mobileDataKeys.states(variables.taskRunId),
        });
        queryClient.invalidateQueries({
          queryKey: mobileDataKeys.latestState(variables.taskRunId),
        });
      }
    },
  });
}

/**
 * Hook to capture mobile logcat.
 */
export function useCaptureLogcat() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({
      taskRunId,
      deviceId,
      lines,
      filterApp,
    }: {
      taskRunId?: string;
      deviceId?: string;
      lines?: number;
      filterApp?: boolean;
    }): Promise<LogcatResult> => {
      const response = await mobileService.captureLogcat(taskRunId, deviceId, lines, filterApp);
      if (!response.success || !response.data) {
        throw new Error(response.message || "Failed to capture logcat");
      }
      return response.data;
    },
    onSuccess: (_, variables) => {
      // Invalidate mobile logs if we have a task run ID
      if (variables.taskRunId) {
        queryClient.invalidateQueries({
          queryKey: mobileDataKeys.logs(variables.taskRunId),
        });
        queryClient.invalidateQueries({
          queryKey: mobileDataKeys.errors(variables.taskRunId),
        });
      }
    },
  });
}

/**
 * Hook to capture both screenshot and logcat.
 */
export function useCaptureFeedback() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({
      taskRunId,
      deviceId,
      logcatLines,
    }: {
      taskRunId: string;
      deviceId?: string;
      logcatLines?: number;
    }): Promise<MobileFeedbackResult> => {
      const response = await mobileService.captureFeedback(taskRunId, deviceId, logcatLines);
      if (!response.success || !response.data) {
        throw new Error(response.message || "Failed to capture feedback");
      }
      return response.data;
    },
    onSuccess: (_, variables) => {
      // Invalidate all mobile data for this task run
      queryClient.invalidateQueries({
        queryKey: mobileDataKeys.states(variables.taskRunId),
      });
      queryClient.invalidateQueries({
        queryKey: mobileDataKeys.latestState(variables.taskRunId),
      });
      queryClient.invalidateQueries({
        queryKey: mobileDataKeys.logs(variables.taskRunId),
      });
      queryClient.invalidateQueries({
        queryKey: mobileDataKeys.errors(variables.taskRunId),
      });
    },
  });
}

/**
 * Hook to delete mobile data for a task run.
 */
export function useDeleteMobileData() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (
      taskRunId: string,
    ): Promise<{ states_deleted: number; logs_deleted: number }> => {
      const response = await mobileService.deleteMobileData(taskRunId);
      if (!response.success || !response.data) {
        throw new Error(response.message || "Failed to delete mobile data");
      }
      return response.data;
    },
    onSuccess: (_, taskRunId) => {
      // Invalidate all mobile data for this task run
      queryClient.invalidateQueries({
        queryKey: mobileDataKeys.states(taskRunId),
      });
      queryClient.invalidateQueries({
        queryKey: mobileDataKeys.latestState(taskRunId),
      });
      queryClient.invalidateQueries({
        queryKey: mobileDataKeys.logs(taskRunId),
      });
      queryClient.invalidateQueries({
        queryKey: mobileDataKeys.errors(taskRunId),
      });
    },
  });
}
