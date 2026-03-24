/**
 * useLlmAnalytics
 *
 * TanStack Query hooks for the LLM Observability Dashboard.
 * Fetches token usage, cost, and latency data from Tauri commands.
 */

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type {
  TokenUsageSummary,
  DailyCostRow,
  ModelCostRow,
  PhaseCostRow,
  ProviderLatencyRow,
  TaskRunCostRow,
  LlmTimeRange,
} from "../components/llm-observability/types";

/** Convert time range to days parameter for Tauri commands */
function timeRangeToDays(range: LlmTimeRange): number {
  switch (range) {
    case "1d":
      return 1;
    case "7d":
      return 7;
    case "30d":
      return 30;
    case "all":
      return 9999;
  }
}

// Query keys for cache management
export const llmAnalyticsKeys = {
  all: ["llm-analytics"] as const,
  summary: (days: number) => [...llmAnalyticsKeys.all, "summary", days] as const,
  dailyCost: (days: number) => [...llmAnalyticsKeys.all, "daily-cost", days] as const,
  costByModel: (days: number) => [...llmAnalyticsKeys.all, "cost-by-model", days] as const,
  costByPhase: (days: number) => [...llmAnalyticsKeys.all, "cost-by-phase", days] as const,
  providerLatency: (days: number) => [...llmAnalyticsKeys.all, "provider-latency", days] as const,
  taskRunCosts: (days: number) => [...llmAnalyticsKeys.all, "task-run-costs", days] as const,
};

export interface UseLlmAnalyticsResult {
  summary: TokenUsageSummary | null;
  dailyCost: DailyCostRow[];
  costByModel: ModelCostRow[];
  costByPhase: PhaseCostRow[];
  providerLatency: ProviderLatencyRow[];
  taskRunCosts: TaskRunCostRow[];
  loading: boolean;
  error: string | null;
  refresh: () => void;
}

/**
 * Hook to fetch all LLM analytics data for the dashboard.
 */
export function useLlmAnalytics(timeRange: LlmTimeRange = "7d"): UseLlmAnalyticsResult {
  const queryClient = useQueryClient();
  const days = timeRangeToDays(timeRange);

  const summaryQuery = useQuery({
    queryKey: llmAnalyticsKeys.summary(days),
    queryFn: () => invoke<TokenUsageSummary>("get_token_usage_summary", { days }),
    staleTime: 30000,
    refetchInterval: 60000,
  });

  const dailyCostQuery = useQuery({
    queryKey: llmAnalyticsKeys.dailyCost(days),
    queryFn: () => invoke<DailyCostRow[]>("get_daily_cost", { days }),
    staleTime: 30000,
    refetchInterval: 60000,
  });

  const costByModelQuery = useQuery({
    queryKey: llmAnalyticsKeys.costByModel(days),
    queryFn: () => invoke<ModelCostRow[]>("get_cost_by_model", { days }),
    staleTime: 30000,
    refetchInterval: 60000,
  });

  const costByPhaseQuery = useQuery({
    queryKey: llmAnalyticsKeys.costByPhase(days),
    queryFn: () => invoke<PhaseCostRow[]>("get_cost_by_phase", { days }),
    staleTime: 30000,
    refetchInterval: 60000,
  });

  const providerLatencyQuery = useQuery({
    queryKey: llmAnalyticsKeys.providerLatency(days),
    queryFn: () => invoke<ProviderLatencyRow[]>("get_provider_latency", { days }),
    staleTime: 30000,
    refetchInterval: 60000,
  });

  const taskRunCostsQuery = useQuery({
    queryKey: llmAnalyticsKeys.taskRunCosts(days),
    queryFn: () => invoke<TaskRunCostRow[]>("get_task_run_costs", { days, limit: 50 }),
    staleTime: 30000,
    refetchInterval: 60000,
  });

  const loading =
    summaryQuery.isLoading ||
    dailyCostQuery.isLoading ||
    costByModelQuery.isLoading ||
    costByPhaseQuery.isLoading ||
    providerLatencyQuery.isLoading ||
    taskRunCostsQuery.isLoading;

  const error =
    summaryQuery.error?.message ??
    dailyCostQuery.error?.message ??
    costByModelQuery.error?.message ??
    costByPhaseQuery.error?.message ??
    providerLatencyQuery.error?.message ??
    taskRunCostsQuery.error?.message ??
    null;

  const refresh = () => {
    queryClient.invalidateQueries({ queryKey: llmAnalyticsKeys.all });
  };

  return {
    summary: summaryQuery.data ?? null,
    dailyCost: dailyCostQuery.data ?? [],
    costByModel: costByModelQuery.data ?? [],
    costByPhase: costByPhaseQuery.data ?? [],
    providerLatency: providerLatencyQuery.data ?? [],
    taskRunCosts: taskRunCostsQuery.data ?? [],
    loading,
    error,
    refresh,
  };
}
