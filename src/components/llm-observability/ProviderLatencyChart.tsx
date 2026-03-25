/**
 * ProviderLatencyChart.tsx
 *
 * Bar chart showing average, min, and max latency per LLM provider.
 */

import { BarChart, Bar, XAxis, YAxis, Tooltip, ResponsiveContainer, Legend } from "recharts";
import { Clock } from "lucide-react";
import type { ProviderLatencyRow } from "./types";

interface ProviderLatencyChartProps {
  data: ProviderLatencyRow[];
}

interface ChartData {
  provider: string;
  avg: number;
  min: number;
  max: number;
  calls: number;
}

function CustomTooltip({
  active,
  payload,
}: {
  active?: boolean;
  payload?: { payload: ChartData; name: string; color: string }[];
}) {
  if (active && payload && payload.length) {
    const d = payload[0].payload;
    return (
      <div className="bg-popover border border-border rounded-lg p-3 shadow-lg">
        <p className="font-semibold text-sm">{d.provider}</p>
        <div className="text-xs text-muted-foreground mt-1 space-y-1">
          <p>Avg: {Math.round(d.avg)}ms</p>
          <p>Min: {Math.round(d.min)}ms</p>
          <p>Max: {Math.round(d.max)}ms</p>
          <p>Calls: {d.calls.toLocaleString()}</p>
        </div>
      </div>
    );
  }
  return null;
}

export function ProviderLatencyChart({ data }: ProviderLatencyChartProps) {
  const chartData: ChartData[] = data.map((row) => ({
    provider: row.provider_used,
    avg: row.avg_duration_ms,
    min: row.min_duration_ms,
    max: row.max_duration_ms,
    calls: row.call_count,
  }));

  if (chartData.length === 0) {
    return (
      <div className="bg-card rounded-lg border border-border p-4">
        <h3 className="font-semibold mb-4 flex items-center gap-2">
          <Clock className="w-4 h-4" />
          Provider Latency
        </h3>
        <div className="text-center text-muted-foreground py-8">
          <p>No latency data available</p>
        </div>
      </div>
    );
  }

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <Clock className="w-4 h-4" />
        Provider Latency
      </h3>

      <div className="h-64">
        <ResponsiveContainer width="100%" height="100%">
          <BarChart data={chartData} margin={{ left: 10, right: 20, top: 5, bottom: 5 }}>
            <XAxis dataKey="provider" tick={{ fontSize: 11, fill: "hsl(var(--foreground))" }} />
            <YAxis
              tick={{ fontSize: 11, fill: "hsl(var(--muted-foreground))" }}
              tickFormatter={(value) => `${value}ms`}
            />
            <Tooltip content={<CustomTooltip />} />
            <Legend wrapperStyle={{ fontSize: 11 }} />
            <Bar
              dataKey="avg"
              name="Avg"
              fill="hsl(var(--primary))"
              radius={[4, 4, 0, 0]}
              maxBarSize={24}
            />
            <Bar dataKey="min" name="Min" fill="hsl(var(--accent))" radius={[4, 4, 0, 0]} maxBarSize={24} />
            <Bar dataKey="max" name="Max" fill="hsl(var(--secondary))" radius={[4, 4, 0, 0]} maxBarSize={24} />
          </BarChart>
        </ResponsiveContainer>
      </div>
    </div>
  );
}
