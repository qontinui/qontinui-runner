/**
 * AnnotationGapTable.tsx
 *
 * Table showing high-interaction elements that lack semantic annotations.
 * These are automation risks — frequently used elements with no data-testid.
 */

import { Tag } from "lucide-react";
import { useQuery } from "@tanstack/react-query";

interface AnnotationGap {
  element_id: string;
  interaction_count: number;
  success_rate: number;
  element_type: string | null;
  label: string | null;
}

interface AnnotationGapTableProps {
  minInteractions?: number;
}

export function AnnotationGapTable({ minInteractions = 10 }: AnnotationGapTableProps) {
  const { data = [], isLoading, error } = useQuery<AnnotationGap[]>({
    queryKey: ["graph-analytics", "annotation-gaps", minInteractions],
    queryFn: async () => {
      const res = await fetch(
        `http://localhost:9876/ui-bridge/analytics/annotation-gaps?min_interactions=${minInteractions}`
      );
      if (!res.ok) return [];
      const json = await res.json();
      return json.data ?? [];
    },
    staleTime: 30_000,
    refetchInterval: 60_000,
  });

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <Tag className="w-4 h-4" />
        Annotation Gaps
        <span className="text-xs text-muted-foreground font-normal">
          (elements needing data-testid)
        </span>
      </h3>

      {isLoading && <div className="text-center text-muted-foreground py-8">Loading...</div>}
      {error && <div className="text-center text-destructive py-8">{String(error)}</div>}

      {data.length === 0 && !isLoading && (
        <div className="text-center text-muted-foreground py-8">
          All high-interaction elements have annotations
        </div>
      )}

      {data.length > 0 && (
        <div className="overflow-x-auto max-h-64 overflow-y-auto">
          <table className="w-full text-left text-sm">
            <thead className="sticky top-0 bg-card">
              <tr className="border-b border-border text-xs text-muted-foreground">
                <th className="py-2 px-3 font-medium">Element</th>
                <th className="py-2 px-3 font-medium">Type</th>
                <th className="py-2 px-3 font-medium text-right">Interactions</th>
                <th className="py-2 px-3 font-medium text-right">Success</th>
                <th className="py-2 px-3 font-medium">Label</th>
              </tr>
            </thead>
            <tbody>
              {data.map((gap) => (
                <tr key={gap.element_id} className="border-b border-border last:border-0 hover:bg-muted/50">
                  <td className="py-2 px-3 font-mono text-sm truncate max-w-[200px]">{gap.element_id}</td>
                  <td className="py-2 px-3 text-xs text-muted-foreground">{gap.element_type ?? "-"}</td>
                  <td className="py-2 px-3 text-right tabular-nums font-semibold">{gap.interaction_count}</td>
                  <td className="py-2 px-3 text-right">
                    <span className={`tabular-nums ${gap.success_rate >= 0.95 ? "text-green-500" : gap.success_rate >= 0.8 ? "text-yellow-500" : "text-red-500"}`}>
                      {Math.round(gap.success_rate * 100)}%
                    </span>
                  </td>
                  <td className="py-2 px-3 text-xs text-muted-foreground truncate max-w-[150px]">{gap.label ?? "-"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
