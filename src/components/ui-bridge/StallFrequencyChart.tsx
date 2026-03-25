/**
 * StallFrequencyChart.tsx
 *
 * Bar chart showing stall event frequency grouped by pattern type.
 */

import { BarChart, Bar, XAxis, YAxis, Tooltip, ResponsiveContainer } from "recharts";
import { PauseCircle } from "lucide-react";
import { useStallFrequency } from "../../hooks/useGraphAnalytics";

interface StallFrequencyChartProps {
  days?: number;
}

export function StallFrequencyChart({ days = 7 }: StallFrequencyChartProps) {
  const { data, isLoading, error } = useStallFrequency(days);

  const chartData = (data ?? []).map((d) => ({
    type: d.pattern_type.replace(/([A-Z])/g, " $1").trim(),
    count: d.count,
  }));

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <PauseCircle className="w-4 h-4" />
        Stall Frequency by Type
      </h3>

      {isLoading && <div className="text-center text-muted-foreground py-8">Loading...</div>}
      {error && <div className="text-center text-destructive py-8">{String(error)}</div>}

      {chartData.length === 0 && !isLoading && (
        <div className="text-center text-muted-foreground py-8">No stall events recorded</div>
      )}

      {chartData.length > 0 && (
        <div className="h-48">
          <ResponsiveContainer width="100%" height="100%">
            <BarChart data={chartData}>
              <XAxis dataKey="type" tick={{ fontSize: 10 }} />
              <YAxis tick={{ fontSize: 11 }} />
              <Tooltip />
              <Bar dataKey="count" fill="hsl(var(--chart-4))" radius={[4, 4, 0, 0]} maxBarSize={40} />
            </BarChart>
          </ResponsiveContainer>
        </div>
      )}
    </div>
  );
}
