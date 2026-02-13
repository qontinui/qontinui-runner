/**
 * ExecutionStatsCard Component
 *
 * A compact statistics card showing aggregate execution metrics.
 * Displays step counts, success rate, elapsed time, and iteration progress.
 */

import { useState, useEffect, useMemo } from "react";
import {
  Clock,
  CheckCircle2,
  XCircle,
  Activity,
  RefreshCw,
  Loader2,
  TrendingUp,
} from "lucide-react";
import { cn } from "../../lib/utils";
import { Badge } from "../ui";
import type { WorkflowStage } from "../../types/dashboard/activity-types";
import { WORKFLOW_STAGE_CONFIG } from "../../types/dashboard/activity-types";
import { getAccentColors, getStatusColors } from "@/design-system";

/**
 * Props for ExecutionStatsCard.
 */
export interface ExecutionStatsCardProps {
  /** Total number of steps */
  totalSteps: number;
  /** Number of completed steps */
  completedSteps: number;
  /** Number of successful steps */
  successfulSteps: number;
  /** Number of failed steps */
  failedSteps: number;
  /** Current workflow stage */
  currentStage: WorkflowStage | null;
  /** Whether a task is running */
  isRunning: boolean;
  /** Start time (timestamp) for elapsed time calculation */
  startTime?: number | null;
  /** Current iteration (1-based) */
  iteration?: number;
  /** Maximum iterations */
  maxIterations?: number;
  /** Whether to show in compact mode */
  compact?: boolean;
  /** Additional CSS classes */
  className?: string;
}

/**
 * Format elapsed time as "Xh Ym Zs" string.
 */
function formatElapsedTime(startTime: number | null | undefined): string {
  if (!startTime) return "0s";

  const elapsed = Math.floor((Date.now() - startTime) / 1000);
  const hours = Math.floor(elapsed / 3600);
  const mins = Math.floor((elapsed % 3600) / 60);
  const secs = elapsed % 60;

  if (hours > 0) {
    return `${hours}h ${mins}m ${secs}s`;
  }
  if (mins > 0) {
    return `${mins}m ${secs}s`;
  }
  return `${secs}s`;
}

/**
 * ExecutionStatsCard component.
 */
export function ExecutionStatsCard({
  totalSteps,
  completedSteps,
  successfulSteps,
  failedSteps,
  currentStage,
  isRunning,
  startTime,
  iteration,
  maxIterations,
  compact = false,
  className,
}: ExecutionStatsCardProps) {
  const [elapsedDisplay, setElapsedDisplay] = useState(formatElapsedTime(startTime));

  // Update elapsed time every second while running
  useEffect(() => {
    if (!isRunning || !startTime) {
      setElapsedDisplay(formatElapsedTime(startTime));
      return;
    }

    const updateTime = () => setElapsedDisplay(formatElapsedTime(startTime));
    updateTime();
    const interval = setInterval(updateTime, 1000);
    return () => clearInterval(interval);
  }, [isRunning, startTime]);

  // Calculate success rate
  const successRate = useMemo(() => {
    if (completedSteps === 0) return 100;
    return (successfulSteps / completedSteps) * 100;
  }, [completedSteps, successfulSteps]);

  // Calculate progress percentage
  const progressPercent = useMemo(() => {
    if (totalSteps === 0) return 0;
    return (completedSteps / totalSteps) * 100;
  }, [totalSteps, completedSteps]);

  const successColors = getStatusColors("success");
  const errorColors = getStatusColors("error");
  const pendingColors = getStatusColors("pending");
  const stageConfig = currentStage ? WORKFLOW_STAGE_CONFIG[currentStage] : null;
  const stageColors = stageConfig
    ? getAccentColors(stageConfig.color as Parameters<typeof getAccentColors>[0])
    : null;

  // Compact mode for inline display
  if (compact) {
    return (
      <div className={cn("flex items-center gap-4 text-xs", className)}>
        {/* Steps progress */}
        <div className="flex items-center gap-1.5">
          <Activity className="h-3.5 w-3.5 text-muted-foreground" />
          <span data-content-role="metric" data-content-label="step progress" className="font-mono">
            {completedSteps}/{totalSteps}
          </span>
        </div>

        {/* Success rate */}
        <div className="flex items-center gap-1.5">
          <CheckCircle2 className={cn("h-3.5 w-3.5", successColors.text)} />
          <span
            data-content-role="metric"
            data-content-label="success rate"
            className={cn("font-mono", successColors.text)}
          >
            {successRate.toFixed(0)}%
          </span>
        </div>

        {/* Failed count (if any) */}
        {failedSteps > 0 && (
          <div className="flex items-center gap-1.5">
            <XCircle className={cn("h-3.5 w-3.5", errorColors.text)} />
            <span
              data-content-role="metric"
              data-content-label="failed steps"
              className={cn("font-mono", errorColors.text)}
            >
              {failedSteps}
            </span>
          </div>
        )}

        {/* Elapsed time */}
        <div className="flex items-center gap-1.5">
          <Clock className="h-3.5 w-3.5 text-muted-foreground" />
          <span data-content-role="metric" data-content-label="elapsed time" className="font-mono">
            {elapsedDisplay}
          </span>
        </div>

        {/* Iteration */}
        {iteration !== undefined && maxIterations !== undefined && maxIterations > 1 && (
          <div className="flex items-center gap-1.5">
            <RefreshCw className="h-3.5 w-3.5 text-muted-foreground" />
            <span
              data-content-role="metric"
              data-content-label="iteration progress"
              className="font-mono"
            >
              {iteration}/{maxIterations}
            </span>
          </div>
        )}
      </div>
    );
  }

  // Full card view
  return (
    <div
      className={cn("bg-card border border-border rounded-lg shadow-sm overflow-hidden", className)}
    >
      {/* Header */}
      <div className="px-3 py-2 border-b border-border/50 bg-muted/30">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <TrendingUp className="h-4 w-4 text-muted-foreground" />
            <span
              data-content-role="heading"
              data-content-level="3"
              className="text-xs font-medium"
            >
              Execution Stats
            </span>
          </div>
          {currentStage && stageColors && (
            <Badge
              data-content-role="badge"
              data-content-label="current stage"
              className={cn(
                "text-[10px] border",
                stageColors.bg,
                stageColors.text,
                stageColors.border,
                isRunning && "animate-phase-glow",
              )}
            >
              {stageConfig?.label}
            </Badge>
          )}
        </div>
      </div>

      {/* Stats Grid */}
      <div className="p-3 grid grid-cols-2 gap-3">
        {/* Steps Progress */}
        <div className="space-y-1">
          <div className="flex items-center gap-1.5 text-muted-foreground">
            <Activity className="h-3.5 w-3.5" />
            <span data-content-role="label" className="text-[10px] uppercase tracking-wide">
              Steps
            </span>
          </div>
          <div className="flex items-baseline gap-1">
            <span
              data-content-role="metric"
              data-content-label="completed steps"
              className="text-lg font-semibold font-mono"
            >
              {completedSteps}
            </span>
            <span className="text-xs text-muted-foreground">/ {totalSteps}</span>
          </div>
          {/* Progress bar */}
          <div className="h-1 bg-muted rounded-full overflow-hidden">
            <div
              className={cn(
                "h-full transition-all duration-300",
                failedSteps > 0 ? "bg-red-500" : "bg-green-500",
              )}
              style={{ width: `${progressPercent}%` }}
            />
          </div>
        </div>

        {/* Success Rate */}
        <div className="space-y-1">
          <div className="flex items-center gap-1.5 text-muted-foreground">
            <CheckCircle2 className="h-3.5 w-3.5" />
            <span data-content-role="label" className="text-[10px] uppercase tracking-wide">
              Success
            </span>
          </div>
          <div className="flex items-baseline gap-1">
            <span
              data-content-role="metric"
              data-content-label="success rate"
              className={cn(
                "text-lg font-semibold font-mono",
                successRate >= 80 ? successColors.text : errorColors.text,
              )}
            >
              {successRate.toFixed(0)}%
            </span>
          </div>
          {failedSteps > 0 && (
            <span
              data-content-role="metric"
              data-content-label="failed steps"
              className={cn("text-xs", errorColors.text)}
            >
              {failedSteps} failed
            </span>
          )}
        </div>

        {/* Elapsed Time */}
        <div className="space-y-1">
          <div className="flex items-center gap-1.5 text-muted-foreground">
            <Clock className="h-3.5 w-3.5" />
            <span data-content-role="label" className="text-[10px] uppercase tracking-wide">
              Elapsed
            </span>
          </div>
          <div className="flex items-center gap-1.5">
            <span
              data-content-role="metric"
              data-content-label="elapsed time"
              className="text-lg font-semibold font-mono"
            >
              {elapsedDisplay}
            </span>
            {isRunning && (
              <Loader2 className={cn("h-3.5 w-3.5 animate-spin", pendingColors.text)} />
            )}
          </div>
        </div>

        {/* Iteration Progress */}
        {iteration !== undefined && maxIterations !== undefined && maxIterations > 1 ? (
          <div className="space-y-1">
            <div className="flex items-center gap-1.5 text-muted-foreground">
              <RefreshCw className="h-3.5 w-3.5" />
              <span data-content-role="label" className="text-[10px] uppercase tracking-wide">
                Iteration
              </span>
            </div>
            <div className="flex items-baseline gap-1">
              <span
                data-content-role="metric"
                data-content-label="iteration progress"
                className="text-lg font-semibold font-mono"
              >
                {iteration}
              </span>
              <span className="text-xs text-muted-foreground">/ {maxIterations}</span>
            </div>
          </div>
        ) : (
          /* Placeholder for grid alignment */
          <div />
        )}
      </div>
    </div>
  );
}

export default ExecutionStatsCard;
