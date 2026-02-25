/**
 * SuccessRateTrend.tsx
 *
 * Line chart showing success rate over time.
 */

import {
  LineChart,
  Line,
  XAxis,
  YAxis,
  Tooltip,
  ResponsiveContainer,
  ReferenceLine,
} from "recharts";
import { TrendingUp } from "lucide-react";
import type { TimeSeriesDataPoint, TimeRange } from "../../types/performance-metrics";
import { formatChartTimestamp } from "../../types/performance-metrics";

interface SuccessRateTrendProps {
  /** Time series data points */
  data: TimeSeriesDataPoint[];
  /** Current time range for formatting */
  timeRange: TimeRange;
}

interface ChartData {
  timestamp: string;
  displayTime: string;
  value: number;
  count: number;
}

function CustomTooltip({
  active,
  payload,
}: {
  active?: boolean;
  payload?: { payload: ChartData }[];
}) {
  if (active && payload && payload.length) {
    const data = payload[0].payload;
    return (
      <div className="bg-popover border border-border rounded-lg p-3 shadow-lg">
        <p className="font-semibold text-sm">{data.displayTime}</p>
        <div className="text-xs text-muted-foreground mt-1">
          <p>
            Success rate: <span className="font-medium">{data.value.toFixed(1)}%</span>
          </p>
          <p>Runs: {data.count}</p>
        </div>
      </div>
    );
  }
  return null;
}

export function SuccessRateTrend({ data, timeRange }: SuccessRateTrendProps) {
  // Transform data for chart
  const chartData: ChartData[] = data.map((point) => ({
    timestamp: point.timestamp,
    displayTime: formatChartTimestamp(point.timestamp, timeRange),
    value: point.value * 100, // Convert to percentage
    count: point.count || 0,
  }));

  if (chartData.length === 0) {
    return (
      <div className="bg-card rounded-lg border border-border p-4">
        <h3 className="font-semibold mb-4 flex items-center gap-2">
          <TrendingUp className="w-4 h-4" />
          Success Rate Trend
        </h3>
        <div className="text-center text-muted-foreground py-8">
          <p>No trend data available</p>
          <p className="text-xs mt-1">Run some workflows to see trends</p>
        </div>
      </div>
    );
  }

  // Calculate average for reference line
  const avgValue =
    chartData.length > 0 ? chartData.reduce((sum, d) => sum + d.value, 0) / chartData.length : 0;

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <TrendingUp className="w-4 h-4" />
        Success Rate Trend
        {chartData.length > 0 && (
          <span className="text-xs text-muted-foreground font-normal ml-2">
            Avg: {avgValue.toFixed(1)}%
          </span>
        )}
      </h3>

      <div className="h-48">
        <ResponsiveContainer width="100%" height="100%">
          <LineChart data={chartData} margin={{ left: 0, right: 10, top: 10, bottom: 0 }}>
            <XAxis
              dataKey="displayTime"
              tick={{ fontSize: 10, fill: "hsl(var(--muted-foreground))" }}
              tickLine={false}
              axisLine={{ stroke: "hsl(var(--border))" }}
            />
            <YAxis
              domain={[0, 100]}
              tick={{ fontSize: 10, fill: "hsl(var(--muted-foreground))" }}
              tickFormatter={(value) => `${value}%`}
              tickLine={false}
              axisLine={{ stroke: "hsl(var(--border))" }}
              width={40}
            />
            <Tooltip content={<CustomTooltip />} />
            <ReferenceLine
              y={avgValue}
              stroke="hsl(var(--muted-foreground))"
              strokeDasharray="3 3"
              strokeOpacity={0.5}
            />
            <ReferenceLine
              y={80}
              stroke="#eab308"
              strokeDasharray="3 3"
              strokeOpacity={0.3}
              label={{ value: "80%", position: "right", fontSize: 9, fill: "#eab308" }}
            />
            <Line
              type="monotone"
              dataKey="value"
              stroke="#22c55e"
              strokeWidth={2}
              dot={{ r: 3, fill: "#22c55e" }}
              activeDot={{ r: 5, fill: "#22c55e" }}
            />
          </LineChart>
        </ResponsiveContainer>
      </div>
    </div>
  );
}
