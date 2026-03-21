/**
 * TransitionReliabilityTable.tsx
 *
 * Table showing state transition reliability metrics.
 */

import { ArrowRightLeft, AlertTriangle, CheckCircle } from "lucide-react";
import type { StateTransitionMetrics } from "../../types/performance-metrics";
import { formatDurationMs, formatSuccessRate } from "../../types/performance-metrics";

interface TransitionReliabilityTableProps {
  /** Transition metrics to display */
  data: StateTransitionMetrics[];
  /** Maximum number of items to show (0 = all) */
  maxItems?: number;
}

export function TransitionReliabilityTable({
  data,
  maxItems = 0,
}: TransitionReliabilityTableProps) {
  // Sort by flakiness and success rate
  const sortedData = [...data].sort((a, b) => {
    // Flaky items first
    if (a.is_flaky && !b.is_flaky) return -1;
    if (!a.is_flaky && b.is_flaky) return 1;
    // Then by success rate (lowest first)
    return a.success_rate - b.success_rate;
  });

  const displayData = maxItems > 0 ? sortedData.slice(0, maxItems) : sortedData;

  if (displayData.length === 0) {
    return (
      <div className="bg-card rounded-lg border border-border p-4">
        <h3 className="font-semibold mb-4 flex items-center gap-2">
          <ArrowRightLeft className="w-4 h-4" />
          Transition Reliability
        </h3>
        <div className="text-center text-muted-foreground py-8">
          <p>No transition data available</p>
          <p className="text-xs mt-1">State transitions will appear here</p>
        </div>
      </div>
    );
  }

  const getStatusIcon = (metrics: StateTransitionMetrics) => {
    if (metrics.is_flaky) {
      return <AlertTriangle className="w-4 h-4 text-orange-500" />;
    }
    if (metrics.success_rate >= 0.95) {
      return <CheckCircle className="w-4 h-4 text-green-500" />;
    }
    if (metrics.success_rate >= 0.8) {
      return <CheckCircle className="w-4 h-4 text-yellow-500" />;
    }
    return <AlertTriangle className="w-4 h-4 text-red-500" />;
  };

  const getSuccessRateColor = (rate: number, isFlaky: boolean) => {
    if (isFlaky) return "text-orange-500";
    if (rate >= 0.95) return "text-green-500";
    if (rate >= 0.8) return "text-yellow-500";
    if (rate >= 0.5) return "text-orange-500";
    return "text-red-500";
  };

  const flakyCount = displayData.filter((d) => d.is_flaky).length;

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <ArrowRightLeft className="w-4 h-4" />
        Transition Reliability
        {flakyCount > 0 && (
          <span className="text-xs bg-orange-500/10 text-orange-500 px-2 py-0.5 rounded-full ml-2">
            {flakyCount} flaky
          </span>
        )}
      </h3>

      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            <tr className="border-b border-border text-left">
              <th className="pb-2 font-medium text-muted-foreground">Status</th>
              <th className="pb-2 font-medium text-muted-foreground">Transition</th>
              <th className="pb-2 font-medium text-muted-foreground text-right">Success</th>
              <th className="pb-2 font-medium text-muted-foreground text-right">Attempts</th>
              <th className="pb-2 font-medium text-muted-foreground text-right">Avg Time</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border">
            {displayData.map((metrics, idx) => (
              <tr
                key={`${metrics.from_state}-${metrics.to_state}-${idx}`}
                className={`hover:bg-muted/30 ${metrics.is_flaky ? "bg-orange-500/5" : ""}`}
              >
                <td className="py-2">{getStatusIcon(metrics)}</td>
                <td className="py-2">
                  <div className="flex items-center gap-1">
                    <span className="font-medium truncate max-w-24" title={metrics.from_state}>
                      {metrics.from_state}
                    </span>
                    <ArrowRightLeft className="w-3 h-3 text-muted-foreground shrink-0" />
                    <span className="font-medium truncate max-w-24" title={metrics.to_state}>
                      {metrics.to_state}
                    </span>
                    {metrics.is_flaky && (
                      <span className="text-xs bg-orange-500/20 text-orange-500 px-1 rounded ml-1">
                        flaky
                      </span>
                    )}
                  </div>
                </td>
                <td
                  className={`py-2 text-right font-medium ${getSuccessRateColor(metrics.success_rate, metrics.is_flaky)}`}
                >
                  {formatSuccessRate(metrics.success_rate)}
                </td>
                <td className="py-2 text-right text-muted-foreground">{metrics.total_attempts}</td>
                <td className="py-2 text-right text-muted-foreground">
                  {formatDurationMs(metrics.avg_duration_ms)}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      {maxItems > 0 && data.length > maxItems && (
        <p className="text-xs text-muted-foreground mt-2 text-center">
          Showing {maxItems} of {data.length} transitions
        </p>
      )}
    </div>
  );
}
