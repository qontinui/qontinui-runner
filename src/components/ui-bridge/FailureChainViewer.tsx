/**
 * FailureChainViewer.tsx
 *
 * Vertical flow diagram showing the causal chain behind a UI element's failures.
 * Element -> TaskRun -> Workflow/Finding chain.
 */

import { GitMerge, ChevronRight } from "lucide-react";
import { useQuery } from "@tanstack/react-query";

interface GraphPathNode {
  key: string;
  kind: string;
  label: string;
  weight: number;
}

interface GraphPathEdge {
  kind: string;
  label: string | null;
  weight: number;
}

interface GraphPath {
  nodes: GraphPathNode[];
  edges: GraphPathEdge[];
  total_weight: number;
}

interface FailureChainViewerProps {
  elementId: string;
}

const KIND_COLORS: Record<string, string> = {
  ui_element: "bg-blue-500",
  task_run: "bg-purple-500",
  workflow: "bg-green-500",
  finding: "bg-orange-500",
  fix: "bg-cyan-500",
  rule: "bg-yellow-500",
  error: "bg-red-500",
};

function NodeBadge({ node }: { node: GraphPathNode }) {
  const color = KIND_COLORS[node.kind] ?? "bg-muted-foreground";
  return (
    <div className="flex items-center gap-2 px-3 py-2 bg-muted rounded-md">
      <div className={`w-2.5 h-2.5 rounded-full ${color} shrink-0`} />
      <div className="min-w-0">
        <span className="text-xs text-muted-foreground">{node.kind}</span>
        <p className="text-sm font-mono truncate">{node.label || node.key}</p>
      </div>
    </div>
  );
}

export function FailureChainViewer({ elementId }: FailureChainViewerProps) {
  const { data, isLoading, error } = useQuery<GraphPath[]>({
    queryKey: ["graph-analytics", "ui-failure-chain", elementId],
    queryFn: async () => {
      const res = await fetch(
        `http://localhost:9876/graph/ui-failure-chain?element_id=${encodeURIComponent(elementId)}`,
      );
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const json = await res.json();
      return json.data ?? [];
    },
    staleTime: 30_000,
    enabled: !!elementId,
  });

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <GitMerge className="w-4 h-4" />
        Failure Chain: <span className="font-mono text-sm">{elementId}</span>
      </h3>

      {isLoading && (
        <div className="text-center text-muted-foreground py-8">Tracing failure chain...</div>
      )}
      {error && <div className="text-center text-destructive py-8">{String(error)}</div>}

      {data && data.length === 0 && (
        <div className="text-center text-muted-foreground py-8">
          No failure chains found for this element
        </div>
      )}

      {data && data.length > 0 && (
        <div className="space-y-4 max-h-96 overflow-y-auto">
          {data.map((path, i) => (
            <div key={i} className="border border-border rounded-md p-3">
              <div className="flex items-center gap-1 flex-wrap">
                {path.nodes.map((node, j) => (
                  <div key={j} className="flex items-center gap-1">
                    <NodeBadge node={node} />
                    {j < path.nodes.length - 1 && (
                      <div className="flex items-center gap-0.5 text-muted-foreground">
                        <ChevronRight className="w-3 h-3" />
                        {path.edges[j] && <span className="text-[10px]">{path.edges[j].kind}</span>}
                        <ChevronRight className="w-3 h-3" />
                      </div>
                    )}
                  </div>
                ))}
              </div>
            </div>
          ))}
        </div>
      )}

      {/* Legend */}
      {data && data.length > 0 && (
        <div className="flex flex-wrap gap-3 mt-3 pt-3 border-t border-border">
          {Object.entries(KIND_COLORS).map(([kind, color]) => (
            <div key={kind} className="flex items-center gap-1.5 text-xs text-muted-foreground">
              <div className={`w-2 h-2 rounded-full ${color}`} />
              {kind}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
