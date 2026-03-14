/**
 * FlowExecutionSummary
 *
 * Summary widget component for Flow Designer execution.
 * Shows compact status, progress, and key metrics.
 */

import { CheckCircle, XCircle, Clock, Pause, AlertCircle, Loader2, GitBranch } from "lucide-react";
import { cn } from "@/lib/utils";
import type { FlowExecutionSummaryProps } from "./types";

/**
 * Format duration in human-readable format.
 */
function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  const minutes = Math.floor(ms / 60000);
  const seconds = ((ms % 60000) / 1000).toFixed(0);
  return `${minutes}m ${seconds}s`;
}

/**
 * Compact status indicator.
 */
function StatusIndicator({ status }: { status: string }) {
  switch (status) {
    case "running":
      return (
        <div className="flex items-center gap-1.5 text-emerald-400">
          <Loader2 className="w-3.5 h-3.5 animate-spin" />
          <span className="text-xs font-medium">Running</span>
        </div>
      );
    case "completed":
      return (
        <div className="flex items-center gap-1.5 text-green-400">
          <CheckCircle className="w-3.5 h-3.5" />
          <span className="text-xs font-medium">Completed</span>
        </div>
      );
    case "failed":
      return (
        <div className="flex items-center gap-1.5 text-red-400">
          <XCircle className="w-3.5 h-3.5" />
          <span className="text-xs font-medium">Failed</span>
        </div>
      );
    case "paused":
      return (
        <div className="flex items-center gap-1.5 text-blue-400">
          <Pause className="w-3.5 h-3.5" />
          <span className="text-xs font-medium">Paused</span>
        </div>
      );
    case "waiting_for_input":
      return (
        <div className="flex items-center gap-1.5 text-orange-400">
          <AlertCircle className="w-3.5 h-3.5" />
          <span className="text-xs font-medium">Waiting</span>
        </div>
      );
    default:
      return (
        <div className="flex items-center gap-1.5 text-muted-foreground">
          <Clock className="w-3.5 h-3.5" />
          <span className="text-xs font-medium">Idle</span>
        </div>
      );
  }
}

/**
 * Progress bar component with defensive parsing.
 * Handles invalid values gracefully to prevent crashes.
 */
function FlowProgressBar({
  completed,
  total,
  failed,
}: {
  completed: number;
  total: number;
  failed: number;
}) {
  // Defensive parsing: ensure values are valid numbers
  const safeCompleted = Number.isFinite(completed) ? Math.max(0, completed) : 0;
  const safeTotal = Number.isFinite(total) ? Math.max(0, total) : 0;
  const safeFailed = Number.isFinite(failed) ? Math.max(0, failed) : 0;

  if (safeTotal === 0 && safeCompleted === 0) return null;

  // Prevent division issues
  const denominator = Math.max(safeTotal, safeCompleted, 1);
  const successPercent = Math.min(
    100,
    Math.max(0, ((safeCompleted - safeFailed) / denominator) * 100),
  );
  const failedPercent = Math.min(
    100 - successPercent,
    Math.max(0, (safeFailed / denominator) * 100),
  );

  return (
    <div className="w-full h-1.5 bg-muted/50 rounded-full overflow-hidden">
      <div className="h-full flex">
        <div
          className="bg-green-500 transition-all duration-300"
          style={{ width: `${successPercent}%` }}
        />
        <div
          className="bg-red-500 transition-all duration-300"
          style={{ width: `${failedPercent}%` }}
        />
      </div>
    </div>
  );
}

/**
 * Flow Execution Summary Widget.
 */
export function FlowExecutionSummary({ data, className }: FlowExecutionSummaryProps) {
  // No active execution - show empty state
  if (!data.isActive && data.status === "idle") {
    return (
      <div className={cn("flex flex-col gap-2 p-2", className)}>
        <div className="flex items-center gap-2 text-muted-foreground">
          <GitBranch className="w-4 h-4" />
          <span className="text-xs">No active flow</span>
        </div>
      </div>
    );
  }

  const { stats, status, flowName, currentStepId, elapsedMs } = data;

  return (
    <div className={cn("flex flex-col gap-2 p-2", className)}>
      {/* Status and flow name */}
      <div className="flex items-center justify-between">
        <StatusIndicator status={status} />
        {flowName && (
          <span className="text-xs text-muted-foreground truncate max-w-[120px]">{flowName}</span>
        )}
      </div>

      {/* Progress bar */}
      <FlowProgressBar
        completed={stats.completedSteps}
        total={stats.totalSteps || stats.completedSteps}
        failed={stats.failedSteps}
      />

      {/* Stats row */}
      <div className="flex items-center justify-between text-xs">
        <div className="flex items-center gap-3">
          <span className="text-muted-foreground">
            {stats.completedSteps} step{stats.completedSteps !== 1 ? "s" : ""}
          </span>
          {stats.failedSteps > 0 && (
            <span className="text-red-400">{stats.failedSteps} failed</span>
          )}
        </div>
        <span className="text-muted-foreground">{formatDuration(elapsedMs)}</span>
      </div>

      {/* Current step indicator */}
      {currentStepId && status === "running" && (
        <div className="flex items-center gap-1.5 text-xs">
          <Loader2 className="w-3 h-3 animate-spin text-emerald-400" />
          <span className="text-muted-foreground truncate">{currentStepId}</span>
        </div>
      )}

      {/* Error preview */}
      {data.error && (
        <div className="text-xs text-red-400 truncate" title={data.error}>
          {data.error}
        </div>
      )}
    </div>
  );
}
