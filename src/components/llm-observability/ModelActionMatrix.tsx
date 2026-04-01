/**
 * ModelActionMatrix.tsx
 *
 * Heatmap-style table showing model × action type success rates and costs.
 */

import { Grid3X3 } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import { getApiBase } from "../../lib/runner-api";

interface ModelActionRow {
  model: string;
  action: string;
  total: number;
  success_rate: number;
  avg_cost_cents: number;
}

interface ModelActionMatrixProps {
  days?: number;
}

function cellColor(rate: number): string {
  if (rate >= 0.95) return "bg-green-500/20";
  if (rate >= 0.8) return "bg-yellow-500/20";
  if (rate >= 0.5) return "bg-orange-500/20";
  return "bg-red-500/20";
}

export function ModelActionMatrix({ days = 7 }: ModelActionMatrixProps) {
  const { data = [], isLoading } = useQuery<ModelActionRow[]>({
    queryKey: ["llm-analytics", "model-action-matrix", days],
    queryFn: async () => {
      const res = await fetch(
        `${getApiBase()}/analytics/token-usage/model-action-matrix?days=${days}`,
      );
      if (!res.ok) return [];
      const json = await res.json();
      return json.data ?? [];
    },
    staleTime: 30_000,
  });

  // Build matrix: models as rows, actions as columns
  const models = [...new Set(data.map((r) => r.model))];
  const actions = [...new Set(data.map((r) => r.action))];
  const lookup = new Map(data.map((r) => [`${r.model}:${r.action}`, r]));

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <Grid3X3 className="w-4 h-4" />
        Model x Action Success Matrix
      </h3>

      {isLoading && <div className="text-center text-muted-foreground py-8">Loading...</div>}

      {data.length === 0 && !isLoading && (
        <div className="text-center text-muted-foreground py-8">No model-action data</div>
      )}

      {models.length > 0 && actions.length > 0 && (
        <div className="overflow-x-auto">
          <table className="w-full text-xs">
            <thead>
              <tr className="border-b border-border">
                <th className="py-2 px-2 text-left font-medium text-muted-foreground">Model</th>
                {actions.map((a) => (
                  <th
                    key={a}
                    className="py-2 px-2 text-center font-medium text-muted-foreground font-mono"
                  >
                    {a}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {models.map((model) => (
                <tr key={model} className="border-b border-border last:border-0">
                  <td className="py-2 px-2 font-mono truncate max-w-[150px]">{model}</td>
                  {actions.map((action) => {
                    const cell = lookup.get(`${model}:${action}`);
                    if (!cell) {
                      return (
                        <td key={action} className="py-2 px-2 text-center text-muted-foreground">
                          -
                        </td>
                      );
                    }
                    return (
                      <td
                        key={action}
                        className={`py-2 px-2 text-center ${cellColor(cell.success_rate)}`}
                      >
                        <div className="tabular-nums font-semibold">
                          {Math.round(cell.success_rate * 100)}%
                        </div>
                        <div className="text-muted-foreground">{cell.total}x</div>
                      </td>
                    );
                  })}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
