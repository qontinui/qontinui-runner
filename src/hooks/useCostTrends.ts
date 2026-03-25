/**
 * useCostTrends
 *
 * Fetches daily cost data from the cost-trends analytics endpoint and
 * computes a 7-day rolling average trend line client-side.
 */

import { useQuery } from "@tanstack/react-query";
import { getApiBase, tracedFetch } from "../lib/runner-api";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface CostTrendDataPoint {
  date: string;
  total_cost_cents: number;
  call_count: number;
  total_input_tokens: number;
  total_output_tokens: number;
}

export interface CostTrendStats {
  totalSpend: number;
  avgDailyCost: number;
  trendDirection: "up" | "down" | "flat";
}

export interface CostTrendChartPoint {
  date: string;
  cost: number;
  calls: number;
  inputTokens: number;
  outputTokens: number;
  movingAvg: number | null;
}

export interface UseCostTrendsReturn {
  data: CostTrendChartPoint[];
  stats: CostTrendStats;
  loading: boolean;
  error: string | null;
  refetch: () => void;
}

// ---------------------------------------------------------------------------
// Query keys
// ---------------------------------------------------------------------------

export const costTrendKeys = {
  all: ["cost-trends"] as const,
  list: () => [...costTrendKeys.all, "list"] as const,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function computeMovingAverage(values: number[], window: number): (number | null)[] {
  return values.map((_, i) => {
    if (i < window - 1) return null;
    const slice = values.slice(i - window + 1, i + 1);
    return slice.reduce((sum, v) => sum + v, 0) / window;
  });
}

function computeTrendDirection(data: CostTrendChartPoint[]): "up" | "down" | "flat" {
  if (data.length < 2) return "flat";
  // Compare average of last 7 days vs previous 7 days
  const recent = data.slice(-7);
  const previous = data.slice(-14, -7);
  if (previous.length === 0) return "flat";

  const recentAvg = recent.reduce((s, d) => s + d.cost, 0) / recent.length;
  const prevAvg = previous.reduce((s, d) => s + d.cost, 0) / previous.length;

  const pctChange = prevAvg === 0 ? 0 : (recentAvg - prevAvg) / prevAvg;
  if (pctChange > 0.05) return "up";
  if (pctChange < -0.05) return "down";
  return "flat";
}

// ---------------------------------------------------------------------------
// Fetcher
// ---------------------------------------------------------------------------

async function fetchCostTrends(): Promise<CostTrendDataPoint[]> {
  const res = await tracedFetch(
    `${getApiBase()}/api/v1/execution/analytics/cost-trends`,
  );
  if (!res.ok) {
    throw new Error(`Failed to fetch cost trends (HTTP ${res.status})`);
  }
  return res.json();
}

// ---------------------------------------------------------------------------
// Hook
// ---------------------------------------------------------------------------

export function useCostTrends(): UseCostTrendsReturn {
  const query = useQuery({
    queryKey: costTrendKeys.list(),
    queryFn: fetchCostTrends,
    staleTime: 60_000,
    refetchInterval: 120_000,
  });

  const raw = query.data ?? [];
  const costs = raw.map((d) => d.total_cost_cents / 100);
  const movingAvgs = computeMovingAverage(costs, 7);

  const data: CostTrendChartPoint[] = raw.map((d, i) => ({
    date: d.date,
    cost: d.total_cost_cents / 100,
    calls: d.call_count,
    inputTokens: d.total_input_tokens,
    outputTokens: d.total_output_tokens,
    movingAvg: movingAvgs[i],
  }));

  const totalSpend = costs.reduce((s, c) => s + c, 0);
  const avgDailyCost = costs.length > 0 ? totalSpend / costs.length : 0;
  const trendDirection = computeTrendDirection(data);

  return {
    data,
    stats: { totalSpend, avgDailyCost, trendDirection },
    loading: query.isLoading,
    error: query.error instanceof Error ? query.error.message : null,
    refetch: query.refetch,
  };
}
