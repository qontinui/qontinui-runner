/**
 * PageComplexityTable.tsx
 *
 * Ranked table showing page complexity by average cost-per-action.
 */

import { FileText } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import { getApiBase } from "../../lib/runner-api";

interface PageComplexityRow {
  target_page_url: string;
  call_count: number;
  total_cost_cents: number;
  avg_cost_per_call_cents: number;
}

interface PageComplexityTableProps {
  days?: number;
}

export function PageComplexityTable({ days = 7 }: PageComplexityTableProps) {
  const { data = [], isLoading } = useQuery<PageComplexityRow[]>({
    queryKey: ["llm-analytics", "page-complexity", days],
    queryFn: async () => {
      const res = await fetch(`${getApiBase()}/analytics/token-usage/page-complexity?days=${days}`);
      if (!res.ok) return [];
      const json = await res.json();
      return json.data ?? [];
    },
    staleTime: 30_000,
  });

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <FileText className="w-4 h-4" />
        Page Complexity (by automation cost)
      </h3>

      {isLoading && <div className="text-center text-muted-foreground py-8">Loading...</div>}

      {data.length === 0 && !isLoading && (
        <div className="text-center text-muted-foreground py-8">No page cost data</div>
      )}

      {data.length > 0 && (
        <div className="overflow-x-auto max-h-64 overflow-y-auto">
          <table className="w-full text-left text-sm">
            <thead className="sticky top-0 bg-card">
              <tr className="border-b border-border text-xs text-muted-foreground">
                <th className="py-2 px-3 font-medium">Page URL</th>
                <th className="py-2 px-3 font-medium text-right">Calls</th>
                <th className="py-2 px-3 font-medium text-right">Total Cost</th>
                <th className="py-2 px-3 font-medium text-right">Avg/Call</th>
              </tr>
            </thead>
            <tbody>
              {data.map((r) => (
                <tr
                  key={r.target_page_url}
                  className="border-b border-border last:border-0 hover:bg-muted/50"
                >
                  <td className="py-2 px-3 font-mono text-xs truncate max-w-[300px]">
                    {r.target_page_url}
                  </td>
                  <td className="py-2 px-3 text-right tabular-nums">{r.call_count}</td>
                  <td className="py-2 px-3 text-right tabular-nums">
                    ${(r.total_cost_cents / 100).toFixed(2)}
                  </td>
                  <td className="py-2 px-3 text-right tabular-nums font-semibold">
                    ${(r.avg_cost_per_call_cents / 100).toFixed(3)}
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
