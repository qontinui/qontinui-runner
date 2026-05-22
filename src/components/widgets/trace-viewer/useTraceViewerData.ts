import { useQuery } from "@tanstack/react-query";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { useWorkflowExecutionOptional } from "@/contexts/WorkflowExecutionContext";
import type { TraceSpan, TraceInsights } from "./types";
import { computeInsights } from "./trace-utils";

async function fetchExecutionSpans(executionId: string): Promise<TraceSpan[]> {
  const response = await tracedFetch(
    `${getApiBase()}/execution-spans?execution_id=${encodeURIComponent(executionId)}&order=asc`,
  );
  if (!response.ok) {
    throw new Error(`Failed to fetch spans: ${response.statusText}`);
  }
  // The runner endpoint returns an envelope `{ count, filters, spans }`,
  // not a bare array. Earlier consumers passed the envelope straight through,
  // which slipped past TS (no runtime check) and surfaced as
  // `spans is not iterable` inside computeInsights() when TraceViewerWidget
  // was rendered for an empty/non-array shape. Unwrap defensively: accept
  // either the envelope or a bare array, default to [].
  const body = await response.json();
  if (Array.isArray(body)) return body;
  if (body && Array.isArray(body.spans)) return body.spans;
  return [];
}

/** Hook to fetch execution spans for a task run. */
export function useTraceViewerData(executionId: string | null) {
  return useQuery({
    queryKey: ["execution-spans", executionId],
    queryFn: () => fetchExecutionSpans(executionId!),
    enabled: !!executionId,
    refetchInterval: 2000, // Poll every 2 seconds for live data
    staleTime: 1000,
  });
}

/** Data shape for the widget registry. */
export interface TraceViewerWidgetData {
  spans: TraceSpan[];
  executionId: string | null;
  insights: TraceInsights;
  isLoading: boolean;
  error: string | null;
}

/** Widget-registry-compatible hook (no arguments). Gets executionId from context. */
export function useTraceViewerWidgetData(): TraceViewerWidgetData {
  const workflowContext = useWorkflowExecutionOptional();
  const executionId = workflowContext?.taskRunId ?? null;
  const { data: spans = [], isLoading, error } = useTraceViewerData(executionId);
  const insights = computeInsights(spans);

  return {
    spans,
    executionId,
    insights,
    isLoading,
    error: error ? String(error) : null,
  };
}
