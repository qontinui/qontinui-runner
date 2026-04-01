/**
 * InterventionEffectivenessChart.tsx
 *
 * Stacked bar chart showing intervention action effectiveness.
 */

import { BarChart, Bar, XAxis, YAxis, Tooltip, ResponsiveContainer, Legend } from "recharts";
import { Shield } from "lucide-react";
import { useInterventionEffectiveness } from "../../hooks/useGraphAnalytics";

interface InterventionEffectivenessChartProps {
  days?: number;
}

export function InterventionEffectivenessChart({ days = 7 }: InterventionEffectivenessChartProps) {
  const { data, isLoading, error } = useInterventionEffectiveness(days);

  const chartData = (data ?? []).map((d) => ({
    action: d.intervention_action,
    successful: d.successful,
    failed: d.total - d.successful,
    rate: Math.round(d.success_rate * 100),
  }));

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <Shield className="w-4 h-4" />
        Intervention Effectiveness
      </h3>

      {isLoading && <div className="text-center text-muted-foreground py-8">Loading...</div>}
      {error && <div className="text-center text-destructive py-8">{String(error)}</div>}

      {chartData.length === 0 && !isLoading && (
        <div className="text-center text-muted-foreground py-8">No intervention data</div>
      )}

      {chartData.length > 0 && (
        <div className="h-48">
          <ResponsiveContainer width="100%" height="100%">
            <BarChart data={chartData}>
              <XAxis dataKey="action" tick={{ fontSize: 10 }} />
              <YAxis tick={{ fontSize: 11 }} />
              <Tooltip />
              <Legend />
              <Bar dataKey="successful" stackId="a" fill="hsl(var(--chart-2))" name="Resolved" />
              <Bar
                dataKey="failed"
                stackId="a"
                fill="hsl(var(--chart-5))"
                name="Unresolved"
                radius={[4, 4, 0, 0]}
              />
            </BarChart>
          </ResponsiveContainer>
        </div>
      )}
    </div>
  );
}
