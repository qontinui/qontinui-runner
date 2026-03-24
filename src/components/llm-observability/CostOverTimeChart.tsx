/**
 * CostOverTimeChart.tsx
 *
 * Line chart showing daily LLM cost over time.
 */

import {
  LineChart,
  Line,
  XAxis,
  YAxis,
  Tooltip,
  ResponsiveContainer,
  CartesianGrid,
} from "recharts";
import { TrendingUp } from "lucide-react";
import type { DailyCostRow } from "./types";

interface CostOverTimeChartProps {
  data: DailyCostRow[];
}

interface ChartData {
  date: string;
  cost: number;
  calls: number;
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
        <p className="font-semibold text-sm">{d.date}</p>
        <div className="text-xs text-muted-foreground mt-1 space-y-1">
          <p>Cost: ${d.cost.toFixed(2)}</p>
          <p>Calls: {d.calls.toLocaleString()}</p>
          <p>Input tokens: {d.inputTokens.toLocaleString()}</p>
          <p>Output tokens: {d.outputTokens.toLocaleString()}</p>
        </div>
      </div>
    );
  }
  return null;
}

export function CostOverTimeChart({ data }: CostOverTimeChartProps) {
  const chartData: ChartData[] = data.map((row) => ({
    date: row.date,
    cost: row.total_cost_cents / 100,
    calls: row.call_count,
    inputTokens: row.total_input_tokens,
    outputTokens: row.total_output_tokens,
  }));

  if (chartData.length === 0) {
    return (
      <div className="bg-card rounded-lg border border-border p-4">
        <h3 className="font-semibold mb-4 flex items-center gap-2">
          <TrendingUp className="w-4 h-4" />
          Cost Over Time
        </h3>
        <div className="text-center text-muted-foreground py-8">
          <p>No daily cost data available</p>
          <p className="text-xs mt-1">Run some workflows to start tracking costs</p>
        </div>
      </div>
    );
  }

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <TrendingUp className="w-4 h-4" />
        Cost Over Time
      </h3>

      <div className="h-64">
        <ResponsiveContainer width="100%" height="100%">
          <LineChart data={chartData} margin={{ left: 10, right: 20, top: 5, bottom: 5 }}>
            <CartesianGrid strokeDasharray="3 3" stroke="hsl(var(--border))" opacity={0.3} />
            <XAxis
              dataKey="date"
              tick={{ fontSize: 11, fill: "hsl(var(--muted-foreground))" }}
              tickFormatter={(value) => {
                const parts = value.split("-");
                return `${parts[1]}/${parts[2]}`;
              }}
            />
            <YAxis
              tick={{ fontSize: 11, fill: "hsl(var(--muted-foreground))" }}
              tickFormatter={(value) => `$${value.toFixed(2)}`}
            />
            <Tooltip content={<CustomTooltip />} />
            <Line
              type="monotone"
              dataKey="cost"
              stroke="hsl(var(--primary))"
              strokeWidth={2}
              dot={{ r: 3 }}
              activeDot={{ r: 5 }}
            />
          </LineChart>
        </ResponsiveContainer>
      </div>
    </div>
  );
}
