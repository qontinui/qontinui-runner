/**
 * CostByModelChart.tsx
 *
 * Horizontal bar chart showing cost breakdown by LLM model.
 */

import { useMemo } from "react";
import { BarChart, Bar, XAxis, YAxis, Tooltip, ResponsiveContainer } from "recharts";
import { Cpu } from "lucide-react";
import type { ModelCostRow } from "./types";

interface CostByModelChartProps {
  data: ModelCostRow[];
}

interface ChartData {
  model: string;
  cost: number;
  calls: number;
  inputTokens: number;
  outputTokens: number;
  avgDuration: number | null;
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
        <p className="font-semibold text-sm">{d.model}</p>
        <div className="text-xs text-muted-foreground mt-1 space-y-1">
          <p>Cost: ${d.cost.toFixed(2)}</p>
          <p>Calls: {d.calls.toLocaleString()}</p>
          <p>Input tokens: {d.inputTokens.toLocaleString()}</p>
          <p>Output tokens: {d.outputTokens.toLocaleString()}</p>
          {d.avgDuration != null && <p>Avg duration: {Math.round(d.avgDuration)}ms</p>}
        </div>
      </div>
    );
  }
  return null;
}

export function CostByModelChart({ data }: CostByModelChartProps) {
  const chartData: ChartData[] = useMemo(
    () =>
      data
        .map((row) => ({
          model: row.model_used,
          cost: row.total_cost_cents / 100,
          calls: row.call_count,
          inputTokens: row.total_input_tokens,
          outputTokens: row.total_output_tokens,
          avgDuration: row.avg_duration_ms,
        }))
        .sort((a, b) => b.cost - a.cost),
    [data],
  );

  if (chartData.length === 0) {
    return (
      <div className="bg-card rounded-lg border border-border p-4">
        <h3 className="font-semibold mb-4 flex items-center gap-2">
          <Cpu className="w-4 h-4" />
          Cost by Model
        </h3>
        <div className="text-center text-muted-foreground py-8">
          <p>No model cost data available</p>
        </div>
      </div>
    );
  }

  const maxLabelLen = Math.max(...chartData.map((d) => d.model.length));
  const leftMargin = Math.min(Math.max(maxLabelLen * 6, 80), 160);

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <Cpu className="w-4 h-4" />
        Cost by Model
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
              dataKey="model"
              tick={{ fontSize: 11, fill: "hsl(var(--foreground))" }}
              width={leftMargin - 10}
            />
            <Tooltip content={<CustomTooltip />} />
            <Bar dataKey="cost" fill="hsl(var(--primary))" radius={[0, 4, 4, 0]} maxBarSize={20} />
          </BarChart>
        </ResponsiveContainer>
      </div>
    </div>
  );
}
