/**
 * CostTrendChart.tsx
 *
 * Daily cost data points with a 7-day moving average trend line overlay.
 * Shows overall stats: total spend, average daily cost, and trend direction.
 */

import {
  LineChart,
  Line,
  XAxis,
  YAxis,
  Tooltip,
  ResponsiveContainer,
  CartesianGrid,
  Legend,
} from "recharts";
import { TrendingUp, TrendingDown, Minus, RefreshCw } from "lucide-react";
import { useCostTrends } from "../../hooks/useCostTrends";
import type { CostTrendChartPoint } from "../../hooks/useCostTrends";

// ---------------------------------------------------------------------------
// Tooltip
// ---------------------------------------------------------------------------

function CustomTooltip({
  active,
  payload,
}: {
  active?: boolean;
  payload?: { payload: CostTrendChartPoint; dataKey: string; color: string }[];
}) {
  if (active && payload && payload.length) {
    const d = payload[0].payload;
    return (
      <div className="bg-popover border border-border rounded-lg p-3 shadow-lg">
        <p className="font-semibold text-sm">{d.date}</p>
        <div className="text-xs text-muted-foreground mt-1 space-y-1">
          <p>Daily cost: ${d.cost.toFixed(2)}</p>
          {d.movingAvg !== null && <p>7-day avg: ${d.movingAvg.toFixed(2)}</p>}
          <p>Calls: {d.calls.toLocaleString()}</p>
          <p>Input tokens: {d.inputTokens.toLocaleString()}</p>
          <p>Output tokens: {d.outputTokens.toLocaleString()}</p>
        </div>
      </div>
    );
  }
  return null;
}

// ---------------------------------------------------------------------------
// Trend icon helper
// ---------------------------------------------------------------------------

function TrendIcon({ direction }: { direction: "up" | "down" | "flat" }) {
  switch (direction) {
    case "up":
      return <TrendingUp className="w-4 h-4 text-red-500" />;
    case "down":
      return <TrendingDown className="w-4 h-4 text-green-500" />;
    case "flat":
      return <Minus className="w-4 h-4 text-muted-foreground" />;
  }
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function CostTrendChart() {
  const { data, stats, loading, error, refetch } = useCostTrends();

  if (loading) {
    return (
      <div className="bg-card rounded-lg border border-border p-4">
        <h3 className="font-semibold mb-4 flex items-center gap-2">
          <TrendingUp className="w-4 h-4" />
          Cost Trends
        </h3>
        <div className="flex items-center justify-center py-12">
          <RefreshCw className="w-5 h-5 animate-spin text-muted-foreground" />
        </div>
      </div>
    );
  }

  if (error) {
    return (
      <div className="bg-card rounded-lg border border-border p-4">
        <h3 className="font-semibold mb-4 flex items-center gap-2">
          <TrendingUp className="w-4 h-4" />
          Cost Trends
        </h3>
        <div className="text-center text-muted-foreground py-8">
          <p className="text-sm">Failed to load cost trends</p>
          <button onClick={() => refetch()} className="mt-2 text-xs underline hover:no-underline">
            Retry
          </button>
        </div>
      </div>
    );
  }

  if (data.length === 0) {
    return (
      <div className="bg-card rounded-lg border border-border p-4">
        <h3 className="font-semibold mb-4 flex items-center gap-2">
          <TrendingUp className="w-4 h-4" />
          Cost Trends
        </h3>
        <div className="text-center text-muted-foreground py-8">
          <p>No cost trend data available</p>
          <p className="text-xs mt-1">Run some workflows to start tracking cost trends</p>
        </div>
      </div>
    );
  }

  return (
    <div
      className="bg-card rounded-lg border border-border p-4"
      data-tutorial-id="llm-cost-trend-chart"
    >
      <div className="flex items-center justify-between mb-4">
        <h3 className="font-semibold flex items-center gap-2">
          <TrendingUp className="w-4 h-4" />
          Cost Trends
        </h3>
        <div className="flex items-center gap-4 text-xs text-muted-foreground">
          <span>
            Total: <strong className="text-foreground">${stats.totalSpend.toFixed(2)}</strong>
          </span>
          <span>
            Avg/day: <strong className="text-foreground">${stats.avgDailyCost.toFixed(2)}</strong>
          </span>
          <span className="flex items-center gap-1">
            Trend: <TrendIcon direction={stats.trendDirection} />
          </span>
        </div>
      </div>

      <div className="h-64">
        <ResponsiveContainer width="100%" height="100%">
          <LineChart data={data} margin={{ left: 10, right: 20, top: 5, bottom: 5 }}>
            <CartesianGrid strokeDasharray="3 3" stroke="hsl(var(--border))" opacity={0.3} />
            <XAxis
              dataKey="date"
              tick={{ fontSize: 11, fill: "hsl(var(--muted-foreground))" }}
              tickFormatter={(value: string) => {
                const parts = value.split("-");
                return `${parts[1]}/${parts[2]}`;
              }}
            />
            <YAxis
              tick={{ fontSize: 11, fill: "hsl(var(--muted-foreground))" }}
              tickFormatter={(value: number) => `$${value.toFixed(2)}`}
            />
            <Tooltip content={<CustomTooltip />} />
            <Legend verticalAlign="top" height={28} wrapperStyle={{ fontSize: 11 }} />
            <Line
              name="Daily Cost"
              type="monotone"
              dataKey="cost"
              stroke="hsl(var(--primary))"
              strokeWidth={1.5}
              dot={{ r: 2 }}
              activeDot={{ r: 4 }}
            />
            <Line
              name="7-Day Average"
              type="monotone"
              dataKey="movingAvg"
              stroke="hsl(var(--chart-2, 220 70% 50%))"
              strokeWidth={2}
              strokeDasharray="5 3"
              dot={false}
              connectNulls={false}
            />
          </LineChart>
        </ResponsiveContainer>
      </div>
    </div>
  );
}
