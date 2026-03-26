/**
 * HealthScoreCard.tsx
 *
 * Gauge-style card showing the composite automation health score
 * with a breakdown of contributing factors.
 */

import { Activity } from "lucide-react";
import { useQuery } from "@tanstack/react-query";

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

function MetricRow({ label, value, format = "percent" }: { label: string; value: number; format?: "percent" | "count" }) {
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
  const { data, isLoading } = useQuery<AutomationHealthScore>({
    queryKey: ["graph-analytics", "health-score", days],
    queryFn: async () => {
      const res = await fetch(`http://localhost:9876/ui-bridge/analytics/health-score?days=${days}`);
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const json = await res.json();
      return json.data;
    },
    staleTime: 30_000,
    refetchInterval: 60_000,
  });

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
