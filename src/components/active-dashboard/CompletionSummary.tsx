/**
 * CompletionSummary Component
 *
 * Overlay that appears when a workflow finishes (completed or failed).
 * Shows summary statistics: duration, step counts, success rate.
 * Auto-dismisses after 15 seconds.
 */

import { useEffect, useCallback, useRef } from "react";
import { cn } from "../../lib/utils";
import type { DashboardStatus } from "../../hooks/dashboard/useDashboardState";
import type { StepStats } from "@/components/widgets/shared/types";

/**
 * Props for CompletionSummary.
 */
interface CompletionSummaryProps {
  /** Whether the overlay is visible */
  isOpen: boolean;
  /** Callback to dismiss the overlay */
  onDismiss: () => void;
  /** Final status of the workflow */
  status: DashboardStatus;
  /** Step statistics from SharedStepDataContext */
  stats: StepStats;
  /** Total duration in seconds */
  durationSeconds: number;
}

/**
 * Format seconds into "Xm Ys" display string.
 */
function formatDuration(totalSeconds: number): string {
  if (totalSeconds < 60) return `${totalSeconds}s`;
  const mins = Math.floor(totalSeconds / 60);
  const secs = totalSeconds % 60;
  if (mins < 60) return `${mins}m ${secs}s`;
  const hours = Math.floor(mins / 60);
  const remainMins = mins % 60;
  return `${hours}h ${remainMins}m`;
}

export function CompletionSummary({
  isOpen,
  onDismiss,
  status,
  stats,
  durationSeconds,
}: CompletionSummaryProps) {
  const autoDismissRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Auto-dismiss after 15 seconds
  useEffect(() => {
    if (!isOpen) return;

    autoDismissRef.current = setTimeout(() => {
      onDismiss();
    }, 15000);

    return () => {
      if (autoDismissRef.current) {
        clearTimeout(autoDismissRef.current);
        autoDismissRef.current = null;
      }
    };
  }, [isOpen, onDismiss]);

  // Close on Escape
  const handleKeyDown = useCallback(
    (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onDismiss();
      }
    },
    [onDismiss],
  );

  useEffect(() => {
    if (!isOpen) return;
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [isOpen, handleKeyDown]);

  if (!isOpen) return null;

  const isSuccess = status === "completed";
  const successRate = Math.round(stats.successRate);

  return (
    <div
      className="fixed inset-0 z-50 bg-black/50 flex items-center justify-center"
      onClick={onDismiss}
      onKeyDown={(e) => { if (e.key === "Escape") onDismiss(); }}
      role="dialog"
      aria-modal="true"
      aria-label="Workflow completion summary"
    >
      <div
        className="bg-card border border-border rounded-xl shadow-2xl w-full max-w-sm mx-4 overflow-hidden"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => e.stopPropagation()}
      >
        {/* Status Header */}
        <div
          className={cn(
            "flex items-center justify-center gap-3 px-5 py-5",
            isSuccess
              ? "bg-green-500/10 border-b border-green-500/20"
              : "bg-red-500/10 border-b border-red-500/20",
          )}
        >
          {/* Status Icon */}
          <div
            className={cn(
              "flex items-center justify-center w-10 h-10 rounded-full",
              isSuccess ? "bg-green-500/20" : "bg-red-500/20",
            )}
          >
            {isSuccess ? (
              <svg
                className="w-6 h-6 text-green-400"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
              >
                <polyline points="20 6 9 17 4 12" />
              </svg>
            ) : (
              <svg
                className="w-6 h-6 text-red-400"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
              >
                <line x1="18" y1="6" x2="6" y2="18" />
                <line x1="6" y1="6" x2="18" y2="18" />
              </svg>
            )}
          </div>
          <div>
            <h2
              className={cn("text-lg font-semibold", isSuccess ? "text-green-400" : "text-red-400")}
            >
              {isSuccess ? "Workflow Completed" : "Workflow Failed"}
            </h2>
          </div>
        </div>

        {/* Stats Grid */}
        <div className="px-5 py-5 space-y-4">
          {/* Duration */}
          <div className="flex items-center justify-between">
            <span className="text-sm text-muted-foreground">Duration</span>
            <span className="text-sm font-medium font-mono text-foreground">
              {formatDuration(durationSeconds)}
            </span>
          </div>

          {/* Steps breakdown */}
          <div className="flex items-center justify-between">
            <span className="text-sm text-muted-foreground">Steps</span>
            <div className="flex items-center gap-2">
              <span className="text-sm font-medium text-foreground">{stats.total} total</span>
              {stats.successful > 0 && (
                <span className="text-xs text-green-400">{stats.successful} passed</span>
              )}
              {stats.failed > 0 && (
                <span className="text-xs text-red-400">{stats.failed} failed</span>
              )}
            </div>
          </div>

          {/* Success Rate */}
          <div className="flex items-center justify-between">
            <span className="text-sm text-muted-foreground">Success Rate</span>
            <span
              className={cn(
                "text-sm font-semibold",
                successRate >= 80
                  ? "text-green-400"
                  : successRate >= 50
                    ? "text-amber-400"
                    : "text-red-400",
              )}
            >
              {successRate}%
            </span>
          </div>

          {/* Progress bar */}
          <div className="w-full h-2 bg-muted rounded-full overflow-hidden">
            <div
              className={cn(
                "h-full rounded-full transition-all duration-500",
                isSuccess ? "bg-green-500" : "bg-red-500",
              )}
              style={{ width: `${successRate}%` }}
            />
          </div>
        </div>

        {/* Actions */}
        <div className="px-5 py-4 border-t border-border flex justify-end">
          <button
            onClick={onDismiss}
            className="px-4 py-2 text-sm font-medium text-foreground bg-muted hover:bg-muted/80 border border-border rounded-lg transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/50"
          >
            Dismiss
          </button>
        </div>
      </div>
    </div>
  );
}
