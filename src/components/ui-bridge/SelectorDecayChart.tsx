/**
 * SelectorDecayChart.tsx
 *
 * Line chart showing element success rate over time windows (decay curve).
 */

import { LineChart, Line, XAxis, YAxis, Tooltip, ResponsiveContainer, ReferenceLine } from "recharts";
import { TrendingDown } from "lucide-react";
import { useDecayCurve } from "../../hooks/useGraphAnalytics";

interface SelectorDecayChartProps {
  elementId: string;
  windowMs?: number;
  windows?: number;
}

export function SelectorDecayChart({ elementId, windowMs = 86_400_000, windows = 7 }: SelectorDecayChartProps) {
  const { data, isLoading, error } = useDecayCurve(elementId, windowMs, windows);

  const chartData = (data ?? [])
    .slice()
    .reverse()
    .map((b, i) => ({
      window: `W${i + 1}`,
      rate: Math.round(b.success_rate * 100),
      total: b.total,
    }));

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <TrendingDown className="w-4 h-4" />
        Selector Decay: <span className="font-mono text-sm">{elementId}</span>
      </h3>

      {isLoading && <div className="text-center text-muted-foreground py-8">Loading...</div>}
      {error && <div className="text-center text-destructive py-8">{String(error)}</div>}

      {chartData.length > 0 && (
        <div className="h-48">
          <ResponsiveContainer width="100%" height="100%">
            <LineChart data={chartData}>
              <XAxis dataKey="window" tick={{ fontSize: 11 }} />
              <YAxis domain={[0, 100]} tick={{ fontSize: 11 }} tickFormatter={(v) => `${v}%`} />
              <ReferenceLine y={95} stroke="hsl(var(--muted-foreground))" strokeDasharray="3 3" />
              <Tooltip formatter={(v) => [`${Number(v)}%`, "Success Rate"]} />
              <Line type="monotone" dataKey="rate" stroke="hsl(var(--primary))" strokeWidth={2} dot />
            </LineChart>
          </ResponsiveContainer>
        </div>
      )}

      {chartData.length === 0 && !isLoading && (
        <div className="text-center text-muted-foreground py-8">No interaction data for this element</div>
      )}
    </div>
  );
}
