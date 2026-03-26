/**
 * CostPerInteractionChart.tsx
 *
 * Bar chart showing cost per successful UI Bridge interaction per task run.
 */

import { BarChart, Bar, XAxis, YAxis, Tooltip, ResponsiveContainer } from "recharts";
import { DollarSign } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import { getApiBase } from "../../lib/runner-api";

interface CostPerInteractionRow {
  task_run_id: string;
  total_cost_cents: number;
  successful_interactions: number;
  cost_per_interaction_cents: number;
}

interface CostPerInteractionChartProps {
  days?: number;
}

export function CostPerInteractionChart({ days = 7 }: CostPerInteractionChartProps) {
  const { data = [], isLoading } = useQuery<CostPerInteractionRow[]>({
    queryKey: ["llm-analytics", "cost-per-interaction", days],
    queryFn: async () => {
      const res = await fetch(`${getApiBase()}/analytics/token-usage/cost-per-interaction?days=${days}`);
      if (!res.ok) return [];
      const json = await res.json();
      return json.data ?? [];
    },
    staleTime: 30_000,
  });

  const chartData = data.slice(0, 20).map((r) => ({
    run: r.task_run_id.slice(0, 8),
    costPerAction: r.cost_per_interaction_cents / 100,
    interactions: r.successful_interactions,
  }));

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <DollarSign className="w-4 h-4" />
        Cost per Successful Interaction
      </h3>

      {isLoading && <div className="text-center text-muted-foreground py-8">Loading...</div>}

      {chartData.length === 0 && !isLoading && (
        <div className="text-center text-muted-foreground py-8">No interaction cost data</div>
      )}

      {chartData.length > 0 && (
        <div className="h-48">
          <ResponsiveContainer width="100%" height="100%">
            <BarChart data={chartData}>
              <XAxis dataKey="run" tick={{ fontSize: 10 }} />
              <YAxis tick={{ fontSize: 11 }} tickFormatter={(v) => `$${v.toFixed(2)}`} />
              <Tooltip formatter={(v: number) => [`$${v.toFixed(3)}`, "Cost/Action"]} />
              <Bar dataKey="costPerAction" fill="hsl(var(--chart-1))" radius={[4, 4, 0, 0]} maxBarSize={30} />
            </BarChart>
          </ResponsiveContainer>
        </div>
      )}
    </div>
  );
}
