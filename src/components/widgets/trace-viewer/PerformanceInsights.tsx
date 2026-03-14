import React from "react";
import type { TraceInsights } from "./types";
import { formatDuration } from "./trace-utils";

interface PerformanceInsightsProps {
  insights: TraceInsights;
}

export const PerformanceInsights: React.FC<PerformanceInsightsProps> = ({ insights }) => {
  if (insights.spanCount === 0) return null;

  return (
    <div className="flex items-center gap-4 px-3 py-1.5 bg-zinc-900 border-t border-zinc-700 text-[11px] text-zinc-500">
      <span>
        Total: <span className="text-zinc-300">{formatDuration(insights.totalDurationMs)}</span>
      </span>
      <span>
        Spans: <span className="text-zinc-300">{insights.spanCount}</span>
      </span>
      {insights.errorCount > 0 && (
        <span>
          Errors: <span className="text-red-400">{insights.errorCount}</span>
        </span>
      )}
      {insights.slowestSpan && (
        <span>
          Slowest:{" "}
          <span className="text-zinc-300">
            {insights.slowestSpan.name.replace(/^qontinui\./, "")} (
            {formatDuration(insights.slowestSpan.durationMs)})
          </span>
        </span>
      )}
      {/* Phase breakdown */}
      <span className="ml-auto flex gap-2">
        {Object.entries(insights.phaseBreakdown)
          .sort(([, a], [, b]) => b - a)
          .slice(0, 3)
          .map(([phase, ms]) => (
            <span key={phase}>
              {phase}: {formatDuration(ms)}
            </span>
          ))}
      </span>
    </div>
  );
};
