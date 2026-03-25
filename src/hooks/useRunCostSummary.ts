/**
 * useRunCostSummary
 *
 * TanStack Query hook for fetching per-run LLM cost breakdown.
 * Consumes GET /api/v1/execution/runs/{runId}/cost-summary.
 */

import { useQuery } from "@tanstack/react-query";
import { getApiBase, tracedFetch } from "../lib/runner-api";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface ModelCostBreakdown {
  model: string;
  provider: string;
  tokens_input: number;
  tokens_output: number;
  cost_usd: number;
  action_count: number;
}

export interface RunCostSummary {
  run_id: string;
  total_tokens_input: number;
  total_tokens_output: number;
  total_cost_usd: number;
  llm_action_count: number;
  per_model: ModelCostBreakdown[];
}

export interface UseRunCostSummaryReturn {
  data: RunCostSummary | null;
  loading: boolean;
  error: Error | null;
  refetch: () => void;
}

// ---------------------------------------------------------------------------
// Query keys
// ---------------------------------------------------------------------------

export const runCostSummaryKeys = {
  all: ["runCostSummary"] as const,
  byRun: (runId: string) => [...runCostSummaryKeys.all, runId] as const,
};

// ---------------------------------------------------------------------------
// Fetcher
// ---------------------------------------------------------------------------

async function fetchRunCostSummary(runId: string): Promise<RunCostSummary> {
  const res = await tracedFetch(
    `${getApiBase()}/api/v1/execution/runs/${runId}/cost-summary`,
  );
  if (!res.ok) {
    throw new Error(`Failed to fetch cost summary (HTTP ${res.status})`);
  }
  return res.json();
}

// ---------------------------------------------------------------------------
// Hook
// ---------------------------------------------------------------------------

/**
 * Fetches the LLM cost breakdown for the given execution run.
 * Cached for 60 seconds (cost data is unlikely to change frequently).
 */
export function useRunCostSummary(runId: string | null | undefined): UseRunCostSummaryReturn {
  const query = useQuery({
    queryKey: runCostSummaryKeys.byRun(runId ?? ""),
    queryFn: () => fetchRunCostSummary(runId!),
    enabled: !!runId,
    staleTime: 60_000,
  });

  return {
    data: query.data ?? null,
    loading: query.isLoading,
    error: query.error instanceof Error ? query.error : null,
    refetch: query.refetch,
  };
}
