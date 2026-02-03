/**
 * CompactRunCard Component
 *
 * Compact card for displaying a secondary run in the ActiveRunsBar.
 * Shows run status, name, and GUI lock indicator.
 */

import { forwardRef } from "react";
import { Lock, Play, CheckCircle2, XCircle, Circle, Loader2, Monitor, Cloud } from "lucide-react";
import type { ActiveRun, BridgeMode } from "../../contexts/ActiveRunsContext";
import { getAccentColors } from "@/design-system";

/**
 * Props for CompactRunCard.
 */
export interface CompactRunCardProps {
  /** The run to display */
  run: ActiveRun;
  /** Whether this card is selected */
  isSelected?: boolean;
  /** Click handler to select this run */
  onClick: () => void;
  /** Whether this card is focused (for keyboard nav) */
  isFocused?: boolean;
  /** Index for accessibility */
  index?: number;
}

/**
 * Get status icon and color based on run status.
 */
function getStatusDisplay(status: ActiveRun["status"]) {
  switch (status) {
    case "running":
      return {
        icon: Loader2,
        color: "blue",
        iconClass: "animate-spin",
        label: "Running",
      };
    case "completed":
      return {
        icon: CheckCircle2,
        color: "green",
        iconClass: "",
        label: "Completed",
      };
    case "failed":
      return {
        icon: XCircle,
        color: "red",
        iconClass: "",
        label: "Failed",
      };
    case "stopped":
      return {
        icon: Circle,
        color: "zinc",
        iconClass: "",
        label: "Stopped",
      };
    case "pending":
    default:
      return {
        icon: Play,
        color: "amber",
        iconClass: "",
        label: "Pending",
      };
  }
}

/**
 * Get mode display configuration.
 */
function getModeDisplay(mode: BridgeMode) {
  switch (mode) {
    case "gui":
      return {
        icon: Monitor,
        color: "amber",
        label: "GUI",
      };
    case "headless":
      return {
        icon: Cloud,
        color: "cyan",
        label: "Headless",
      };
    default:
      return null;
  }
}

/**
 * Format elapsed time from start time.
 */
function formatElapsed(startTime: number | null): string {
  if (!startTime) return "";

  const elapsed = Math.floor((Date.now() - startTime) / 1000);
  if (elapsed < 60) return `${elapsed}s`;

  const mins = Math.floor(elapsed / 60);
  const secs = elapsed % 60;
  return `${mins}m ${secs}s`;
}

/**
 * Compact run card for the active runs bar.
 * Uses forwardRef to support keyboard navigation focus management.
 */
export const CompactRunCard = forwardRef<HTMLButtonElement, CompactRunCardProps>(
  function CompactRunCard({ run, isSelected = false, onClick, isFocused = false, index }, ref) {
    const statusDisplay = getStatusDisplay(run.status);
    const StatusIcon = statusDisplay.icon;
    const colors = getAccentColors(statusDisplay.color as Parameters<typeof getAccentColors>[0]);
    const modeDisplay = getModeDisplay(run.mode);

    const elapsed = run.status === "running" ? formatElapsed(run.startTime) : "";

    // Recently completed runs get a brief highlight animation
    const isRecentlyCompleted = run.status === "completed" || run.status === "failed";

    // Get mode colors
    const modeColors = modeDisplay
      ? getAccentColors(modeDisplay.color as Parameters<typeof getAccentColors>[0])
      : null;
    const ModeIcon = modeDisplay?.icon;

    return (
      <button
        ref={ref}
        onClick={onClick}
        role="option"
        aria-selected={isSelected}
        aria-label={`Run: ${run.taskName || "Unnamed Run"}, Status: ${statusDisplay.label}${run.mode ? `, Mode: ${run.mode}` : ""}${run.hasGuiLock ? ", Has GUI control" : ""}`}
        tabIndex={isFocused ? 0 : -1}
        data-run-index={index}
        className={`
          flex items-center gap-2 px-3 py-2 rounded-lg
          border transition-all duration-200
          min-w-[180px] max-w-[240px]
          focus:outline-none focus:ring-2 focus:ring-primary focus:ring-offset-1
          ${
            isSelected
              ? `${colors.border} border-2 ${colors.bg} shadow-md`
              : isFocused
                ? "border-primary/50 bg-muted/70"
                : "border-border bg-card hover:bg-muted/50 hover:border-muted-foreground/30"
          }
          ${isRecentlyCompleted ? "animate-pulse-once" : ""}
        `}
        title={`${run.taskName || "Unnamed Run"} - ${statusDisplay.label}${run.mode ? ` (${run.mode})` : ""}`}
      >
        {/* Status Icon */}
        <div className={`flex-shrink-0 ${colors.text}`}>
          <StatusIcon className={`h-4 w-4 ${statusDisplay.iconClass}`} />
        </div>

        {/* Run Info */}
        <div className="flex-1 min-w-0 text-left">
          <div className="flex items-center gap-1.5">
            <span className="text-xs font-medium text-foreground truncate">
              {run.taskName || run.workflowName || "Run"}
            </span>
            {/* Mode badge */}
            {modeDisplay && modeColors && ModeIcon && (
              <div
                className={`flex items-center gap-0.5 px-1.5 py-0.5 rounded text-[9px] font-medium ${modeColors.bg} ${modeColors.text}`}
                title={`${modeDisplay.label} mode`}
              >
                <ModeIcon className="h-2.5 w-2.5" />
                <span>{modeDisplay.label}</span>
              </div>
            )}
            {/* GUI Lock indicator with pulsing glow */}
            {run.hasGuiLock && (
              <div className="relative flex-shrink-0" title="Has exclusive GUI control">
                <Lock className="h-3 w-3 text-amber-500 relative z-10" />
                <div className="absolute inset-0 h-3 w-3 bg-amber-500/40 rounded-full animate-ping" />
              </div>
            )}
          </div>

          {/* Status line */}
          <div className="flex items-center gap-2 text-[10px] text-muted-foreground">
            <span className={colors.text}>{statusDisplay.label}</span>
            {elapsed && <span className="font-mono">{elapsed}</span>}
            {run.maxIterations > 1 && (
              <span>
                {run.iteration}/{run.maxIterations}
              </span>
            )}
          </div>
        </div>
      </button>
    );
  },
);

// Also export as default
export default CompactRunCard;
