/**
 * ActionTimingChart.tsx
 *
 * Bar chart showing action execution times with percentile data.
 */

import { BarChart, Bar, XAxis, YAxis, Tooltip, ResponsiveContainer, Cell } from "recharts";
import { Timer } from "lucide-react";
import type { ActionPerformanceMetrics } from "../../types/performance-metrics";
import { formatDurationMs } from "../../types/performance-metrics";

interface ActionTimingChartProps {
  /** Action performance metrics to display */
  data: ActionPerformanceMetrics[];
  /** Maximum number of items to show */
  maxItems?: number;
}

interface ChartData {
  name: string;
  avg: number;
  p50: number;
  p95: number;
  p99: number;
  successRate: number;
  total: number;
}

export function ActionTimingChart({ data, maxItems = 10 }: ActionTimingChartProps) {
  // Transform and limit data
  const chartData: ChartData[] = data.slice(0, maxItems).map((item) => ({
    name: item.action_type,
    avg: Math.round(item.avg_duration_ms),
    p50: item.p50_duration_ms,
    p95: item.p95_duration_ms,
    p99: item.p99_duration_ms,
    successRate: item.success_rate,
    total: item.total_executions,
  }));

  if (chartData.length === 0) {
    return (
      <div className="bg-card rounded-lg border border-border p-4">
        <h3 className="font-semibold mb-4 flex items-center gap-2">
          <Timer className="w-4 h-4" />
          Action Timing
        </h3>
        <div className="text-center text-muted-foreground py-8">
          <p>No action timing data available</p>
          <p className="text-xs mt-1">Run some workflows to collect metrics</p>
        </div>
      </div>
    );
  }

  // Get bar color based on success rate
  const getBarColor = (successRate: number) => {
    if (successRate >= 0.95) return "#22c55e"; // green-500
    if (successRate >= 0.8) return "#eab308"; // yellow-500
    if (successRate >= 0.5) return "#f97316"; // orange-500
    return "#ef4444"; // red-500
  };

  const CustomTooltip = ({
    active,
    payload,
  }: {
    active?: boolean;
    payload?: { payload: ChartData }[];
  }) => {
    if (active && payload && payload.length) {
      const data = payload[0].payload;
      return (
        <div className="bg-popover border border-border rounded-lg p-3 shadow-lg">
          <p className="font-semibold text-sm">{data.name}</p>
          <div className="text-xs text-muted-foreground mt-1 space-y-1">
            <p>Total executions: {data.total}</p>
            <p>Success rate: {(data.successRate * 100).toFixed(1)}%</p>
            <div className="border-t border-border pt-1 mt-1">
              <p>Average: {formatDurationMs(data.avg)}</p>
              <p>P50 (median): {formatDurationMs(data.p50)}</p>
              <p>P95: {formatDurationMs(data.p95)}</p>
              <p>P99: {formatDurationMs(data.p99)}</p>
            </div>
          </div>
        </div>
      );
    }
    return null;
  };

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <Timer className="w-4 h-4" />
        Action Timing
        <span className="text-xs text-muted-foreground font-normal ml-2">
          (top {chartData.length} by volume)
        </span>
      </h3>

      <div className="h-64">
        <ResponsiveContainer width="100%" height="100%">
          <BarChart data={chartData} layout="vertical" margin={{ left: 60, right: 20 }}>
            <XAxis
              type="number"
              tickFormatter={(value) => formatDurationMs(value)}
              tick={{ fontSize: 11, fill: "hsl(var(--muted-foreground))" }}
            />
            <YAxis
              type="category"
              dataKey="name"
              tick={{ fontSize: 11, fill: "hsl(var(--foreground))" }}
              width={55}
            />
            <Tooltip content={<CustomTooltip />} />
            <Bar dataKey="avg" radius={[0, 4, 4, 0]} maxBarSize={20}>
              {chartData.map((entry, index) => (
                <Cell key={`cell-${index}`} fill={getBarColor(entry.successRate)} />
              ))}
            </Bar>
          </BarChart>
        </ResponsiveContainer>
      </div>

      <div className="flex justify-center gap-4 mt-2 text-xs">
        <div className="flex items-center gap-1">
          <div className="w-3 h-3 rounded bg-green-500" />
          <span className="text-muted-foreground">&gt;95%</span>
        </div>
        <div className="flex items-center gap-1">
          <div className="w-3 h-3 rounded bg-yellow-500" />
          <span className="text-muted-foreground">80-95%</span>
        </div>
        <div className="flex items-center gap-1">
          <div className="w-3 h-3 rounded bg-orange-500" />
          <span className="text-muted-foreground">50-80%</span>
        </div>
        <div className="flex items-center gap-1">
          <div className="w-3 h-3 rounded bg-red-500" />
          <span className="text-muted-foreground">&lt;50%</span>
        </div>
      </div>
    </div>
  );
}
