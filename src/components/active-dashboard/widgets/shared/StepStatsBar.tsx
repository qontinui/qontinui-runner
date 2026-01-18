/**
 * StepStatsBar Component
 *
 * Displays statistics bar for step execution.
 * Shows elapsed time, completed count, success rate, etc.
 */

import { Clock, CheckCircle2, XCircle, Activity } from "lucide-react";
import { cn } from "../../../../lib/utils";
import { getStatusColors } from "@/design-system";
import type { StepStats } from "./types";

interface StepStatsBarProps {
  stats: StepStats;
  /** Additional CSS classes */
  className?: string;
  /** Compact mode for summary views */
  compact?: boolean;
}

/**
 * Format elapsed time as "Xm Ys" string.
 */
function formatTime(seconds: number): string {
  const mins = Math.floor(seconds / 60);
  const secs = seconds % 60;
  if (mins > 0) {
    return `${mins}m ${secs}s`;
  }
  return `${secs}s`;
}

/**
 * Statistics bar component for step widgets.
 */
export function StepStatsBar({ stats, className, compact = false }: StepStatsBarProps) {
  const successColors = getStatusColors("success");
  const errorColors = getStatusColors("error");

  if (compact) {
    return (
      <div className={cn("flex flex-wrap items-center gap-x-4 gap-y-1", className)}>
        {/* Total/Completed */}
        <div className="flex items-center gap-1.5">
          <Activity className="h-3.5 w-3.5 text-muted-foreground" />
          <span className="text-sm font-medium text-foreground">
            {stats.completed}/{stats.total}
          </span>
          <span className="text-xs text-muted-foreground">steps</span>
        </div>

        {/* Success Rate */}
        <div className="flex items-center gap-1.5">
          <CheckCircle2 className={cn("h-3.5 w-3.5", successColors.text)} />
          <span className={cn("text-sm font-medium", successColors.text)}>
            {stats.successRate.toFixed(0)}%
          </span>
        </div>

        {/* Failed Count (only if > 0) */}
        {stats.failed > 0 && (
          <div className="flex items-center gap-1.5">
            <XCircle className={cn("h-3.5 w-3.5", errorColors.text)} />
            <span className={cn("text-sm font-medium", errorColors.text)}>{stats.failed}</span>
            <span className="text-xs text-muted-foreground">failed</span>
          </div>
        )}
      </div>
    );
  }

  return (
    <div
      className={cn(
        "flex items-center gap-6 border-b border-border px-4 py-3 bg-muted/20",
        className,
      )}
    >
      {/* Elapsed Time */}
      <div className="flex items-center gap-2">
        <Clock className="h-4 w-4 text-muted-foreground" />
        <div>
          <p className="text-xs text-muted-foreground">Elapsed</p>
          <p className="font-mono text-sm text-foreground">{formatTime(stats.elapsedTime)}</p>
        </div>
      </div>

      {/* Steps Completed */}
      <div className="flex items-center gap-2">
        <Activity className="h-4 w-4 text-muted-foreground" />
        <div>
          <p className="text-xs text-muted-foreground">Steps</p>
          <p className="font-mono text-sm text-foreground">
            {stats.completed} / {stats.total}
          </p>
        </div>
      </div>

      {/* Success Rate */}
      <div className="flex items-center gap-2">
        <CheckCircle2 className={cn("h-4 w-4", successColors.text)} />
        <div>
          <p className="text-xs text-muted-foreground">Success Rate</p>
          <p className={cn("font-mono text-sm", successColors.text)}>
            {stats.successRate.toFixed(1)}%
          </p>
        </div>
      </div>

      {/* Failed Count */}
      {stats.failed > 0 && (
        <div className="flex items-center gap-2">
          <XCircle className={cn("h-4 w-4", errorColors.text)} />
          <div>
            <p className="text-xs text-muted-foreground">Failed</p>
            <p className={cn("font-mono text-sm", errorColors.text)}>{stats.failed}</p>
          </div>
        </div>
      )}
    </div>
  );
}

export default StepStatsBar;
