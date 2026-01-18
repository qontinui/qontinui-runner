/**
 * PlaywrightSummary Component
 *
 * Compact summary view for the Playwright widget.
 * Shows pass/fail/skip counts and current test name.
 */

import { CheckCircle, XCircle, SkipForward, Loader2, PlayCircle } from "lucide-react";
import { cn } from "../../../../lib/utils";
import type { BaseWidgetProps } from "../../../../types/dashboard/widget-props";
import { usePlaywrightDataWithStatus } from "./usePlaywrightData";
import { getStatusColors, getAccentColors } from "@/design-system";

/**
 * Format duration for compact display.
 */
function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  const mins = Math.floor(ms / 60000);
  const secs = Math.floor((ms % 60000) / 1000);
  return `${mins}m ${secs}s`;
}

/**
 * PlaywrightSummary - Compact widget for dashboard summary row.
 */
export function PlaywrightSummary({ isActive, onRequestFocus, className }: BaseWidgetProps) {
  const { data, isLoading } = usePlaywrightDataWithStatus();

  const handleClick = () => {
    if (onRequestFocus) {
      onRequestFocus();
    }
  };

  // Calculate progress
  const totalTests = data.totalTests || 0;
  const completedTests = data.passedTests + data.failedTests + data.skippedTests;
  const progressPercent = totalTests > 0 ? Math.round((completedTests / totalTests) * 100) : 0;

  const purpleColors = getAccentColors("purple");
  const successColors = getStatusColors("success");
  const errorColors = getStatusColors("error");
  const pausedColors = getStatusColors("paused");

  return (
    <button
      onClick={handleClick}
      className={cn(
        "flex items-center gap-3 px-3 py-2 rounded-lg border transition-all",
        `hover:${purpleColors.bg} hover:${purpleColors.border}`,
        isActive ? cn(purpleColors.bg, purpleColors.border) : "bg-card border-border",
        className,
      )}
    >
      {/* Icon */}
      <div className="flex-shrink-0">
        {data.isRunning ? (
          <Loader2 className={cn("h-5 w-5 animate-spin", purpleColors.text)} />
        ) : (
          <PlayCircle className={cn("h-5 w-5", purpleColors.text)} />
        )}
      </div>

      {/* Content */}
      <div className="flex-1 min-w-0 text-left">
        {isLoading ? (
          <span className="text-sm text-muted-foreground">Loading...</span>
        ) : totalTests === 0 ? (
          <span className="text-sm text-muted-foreground">No tests</span>
        ) : (
          <>
            {/* Status counts */}
            <div className="flex items-center gap-3 text-xs">
              <span className="flex items-center gap-1">
                <CheckCircle className={cn("h-3 w-3", successColors.text)} />
                <span className={cn("font-medium", successColors.text)}>{data.passedTests}</span>
              </span>
              <span className="flex items-center gap-1">
                <XCircle className={cn("h-3 w-3", errorColors.text)} />
                <span className={cn("font-medium", errorColors.text)}>{data.failedTests}</span>
              </span>
              <span className="flex items-center gap-1">
                <SkipForward className={cn("h-3 w-3", pausedColors.text)} />
                <span className={cn("font-medium", pausedColors.text)}>{data.skippedTests}</span>
              </span>
            </div>

            {/* Current test or progress */}
            {data.isRunning && data.currentTest ? (
              <p className="text-xs text-muted-foreground truncate mt-0.5">{data.currentTest}</p>
            ) : (
              <p className="text-xs text-muted-foreground mt-0.5">{progressPercent}% complete</p>
            )}
          </>
        )}
      </div>

      {/* Duration */}
      {data.durationMs > 0 && (
        <span className="text-xs text-muted-foreground font-mono flex-shrink-0">
          {formatDuration(data.durationMs)}
        </span>
      )}
    </button>
  );
}

export default PlaywrightSummary;
