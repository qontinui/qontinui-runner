/**
 * PhaseBreakdownChart.tsx
 *
 * Horizontal bar chart showing cost per workflow phase with token breakdown.
 * Follows the CostByPhaseChart pattern from llm-observability.
 */

import { useMemo } from "react";
import {
  BarChart,
  Bar,
  XAxis,
  YAxis,
  Tooltip,
  ResponsiveContainer,
  Legend,
} from "recharts";
import { Layers } from "lucide-react";
import type { PhaseCostBreakdown } from "./types";

interface PhaseBreakdownChartProps {
  data: PhaseCostBreakdown[];
}

interface ChartData {
  phase: string;
  cost: number;
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
}

function formatTokenCount(count: number): string {
  if (count >= 1_000_000) return `${(count / 1_000_000).toFixed(1)}M`;
  if (count >= 1_000) return `${(count / 1_000).toFixed(1)}K`;
  return count.toLocaleString();
}

function CustomTooltip({
  active,
  payload,
}: {
  active?: boolean;
  payload?: { payload: ChartData }[];
}) {
  if (active && payload && payload.length) {
    const d = payload[0].payload;
    return (
      <div className="bg-popover border border-border rounded-lg p-3 shadow-lg">
        <p className="font-semibold text-sm">{d.phase}</p>
        <div className="text-xs text-muted-foreground mt-1 space-y-1">
          <p>Cost: ${d.cost.toFixed(4)}</p>
          <p>Input: {formatTokenCount(d.inputTokens)}</p>
          <p>Output: {formatTokenCount(d.outputTokens)}</p>
          <p>Cache created: {formatTokenCount(d.cacheCreationTokens)}</p>
          <p>Cache read: {formatTokenCount(d.cacheReadTokens)}</p>
        </div>
      </div>
    );
  }
  return null;
}

export function PhaseBreakdownChart({ data }: PhaseBreakdownChartProps) {
  const chartData: ChartData[] = useMemo(
    () =>
      data
        .map((row) => ({
          phase: row.phase,
          cost: row.cost_usd,
          inputTokens: row.input_tokens,
          outputTokens: row.output_tokens,
          cacheCreationTokens: row.cache_creation_tokens,
          cacheReadTokens: row.cache_read_tokens,
        }))
        .sort((a, b) => b.cost - a.cost),
    [data],
  );

  if (chartData.length === 0) {
    return (
      <div className="bg-card rounded-lg border border-border/50 p-4">
        <h3 className="text-sm font-semibold flex items-center gap-2 mb-3">
          <Layers className="w-4 h-4" />
          Cost by Phase
        </h3>
        <div className="text-center text-muted-foreground py-8">
          <p className="text-sm">No phase cost data available</p>
        </div>
      </div>
    );
  }

  const maxLabelLen = Math.max(...chartData.map((d) => d.phase.length));
  const leftMargin = Math.min(Math.max(maxLabelLen * 6, 60), 140);

  return (
    <div className="bg-card rounded-lg border border-border/50 p-4">
      <h3 className="text-sm font-semibold flex items-center gap-2 mb-3">
        <Layers className="w-4 h-4" />
        Cost by Phase
      </h3>

      <div className="h-64">
        <ResponsiveContainer width="100%" height="100%">
          <BarChart
            data={chartData}
            layout="vertical"
            margin={{ left: leftMargin, right: 20 }}
          >
            <XAxis
              type="number"
              tick={{ fontSize: 11, fill: "hsl(var(--muted-foreground))" }}
              tickFormatter={(value: number) => `$${value.toFixed(2)}`}
            />
            <YAxis
              type="category"
              dataKey="phase"
              tick={{ fontSize: 11, fill: "hsl(var(--foreground))" }}
              width={leftMargin - 10}
            />
            <Tooltip content={<CustomTooltip />} />
            <Legend
              wrapperStyle={{ fontSize: 11 }}
              formatter={(value: string) =>
                value === "cost" ? "Cost (USD)" : value
              }
            />
            <Bar
              dataKey="cost"
              name="Cost (USD)"
              fill="hsl(var(--chart-1))"
              radius={[0, 4, 4, 0]}
              maxBarSize={20}
            />
          </BarChart>
        </ResponsiveContainer>
      </div>
    </div>
  );
}
