/**
 * TimelineStatsBar Component
 *
 * Displays timeline-specific statistics:
 * - Elapsed time
 * - Current iteration
 * - Average iteration duration
 * - Improvement over last verification
 *
 * Progress bar is wrapped with error boundary to prevent
 * data issues from crashing the timeline view.
 */

import {
  CheckCircle2,
  Clock,
  RotateCcw,
  Timer,
  TrendingUp,
  TrendingDown,
  Minus,
} from "lucide-react";
import { cn } from "../../../../lib/utils";
import { ProgressBar, ProgressErrorBoundary } from "../../../ui";
import { getStatusColors } from "@/design-system";
import { formatDuration } from "../shared";
import type { TimelineStats } from "./types";
import type { StepStats } from "../shared/types";

interface TimelineStatsBarProps {
  stats: TimelineStats;
  /** Step statistics for showing overall progress */
  stepStats?: StepStats;
  /** Additional CSS classes */
  className?: string;
  /** Show compact version without progress bar */
  compact?: boolean;
}

/**
 * Timeline-specific statistics bar component.
 */
export function TimelineStatsBar({
  stats,
  stepStats,
  className,
  compact = false,
}: TimelineStatsBarProps) {
  const successColors = getStatusColors("success");
  const errorColors = getStatusColors("error");

  // Determine improvement icon and color
  let ImprovementIcon = Minus;
  let improvementColorClass = "text-muted-foreground";
  if (stats.improvement) {
    if (stats.improvement.delta > 0) {
      ImprovementIcon = TrendingUp;
      improvementColorClass = successColors.text;
    } else if (stats.improvement.delta < 0) {
      ImprovementIcon = TrendingDown;
      improvementColorClass = errorColors.text;
    }
  }

  // Determine overall progress status
  const getProgressStatus = () => {
    if (!stepStats) return "active";
    if (stepStats.failed > 0) return "error";
    if (stepStats.completed === stepStats.total && stepStats.total > 0) return "success";
    return "active";
  };

  return (
    <div className={cn("border-b border-border px-4 py-3 bg-muted/20", className)}>
      {/* Overall progress bar (shown when stepStats available and not compact) */}
      {stepStats && !compact && stepStats.total > 0 && (
        <div className="mb-3">
          <ProgressErrorBoundary componentName="TimelineStatsBar.ProgressBar">
            <ProgressBar
              current={stepStats.completed}
              total={stepStats.total}
              status={getProgressStatus()}
              showLabel
              labelFormat="both"
              size="sm"
              description={`${stepStats.pending} pending${stepStats.failed > 0 ? `, ${stepStats.failed} failed` : ""}`}
            />
          </ProgressErrorBoundary>
        </div>
      )}

      {/* Stats row */}
      <div className="flex items-center gap-6">
        {/* Elapsed Time */}
        <div className="flex items-center gap-2">
          <Clock className="h-4 w-4 text-muted-foreground" />
          <div>
            <p className="text-xs text-muted-foreground">Elapsed</p>
            <p className="font-mono text-sm text-foreground">
              {formatDuration(stats.elapsedTime * 1000)}
            </p>
          </div>
        </div>

        {/* Success Rate */}
        {stepStats && stepStats.completed > 0 && (
          <div className="flex items-center gap-2">
            <CheckCircle2
              className={cn(
                "h-4 w-4",
                stepStats.successRate >= 80
                  ? successColors.text
                  : stepStats.successRate >= 50
                    ? "text-muted-foreground"
                    : errorColors.text,
              )}
            />
            <div>
              <p className="text-xs text-muted-foreground">Pass Rate</p>
              <p
                className={cn(
                  "font-mono text-sm",
                  stepStats.successRate >= 80
                    ? successColors.text
                    : stepStats.successRate >= 50
                      ? "text-foreground"
                      : errorColors.text,
                )}
              >
                {stepStats.successRate.toFixed(0)}%
              </p>
            </div>
          </div>
        )}

        {/* Current Iteration */}
        {stats.maxIteration > 0 && (
          <div className="flex items-center gap-2">
            <RotateCcw className="h-4 w-4 text-muted-foreground" />
            <div>
              <p className="text-xs text-muted-foreground">Iteration</p>
              <p className="font-mono text-sm text-foreground">
                {stats.currentIteration ?? stats.maxIteration}
              </p>
            </div>
          </div>
        )}

        {/* Average Iteration Duration */}
        {stats.avgIterationDurationMs !== null && (
          <div className="flex items-center gap-2">
            <Timer className="h-4 w-4 text-muted-foreground" />
            <div>
              <p className="text-xs text-muted-foreground">Avg Iteration</p>
              <p className="font-mono text-sm text-foreground">
                {formatDuration(stats.avgIterationDurationMs)}
              </p>
            </div>
          </div>
        )}

        {/* Improvement over last iteration */}
        {stats.improvement && (
          <div className="flex items-center gap-2">
            <ImprovementIcon className={cn("h-4 w-4", improvementColorClass)} />
            <div>
              <p className="text-xs text-muted-foreground">vs Last Iteration</p>
              <p className={cn("font-mono text-sm", improvementColorClass)}>
                {stats.improvement.delta > 0 ? "+" : ""}
                {stats.improvement.delta}/{stats.improvement.total} tests
                <span className="text-xs ml-1">
                  ({stats.improvement.percentage > 0 ? "+" : ""}
                  {stats.improvement.percentage.toFixed(1)}%)
                </span>
              </p>
            </div>
          </div>
        )}

        {/* Verification results breakdown */}
        {stats.verificationResults.length > 0 && (
          <div className="flex items-center gap-2">
            <TrendingUp className="h-4 w-4 text-muted-foreground" />
            <div>
              <p className="text-xs text-muted-foreground">
                {stats.verificationResults.length === 1 ? "Verification" : "Verification History"}
              </p>
              {stats.verificationResults.length === 1 ? (
                <p className="font-mono text-sm text-foreground">
                  {stats.verificationResults[0].passed}/{stats.verificationResults[0].total} passed
                  {stats.verificationResults[0].durationMs != null && (
                    <span className="ml-1 text-muted-foreground text-xs">
                      in {formatDuration(stats.verificationResults[0].durationMs)}
                    </span>
                  )}
                </p>
              ) : (
                <div className="flex items-center gap-1.5">
                  {stats.verificationResults.map((result) => (
                    <span
                      key={result.iteration}
                      className={cn(
                        "font-mono text-xs px-1 py-0 rounded",
                        result.passed === result.total && result.total > 0
                          ? cn(successColors.text, "bg-green-500/10")
                          : result.passed === 0
                            ? cn(errorColors.text, "bg-red-500/10")
                            : "text-foreground bg-muted/30",
                      )}
                      title={`Iteration ${result.iteration}: ${result.passed}/${result.total} passed${result.durationMs ? ` (${formatDuration(result.durationMs)})` : ""}`}
                    >
                      {result.passed}/{result.total}
                      {result.durationMs != null && (
                        <span className="ml-1 text-muted-foreground text-[10px]">
                          {formatDuration(result.durationMs)}
                        </span>
                      )}
                    </span>
                  ))}
                </div>
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

export default TimelineStatsBar;
