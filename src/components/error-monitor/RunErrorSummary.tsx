/**
 * RunErrorSummary — Compact inline error count badges for a task run.
 * Designed to embed in TaskRunLivePanel and similar detail views.
 */

import { AlertCircle, AlertTriangle, Bug } from "lucide-react";
import { cn } from "../../lib/utils";
import { useErrorSummary } from "../../hooks/useErrorMonitor";

interface RunErrorSummaryProps {
  taskRunId: string;
  className?: string;
}

export function RunErrorSummary({ taskRunId, className }: RunErrorSummaryProps) {
  const { summary } = useErrorSummary({ taskRunId, refreshInterval: 30000 });

  if (!summary || summary.unresolvedCount === 0) return null;

  return (
    <div className={cn("flex items-center gap-2 text-xs", className)}>
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
    </div>
  );
}
