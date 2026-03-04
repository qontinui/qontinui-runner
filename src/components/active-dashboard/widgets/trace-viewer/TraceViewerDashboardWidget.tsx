/**
 * Dashboard-compatible full widget wrapper for TraceViewerWidget.
 * Conforms to the WidgetConfig FullComponent interface (BaseWidgetProps & { data: TData }).
 */

import React from "react";
import type { BaseWidgetProps } from "../../../../types/dashboard/widget-props";
import type { TraceViewerWidgetData } from "./useTraceViewerData";
import { TraceViewerWidget } from "./TraceViewerWidget";

interface TraceViewerDashboardWidgetProps extends BaseWidgetProps {
  data: TraceViewerWidgetData;
}

export const TraceViewerDashboardWidget: React.FC<TraceViewerDashboardWidgetProps> = ({
  data,
}) => {
  return (
    <div className="h-full overflow-hidden">
      <TraceViewerWidget
        executionId={data.executionId}
        spans={data.spans}
        isLoading={data.isLoading}
        error={data.error}
        height={400}
      />
    </div>
  );
};
