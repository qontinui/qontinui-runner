/**
 * AutomationRegressionTable.tsx
 *
 * Table showing element+action pairs that degraded between recent and prior runs.
 */

import { ArrowDown } from "lucide-react";
import { useAutomationRegressions } from "../../hooks/useGraphAnalytics";

interface AutomationRegressionTableProps {
  days?: number;
}

export function AutomationRegressionTable({ days = 7 }: AutomationRegressionTableProps) {
  const { data, isLoading, error } = useAutomationRegressions(days);

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <ArrowDown className="w-4 h-4 text-red-500" />
        Automation Regressions
      </h3>

      {isLoading && <div className="text-center text-muted-foreground py-8">Loading...</div>}
      {error && <div className="text-center text-destructive py-8">{String(error)}</div>}

      {data && data.length === 0 && (
        <div className="text-center text-muted-foreground py-8">No regressions detected</div>
      )}

      {data && data.length > 0 && (
        <div className="overflow-x-auto">
          <table className="w-full text-left text-sm">
            <thead>
              <tr className="border-b border-border text-xs text-muted-foreground">
                <th className="py-2 px-3 font-medium">Element</th>
                <th className="py-2 px-3 font-medium">Action</th>
                <th className="py-2 px-3 font-medium text-right">Prior</th>
                <th className="py-2 px-3 font-medium text-right">Recent</th>
                <th className="py-2 px-3 font-medium text-right">Delta</th>
              </tr>
            </thead>
            <tbody>
              {data.map((r, i) => (
                <tr key={i} className="border-b border-border last:border-0 hover:bg-muted/50">
                  <td className="py-2 px-3 font-mono text-sm truncate max-w-[200px]">{r.element_id}</td>
                  <td className="py-2 px-3 font-mono">{r.action}</td>
                  <td className="py-2 px-3 text-right tabular-nums text-green-500">
                    {Math.round(r.prior_success_rate * 100)}%
                  </td>
                  <td className="py-2 px-3 text-right tabular-nums text-red-500">
                    {Math.round(r.recent_success_rate * 100)}%
                  </td>
                  <td className="py-2 px-3 text-right tabular-nums text-red-500 font-semibold">
                    {Math.round(r.delta * 100)}%
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
