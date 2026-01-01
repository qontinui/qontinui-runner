/**
 * RunHistoryPanel.tsx
 *
 * Shows recent run history with:
 * - Success/fail timeline
 * - Duration trend
 * - Click to view run details
 */

import { History, CheckCircle2, XCircle, Clock, AlertTriangle, ChevronRight } from "lucide-react";
import type { RunDetails } from "../../types/statistics";

interface RunHistoryPanelProps {
  runs: RunDetails[];
  onRunClick?: (run: RunDetails) => void;
}

export function RunHistoryPanel({ runs, onRunClick }: RunHistoryPanelProps) {
  const formatDuration = (ms: number | undefined) => {
    if (!ms) return "-";
    if (ms < 1000) return `${ms}ms`;
    return `${(ms / 1000).toFixed(1)}s`;
  };

  const formatTime = (iso: string) => {
    const date = new Date(iso);
    return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  };

  const getStatusIcon = (status: string, success?: boolean) => {
    if (status === "completed" && success) {
      return <CheckCircle2 className="w-4 h-4 text-green-500" />;
    }
    if (status === "failed" || success === false) {
      return <XCircle className="w-4 h-4 text-red-500" />;
    }
    if (status === "timeout") {
      return <Clock className="w-4 h-4 text-orange-500" />;
    }
    return <AlertTriangle className="w-4 h-4 text-yellow-500" />;
  };

  // Calculate stats for the header
  const recentRuns = runs.slice(0, 20);
  const successCount = recentRuns.filter((r) => r.success).length;
  const failCount = recentRuns.filter((r) => r.success === false).length;

  if (runs.length === 0) {
    return (
      <div className="bg-card rounded-lg border border-border p-4">
        <h3 className="font-semibold mb-4 flex items-center gap-2">
          <History className="w-4 h-4" />
          Run History
        </h3>
        <div className="text-center text-muted-foreground py-4">
          <p>No runs recorded yet</p>
        </div>
      </div>
    );
  }

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <History className="w-4 h-4" />
        Run History
        <span className="text-xs text-muted-foreground ml-auto">
          Last 20: {successCount} passed, {failCount} failed
        </span>
      </h3>

      {/* Mini bar chart */}
      <div className="flex gap-0.5 mb-4 h-8">
        {[...recentRuns].reverse().map((run) => (
          <div
            key={run.id}
            className={`flex-1 rounded-sm transition-colors ${
              run.success
                ? "bg-green-500 hover:bg-green-400"
                : run.status === "timeout"
                  ? "bg-orange-500 hover:bg-orange-400"
                  : "bg-red-500 hover:bg-red-400"
            }`}
            title={`${run.workflow_name || "Run"}: ${run.status}`}
          />
        ))}
      </div>

      {/* Recent runs list */}
      <div className="space-y-1 max-h-48 overflow-auto">
        {runs.slice(0, 10).map((run) => (
          <button
            key={run.id}
            onClick={() => onRunClick?.(run)}
            className="w-full flex items-center gap-2 p-2 rounded hover:bg-muted/50 transition-colors text-sm text-left"
          >
            {getStatusIcon(run.status, run.success)}
            <span className="flex-1 truncate">{run.workflow_name || "Workflow Run"}</span>
            <span className="text-muted-foreground text-xs">{formatDuration(run.duration_ms)}</span>
            <span className="text-muted-foreground text-xs">{formatTime(run.started_at)}</span>
            {onRunClick && <ChevronRight className="w-4 h-4 text-muted-foreground" />}
          </button>
        ))}
      </div>
    </div>
  );
}
