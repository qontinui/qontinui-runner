import React from "react";
import { useTraceViewerData } from "./useTraceViewerData";
import { computeInsights, formatDuration } from "./trace-utils";

interface TraceViewerSummaryProps {
  executionId: string | null;
}

export const TraceViewerSummary: React.FC<TraceViewerSummaryProps> = ({ executionId }) => {
  const { data: spans = [] } = useTraceViewerData(executionId);
  const insights = computeInsights(spans);

  if (spans.length === 0) {
    return <div className="text-zinc-500 text-xs">No trace data</div>;
  }

  return (
    <div className="flex items-center gap-3 text-xs">
      <span className="text-zinc-400">
        {insights.spanCount} spans
      </span>
      <span className="text-zinc-400">
        {formatDuration(insights.totalDurationMs)}
      </span>
      {insights.errorCount > 0 && (
        <span className="text-red-400">
          {insights.errorCount} errors
        </span>
      )}
    </div>
  );
};
