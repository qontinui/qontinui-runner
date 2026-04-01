import { AlertTriangle, CheckCircle, Shield, XCircle } from "lucide-react";
import { useFlakyElements } from "../../hooks/useGraphAnalytics";
import type { ElementReliability } from "../../hooks/useGraphAnalytics";

function ReliabilityBadge({ rate }: { rate: number }) {
  if (rate >= 0.95)
    return (
      <span className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-xs font-medium bg-green-500/10 text-green-400">
        <CheckCircle className="w-3 h-3" />
        Stable
      </span>
    );
  if (rate >= 0.8)
    return (
      <span className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-xs font-medium bg-yellow-500/10 text-yellow-500">
        <AlertTriangle className="w-3 h-3" />
        Flaky
      </span>
    );
  return (
    <span className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-xs font-medium bg-red-500/10 text-red-500">
      <XCircle className="w-3 h-3" />
      Unreliable
    </span>
  );
}

function ElementRow({ element }: { element: ElementReliability }) {
  const pct = Math.round(element.success_rate * 100);
  return (
    <div className="flex items-center justify-between py-2 px-1">
      <div className="flex-1 min-w-0">
        <p className="text-sm font-medium text-foreground truncate">
          {element.element_id || "Unknown"}
        </p>
        <p className="text-xs text-muted-foreground">{element.total_interactions} interactions</p>
      </div>
      <div className="flex items-center gap-3">
        <div className="text-right">
          <p className="text-sm font-mono">{pct}%</p>
          <p className="text-xs text-muted-foreground">success</p>
        </div>
        <ReliabilityBadge rate={element.success_rate} />
      </div>
    </div>
  );
}

export default function ElementReliabilityDashboard() {
  const { data: elements, isLoading, error } = useFlakyElements();

  return (
    <div className="h-full flex flex-col overflow-hidden p-4 space-y-4">
      <div className="flex items-center gap-2">
        <Shield className="w-5 h-5 text-blue-400" />
        <h2 className="text-lg font-semibold">Element Reliability</h2>
      </div>

      <div className="flex-1 overflow-y-auto space-y-4">
        {isLoading && <p className="text-sm text-muted-foreground">Loading element data...</p>}
        {error && <p className="text-sm text-red-400">Failed to load element data</p>}
        {!isLoading && !error && (!elements || elements.length === 0) && (
          <div className="text-center py-8 text-muted-foreground">
            <Shield className="w-8 h-8 mx-auto mb-2 opacity-50" />
            <p className="text-sm">No element reliability data yet</p>
            <p className="text-xs mt-1">Run some automations to build up element history</p>
          </div>
        )}
        {elements && elements.length > 0 && (
          <>
            <div className="grid grid-cols-3 gap-3">
              <div className="bg-card rounded-lg border border-border p-3 text-center">
                <p className="text-2xl font-bold text-green-400">
                  {elements.filter((e) => e.success_rate >= 0.95).length}
                </p>
                <p className="text-xs text-muted-foreground">Stable</p>
              </div>
              <div className="bg-card rounded-lg border border-border p-3 text-center">
                <p className="text-2xl font-bold text-yellow-500">
                  {elements.filter((e) => e.success_rate >= 0.8 && e.success_rate < 0.95).length}
                </p>
                <p className="text-xs text-muted-foreground">Flaky</p>
              </div>
              <div className="bg-card rounded-lg border border-border p-3 text-center">
                <p className="text-2xl font-bold text-red-500">
                  {elements.filter((e) => e.success_rate < 0.8).length}
                </p>
                <p className="text-xs text-muted-foreground">Unreliable</p>
              </div>
            </div>

            <div className="bg-card rounded-lg border border-border">
              <div className="p-3 border-b border-border">
                <h3 className="text-sm font-medium">Elements by Reliability</h3>
              </div>
              <div className="divide-y divide-border px-3">
                {elements
                  .sort((a, b) => a.success_rate - b.success_rate)
                  .map((el, i) => (
                    <ElementRow key={el.element_id || i} element={el} />
                  ))}
              </div>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
