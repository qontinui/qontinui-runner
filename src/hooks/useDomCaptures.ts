/**
 * useDomCaptures
 *
 * TanStack Query hooks for DOM capture data.
 * Provides data fetching with caching, refetching, and error handling.
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { domCaptureService } from "../services/dom-capture-service";
import type { DomCapture, CaptureFromExtensionRequest } from "../types/domCapture";

// Query keys for react-query cache
export const domCaptureKeys = {
  all: ["domCaptures"] as const,
  list: () => [...domCaptureKeys.all, "list"] as const,
  listForTask: (taskRunId: string) => [...domCaptureKeys.all, "list", taskRunId] as const,
  detail: (id: string) => [...domCaptureKeys.all, "detail", id] as const,
  html: (id: string) => [...domCaptureKeys.all, "html", id] as const,
};

/**
 * Hook to get all DOM captures.
 * Optionally filter by task run ID.
 * Polls periodically to pick up new captures.
 */
export function useDomCaptures(taskRunId?: string) {
  return useQuery({
    queryKey: taskRunId ? domCaptureKeys.listForTask(taskRunId) : domCaptureKeys.list(),
    queryFn: async (): Promise<DomCapture[]> => {
      if (taskRunId) {
        return domCaptureService.listCapturesForTask(taskRunId);
      }
      return domCaptureService.listCaptures();
    },
    staleTime: 5000, // 5 seconds
    refetchInterval: 15000, // Poll every 15 seconds
  });
}

/**
 * Hook to get a specific DOM capture by ID.
 * @param id - DOM capture ID (or null to disable)
 */
export function useDomCapture(id: string | null) {
  return useQuery({
    queryKey: domCaptureKeys.detail(id ?? ""),
    queryFn: async (): Promise<DomCapture | null> => {
      if (!id) return null;
      return domCaptureService.getCapture(id);
    },
    enabled: !!id,
    staleTime: 60000, // 1 minute - captures don't change
  });
}

/**
 * Hook to get the HTML content of a DOM capture.
 * @param id - DOM capture ID (or null to disable)
 */
export function useDomCaptureHtml(id: string | null) {
  return useQuery({
    queryKey: domCaptureKeys.html(id ?? ""),
    queryFn: async (): Promise<string | null> => {
      if (!id) return null;
      return domCaptureService.getCaptureHtml(id);
    },
    enabled: !!id,
    staleTime: Infinity, // HTML content never changes
  });
}

/**
 * Hook to submit a DOM capture from an external source.
 * Returns a mutation that can be called with capture data.
 */
export function useSubmitDomCapture() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (request: CaptureFromExtensionRequest) =>
      domCaptureService.submitFromExtension(request),
    onSuccess: () => {
      // Invalidate the list to pick up the new capture
      queryClient.invalidateQueries({ queryKey: domCaptureKeys.list() });
    },
  });
}

/**
 * Hook to invalidate DOM capture queries.
 * Useful when you know captures have changed (e.g., after receiving a new capture).
 */
export function useInvalidateDomCaptures() {
  const queryClient = useQueryClient();

  return () => {
    queryClient.invalidateQueries({ queryKey: domCaptureKeys.all });
  };
}
