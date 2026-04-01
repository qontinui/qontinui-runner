/**
 * CostByPhaseChart.tsx
 *
 * Horizontal bar chart showing cost breakdown by workflow phase.
 */

import { useMemo } from "react";
import { BarChart, Bar, XAxis, YAxis, Tooltip, ResponsiveContainer } from "recharts";
import { Layers } from "lucide-react";
import type { PhaseCostRow } from "./types";

interface CostByPhaseChartProps {
  data: PhaseCostRow[];
}

interface ChartData {
  phase: string;
  cost: number;
  inputTokens: number;
  outputTokens: number;
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
          <p>Cost: ${d.cost.toFixed(2)}</p>
          <p>Input tokens: {d.inputTokens.toLocaleString()}</p>
          <p>Output tokens: {d.outputTokens.toLocaleString()}</p>
        </div>
      </div>
    );
  }
  return null;
}

export function CostByPhaseChart({ data }: CostByPhaseChartProps) {
  const chartData: ChartData[] = useMemo(
    () =>
      data
        .map((row) => ({
          phase: row.phase,
          cost: row.total_cost_cents / 100,
          inputTokens: row.total_input_tokens,
          outputTokens: row.total_output_tokens,
        }))
        .sort((a, b) => b.cost - a.cost),
    [data],
  );

  if (chartData.length === 0) {
    return (
      <div className="bg-card rounded-lg border border-border p-4">
        <h3 className="font-semibold mb-4 flex items-center gap-2">
          <Layers className="w-4 h-4" />
          Cost by Phase
        </h3>
        <div className="text-center text-muted-foreground py-8">
          <p>No phase cost data available</p>
        </div>
      </div>
    );
  }

  const maxLabelLen = Math.max(...chartData.map((d) => d.phase.length));
  const leftMargin = Math.min(Math.max(maxLabelLen * 6, 60), 140);

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <Layers className="w-4 h-4" />
        Cost by Phase
      </h3>

      <div className="h-64">
        <ResponsiveContainer width="100%" height="100%">
          <BarChart data={chartData} layout="vertical" margin={{ left: leftMargin, right: 20 }}>
            <XAxis
              type="number"
              tick={{ fontSize: 11, fill: "hsl(var(--muted-foreground))" }}
              tickFormatter={(value) => `$${value.toFixed(2)}`}
            />
            <YAxis
              type="category"
              dataKey="phase"
              tick={{ fontSize: 11, fill: "hsl(var(--foreground))" }}
              width={leftMargin - 10}
            />
            <Tooltip content={<CustomTooltip />} />
            <Bar
              dataKey="cost"
              fill="hsl(var(--secondary))"
              radius={[0, 4, 4, 0]}
              maxBarSize={20}
            />
          </BarChart>
        </ResponsiveContainer>
      </div>
    </div>
  );
}
