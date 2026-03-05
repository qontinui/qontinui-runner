/**
 * Dashboard-compatible summary widget wrapper for TraceViewerSummary.
 * Conforms to the WidgetConfig SummaryComponent interface (BaseWidgetProps & { data: TData }).
 */

import React from "react";
import type { BaseWidgetProps } from "../../../../types/dashboard/widget-props";
import type { TraceViewerWidgetData } from "./useTraceViewerData";
import { formatDuration } from "./trace-utils";

interface TraceViewerDashboardSummaryProps extends BaseWidgetProps {
  data: TraceViewerWidgetData;
}

export const TraceViewerDashboardSummary: React.FC<TraceViewerDashboardSummaryProps> = ({
  data,
}) => {
  if (data.spans.length === 0) {
    return <div className="text-zinc-500 text-xs">No trace data</div>;
  }

  return (
    <div className="flex items-center gap-3 text-xs">
      <span className="text-zinc-400">{data.insights.spanCount} spans</span>
      <span className="text-zinc-400">{formatDuration(data.insights.totalDurationMs)}</span>
      {data.insights.errorCount > 0 && (
        <span className="text-red-400">{data.insights.errorCount} errors</span>
      )}
    </div>
  );
};
