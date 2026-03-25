/**
 * FailureTaxonomyPanel.tsx
 *
 * Grouped display of failure clusters by error message, with affected elements.
 */

import { AlertCircle } from "lucide-react";
import { useFailureTaxonomy } from "../../hooks/useGraphAnalytics";

interface FailureTaxonomyPanelProps {
  days?: number;
}

export function FailureTaxonomyPanel({ days = 7 }: FailureTaxonomyPanelProps) {
  const { data, isLoading, error } = useFailureTaxonomy(days);

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <AlertCircle className="w-4 h-4" />
        Failure Taxonomy
      </h3>

      {isLoading && <div className="text-center text-muted-foreground py-8">Loading...</div>}
      {error && <div className="text-center text-destructive py-8">{String(error)}</div>}

      {data && data.length === 0 && (
        <div className="text-center text-muted-foreground py-8">No failures recorded</div>
      )}

      {data && data.length > 0 && (
        <div className="space-y-2 max-h-96 overflow-y-auto">
          {data.map((cluster, i) => {
            const elements = cluster.affected_elements?.split(",").filter(Boolean) ?? [];
            return (
              <div key={i} className="border border-border rounded-md p-3">
                <div className="flex items-start justify-between gap-2">
                  <p className="text-sm font-mono text-destructive flex-1 break-all">
                    {cluster.error_message}
                  </p>
                  <span className="shrink-0 text-sm font-semibold tabular-nums bg-red-500/10 text-red-500 px-2 py-0.5 rounded">
                    {cluster.count}x
                  </span>
                </div>
                {elements.length > 0 && (
                  <div className="flex flex-wrap gap-1 mt-2">
                    {elements.slice(0, 5).map((el) => (
                      <span key={el} className="text-xs px-1.5 py-0.5 rounded bg-muted text-muted-foreground font-mono">
                        {el}
                      </span>
                    ))}
                    {elements.length > 5 && (
                      <span className="text-xs text-muted-foreground">+{elements.length - 5} more</span>
                    )}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
