/**
 * ActionBaselineTable.tsx
 *
 * Table showing per-action-type performance baselines (latency + success rate).
 */

import { Timer } from "lucide-react";
import { useActionBaselines } from "../../hooks/useGraphAnalytics";

interface ActionBaselineTableProps {
  days?: number;
}

export function ActionBaselineTable({ days = 7 }: ActionBaselineTableProps) {
  const { data, isLoading, error } = useActionBaselines(days);

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <Timer className="w-4 h-4" />
        Action Performance Baselines
      </h3>

      {isLoading && <div className="text-center text-muted-foreground py-8">Loading...</div>}
      {error && <div className="text-center text-destructive py-8">{String(error)}</div>}

      {data && data.length === 0 && (
        <div className="text-center text-muted-foreground py-8">No action data available</div>
      )}

      {data && data.length > 0 && (
        <div className="overflow-x-auto">
          <table className="w-full text-left text-sm">
            <thead>
              <tr className="border-b border-border text-xs text-muted-foreground">
                <th className="py-2 px-3 font-medium">Action</th>
                <th className="py-2 px-3 font-medium text-right">Count</th>
                <th className="py-2 px-3 font-medium text-right">Avg (ms)</th>
                <th className="py-2 px-3 font-medium text-right">Min (ms)</th>
                <th className="py-2 px-3 font-medium text-right">Max (ms)</th>
                <th className="py-2 px-3 font-medium text-right">Success</th>
              </tr>
            </thead>
            <tbody>
              {data.map((row) => (
                <tr
                  key={row.action}
                  className="border-b border-border last:border-0 hover:bg-muted/50"
                >
                  <td className="py-2 px-3 font-mono">{row.action}</td>
                  <td className="py-2 px-3 text-right tabular-nums">{row.count}</td>
                  <td className="py-2 px-3 text-right tabular-nums">
                    {row.avg_duration_ms != null ? Math.round(row.avg_duration_ms) : "-"}
                  </td>
                  <td className="py-2 px-3 text-right tabular-nums">
                    {row.min_duration_ms != null ? Math.round(row.min_duration_ms) : "-"}
                  </td>
                  <td className="py-2 px-3 text-right tabular-nums">
                    {row.max_duration_ms != null ? Math.round(row.max_duration_ms) : "-"}
                  </td>
                  <td className="py-2 px-3 text-right">
                    <span
                      className={`tabular-nums ${row.success_rate >= 0.95 ? "text-green-500" : row.success_rate >= 0.8 ? "text-yellow-500" : "text-red-500"}`}
                    >
                      {Math.round(row.success_rate * 100)}%
                    </span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
