/**
 * HealthScoreCard.tsx
 *
 * Gauge-style card showing the composite automation health score
 * with a breakdown of contributing factors.
 */

import { Activity, AlertTriangle, RefreshCw } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import { withTimeout } from "@/lib/withTimeout";

interface AutomationHealthScore {
  overall_score: number;
  element_success_rate: number;
  regression_rate: number;
  stall_frequency: number;
  total_interactions: number;
  total_elements: number;
  total_stalls: number;
}

interface HealthScoreCardProps {
  days?: number;
}

function scoreColor(score: number): string {
  if (score >= 0.9) return "text-green-500";
  if (score >= 0.7) return "text-yellow-500";
  if (score >= 0.5) return "text-orange-500";
  return "text-red-500";
}

function scoreLabel(score: number): string {
  if (score >= 0.9) return "Excellent";
  if (score >= 0.7) return "Good";
  if (score >= 0.5) return "Fair";
  return "Poor";
}

function MetricRow({
  label,
  value,
  format = "percent",
}: {
  label: string;
  value: number;
  format?: "percent" | "count";
}) {
  return (
    <div className="flex justify-between items-center text-sm">
      <span className="text-muted-foreground">{label}</span>
      <span className="tabular-nums font-medium">
        {format === "percent" ? `${Math.round(value * 100)}%` : value}
      </span>
    </div>
  );
}

export function HealthScoreCard({ days = 7 }: HealthScoreCardProps) {
  const { data, isLoading, isError, error, refetch, isFetching } = useQuery<AutomationHealthScore>({
    queryKey: ["graph-analytics", "health-score", days],
    queryFn: async ({ signal }) => {
      // iter4 B-2: bound the fetch so a hung backend (e.g. the PG pool
      // stall this remediation also fixes server-side, B-1) surfaces an
      // error+retry instead of an eternal "Loading health score…" spinner.
      // The AbortSignal cancels the in-flight request when the timeout wins.
      const res = await withTimeout(
        fetch(`http://localhost:9876/ui-bridge/analytics/health-score?days=${days}`, {
          signal,
        }),
        10_000,
        "health-score",
      );
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const json = await res.json();
      return json.data;
    },
    staleTime: 30_000,
    refetchInterval: 60_000,
    retry: 1,
  });

  if (isError) {
    return (
      <div className="bg-card rounded-lg border border-border p-6">
        <div className="text-center py-4">
          <AlertTriangle className="w-8 h-8 mx-auto mb-2 text-red-500 opacity-80" />
          <p className="text-sm text-red-500">
            Failed to load health score
            {error instanceof Error ? `: ${error.message}` : ""}
          </p>
          <button
            type="button"
            onClick={() => refetch()}
            disabled={isFetching}
            className="btn-secondary mt-3 inline-flex items-center gap-2"
          >
            <RefreshCw className={`w-3.5 h-3.5 ${isFetching ? "animate-spin" : ""}`} />
            Retry
          </button>
        </div>
      </div>
    );
  }

  if (isLoading || !data) {
    return (
      <div className="bg-card rounded-lg border border-border p-6">
        <div className="text-center text-muted-foreground py-4">Loading health score...</div>
      </div>
    );
  }

  const pct = Math.round(data.overall_score * 100);

  return (
    <div className="bg-card rounded-lg border border-border p-6">
      <div className="flex items-center justify-between mb-4">
        <h3 className="font-semibold flex items-center gap-2">
          <Activity className="w-4 h-4" />
          Automation Health
        </h3>
        <span className="text-xs text-muted-foreground">{days}d window</span>
      </div>

      {/* Score display */}
      <div className="text-center mb-6">
        <div className={`text-5xl font-bold tabular-nums ${scoreColor(data.overall_score)}`}>
          {pct}
        </div>
        <div className={`text-sm font-medium mt-1 ${scoreColor(data.overall_score)}`}>
          {scoreLabel(data.overall_score)}
        </div>
      </div>

      {/* Breakdown */}
      <div className="space-y-2 border-t border-border pt-4">
        <MetricRow label="Element Success Rate" value={data.element_success_rate} />
        <MetricRow label="Regression Rate" value={data.regression_rate} />
        <MetricRow label="Stall Frequency" value={data.stall_frequency} />
        <MetricRow label="Total Interactions" value={data.total_interactions} format="count" />
        <MetricRow label="Unique Elements" value={data.total_elements} format="count" />
      </div>
    </div>
  );
}
