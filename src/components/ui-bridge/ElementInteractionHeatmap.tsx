/**
 * ElementInteractionHeatmap.tsx
 *
 * Shows element interaction frequency and success rates as a heatmap-style
 * table. Highlights flaky elements in red, reliable ones in green.
 */

import { MousePointerClick } from "lucide-react";
import { useFlakyElements, type ElementReliability } from "../../hooks/useGraphAnalytics";

function reliabilityColor(rate: number): string {
  if (rate >= 0.95) return "bg-green-500/20 text-green-700 dark:text-green-400";
  if (rate >= 0.8) return "bg-yellow-500/20 text-yellow-700 dark:text-yellow-400";
  return "bg-red-500/20 text-red-700 dark:text-red-400";
}

function SuccessBar({ rate }: { rate: number }) {
  const pct = Math.round(rate * 100);
  return (
    <div className="flex items-center gap-2">
      <div className="flex-1 h-2 bg-muted rounded-full overflow-hidden">
        <div
          className={`h-full rounded-full ${
            rate >= 0.95 ? "bg-green-500" : rate >= 0.8 ? "bg-yellow-500" : "bg-red-500"
          }`}
          style={{ width: `${pct}%` }}
        />
      </div>
      <span className="text-xs tabular-nums w-10 text-right">{pct}%</span>
    </div>
  );
}

function ElementRow({ element }: { element: ElementReliability }) {
  return (
    <tr className="border-b border-border last:border-0 hover:bg-muted/50">
      <td className="py-2 px-3">
        <div className="flex items-center gap-2">
          <span className="font-mono text-sm truncate max-w-[200px]">{element.element_id}</span>
          {element.flaky && (
            <span className="text-xs px-1.5 py-0.5 rounded bg-red-500/10 text-red-500 shrink-0">
              flaky
            </span>
          )}
        </div>
      </td>
      <td className="py-2 px-3 text-right tabular-nums text-sm">
        {element.total_interactions}
      </td>
      <td className="py-2 px-3 w-40">
        <SuccessBar rate={element.success_rate} />
      </td>
      <td className="py-2 px-3">
        <span className={`text-xs px-2 py-1 rounded ${reliabilityColor(element.success_rate)}`}>
          {element.recommended_confidence.toFixed(2)}
        </span>
      </td>
      <td className="py-2 px-3 text-xs text-muted-foreground truncate max-w-[200px]">
        {element.last_failure_reason ?? "-"}
      </td>
    </tr>
  );
}

export function ElementInteractionHeatmap() {
  const { data: elements, isLoading, error } = useFlakyElements();

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <MousePointerClick className="w-4 h-4" />
        Element Reliability
      </h3>

      {isLoading && (
        <div className="text-center text-muted-foreground py-8">
          Loading element data...
        </div>
      )}

      {error && (
        <div className="text-center text-destructive py-8">
          Failed to load element data: {String(error)}
        </div>
      )}

      {elements && elements.length === 0 && (
        <div className="text-center text-muted-foreground py-8">
          No flaky elements detected (all elements have &gt;90% success rate)
        </div>
      )}

      {elements && elements.length > 0 && (
        <div className="overflow-x-auto">
          <table className="w-full text-left">
            <thead>
              <tr className="border-b border-border text-xs text-muted-foreground">
                <th className="py-2 px-3 font-medium">Element</th>
                <th className="py-2 px-3 font-medium text-right">Interactions</th>
                <th className="py-2 px-3 font-medium">Success Rate</th>
                <th className="py-2 px-3 font-medium">Confidence</th>
                <th className="py-2 px-3 font-medium">Last Failure</th>
              </tr>
            </thead>
            <tbody>
              {elements.map((el) => (
                <ElementRow key={el.element_id} element={el} />
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
