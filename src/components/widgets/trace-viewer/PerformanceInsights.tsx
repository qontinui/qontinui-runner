import React from "react";
import type { TraceInsights, CriticalPathInfo } from "./types";
import { formatDuration, computeTokenInsights } from "./trace-utils";

interface PerformanceInsightsProps {
  insights: TraceInsights;
  criticalPath?: CriticalPathInfo | null;
  tokenInsights?: ReturnType<typeof computeTokenInsights>;
}

export const PerformanceInsights: React.FC<PerformanceInsightsProps> = ({ insights, criticalPath, tokenInsights }) => {
  if (insights.spanCount === 0) return null;

  const parallelSlack =
    criticalPath && insights.totalDurationMs > 0
      ? insights.totalDurationMs - criticalPath.pathDurationMs
      : null;

  return (
    <div className="flex flex-wrap items-center gap-4 px-3 py-1.5 bg-zinc-900 border-t border-zinc-700 text-[11px] text-zinc-500" data-tutorial-id="trace-performance-insights">
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

      {/* Critical path stats */}
      {criticalPath && criticalPath.pathDurationMs > 0 && (
        <>
          <span className="text-zinc-600">|</span>
          <span>
            Critical path:{" "}
            <span className="text-red-300">
              {formatDuration(criticalPath.pathDurationMs)}
              {insights.totalDurationMs > 0 &&
                ` (${((criticalPath.pathDurationMs / insights.totalDurationMs) * 100).toFixed(0)}%)`}
            </span>
          </span>
          {criticalPath.bottleneck && (
            <span>
              Bottleneck:{" "}
              <span className="text-red-300">
                {criticalPath.bottleneck.name.replace(/^qontinui\./, "")} (
                {formatDuration(criticalPath.bottleneck.durationMs)})
              </span>
            </span>
          )}
          {parallelSlack !== null && parallelSlack > 0 && (
            <span>
              Parallel slack: <span className="text-zinc-300">{formatDuration(parallelSlack)}</span>
            </span>
          )}
        </>
      )}

      {/* Token insights */}
      {tokenInsights && tokenInsights.totalInputTokens + tokenInsights.totalOutputTokens > 0 && (
        <>
          <span className="text-zinc-600">|</span>
          <span>
            Tokens: <span className="text-zinc-300">{tokenInsights.totalInputTokens.toLocaleString()}</span>
            <span className="text-zinc-500">&#8593;</span>
            {' '}
            <span className="text-zinc-300">{tokenInsights.totalOutputTokens.toLocaleString()}</span>
            <span className="text-zinc-500">&#8595;</span>
          </span>
          {tokenInsights.totalCostCents > 0 && (
            <span>
              Cost: <span className="text-zinc-300">${(tokenInsights.totalCostCents / 100).toFixed(4)}</span>
            </span>
          )}
        </>
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
