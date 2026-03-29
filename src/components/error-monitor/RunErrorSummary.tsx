/**
 * RunErrorSummary — Compact inline error count badges for a task run.
 * Designed to embed in TaskRunLivePanel and similar detail views.
 * Clicking navigates to the Error Monitor tab filtered to this run.
 */

import { useCallback } from "react";
import { AlertCircle, AlertTriangle, Bug, ArrowRight } from "lucide-react";
import { cn } from "../../lib/utils";
import { useErrorSummary, ERROR_MONITOR_REFRESH_INTERVAL } from "../../hooks/useErrorMonitor";

interface RunErrorSummaryProps {
  taskRunId: string;
  /** Optional display name shown in the Error Monitor scope indicator */
  taskRunName?: string;
  className?: string;
}

export function RunErrorSummary({ taskRunId, taskRunName, className }: RunErrorSummaryProps) {
  const { summary } = useErrorSummary({ taskRunId, refreshInterval: ERROR_MONITOR_REFRESH_INTERVAL });

  const handleClick = useCallback(() => {
    window.dispatchEvent(
      new CustomEvent("navigate-to-error-monitor", {
        detail: { taskRunId, taskRunName },
      }),
    );
  }, [taskRunId, taskRunName]);

  if (!summary || summary.unresolvedCount === 0) return null;

  return (
    <button
      type="button"
      onClick={handleClick}
      className={cn(
        "flex items-center gap-2 text-xs rounded-md px-1.5 py-1",
        "transition-colors hover:bg-muted/60 cursor-pointer group",
        className,
      )}
      title="View all errors in Error Monitor"
    >
      <span className="text-muted-foreground font-medium">Errors:</span>
      {(summary.criticalCount ?? 0) > 0 && (
        <span className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded bg-red-600/20 text-red-600 border border-red-600/30">
          <AlertCircle className="w-3 h-3" />
          {summary.criticalCount}
        </span>
      )}
      {(summary.errorCount ?? 0) > 0 && (
        <span className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded bg-red-500/20 text-red-500 border border-red-500/30">
          <Bug className="w-3 h-3" />
          {summary.errorCount}
        </span>
      )}
      {(summary.warningCount ?? 0) > 0 && (
        <span className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded bg-yellow-500/20 text-yellow-500 border border-yellow-500/30">
          <AlertTriangle className="w-3 h-3" />
          {summary.warningCount}
        </span>
      )}
      <span className="text-muted-foreground">
        ({summary.unresolvedCount} unresolved)
      </span>
      <ArrowRight className="w-3 h-3 text-muted-foreground opacity-0 group-hover:opacity-100 transition-opacity" />
    </button>
  );
}
