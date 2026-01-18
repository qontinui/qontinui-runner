/**
 * VerificationSummary Component
 *
 * Compact summary view for the Verification widget.
 * Shows a pass/fail badge with icon, test name, and "View Details" link.
 */

import { CheckCircle, XCircle, Clock, Loader2, SkipForward, FlaskConical } from "lucide-react";
import { cn } from "../../../../lib/utils";
import type { BaseWidgetProps } from "../../../../types/dashboard/widget-props";
import type { VerificationStatus } from "./types";
import { useVerificationDataWithStatus } from "./useVerificationData";
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
 * Get status configuration for display.
 */
function getStatusConfig(status: VerificationStatus) {
  const pendingColors = getStatusColors("pending");
  const _runningColors = getStatusColors("running");
  const successColors = getStatusColors("success");
  const errorColors = getStatusColors("error");
  const pausedColors = getStatusColors("paused");

  const configs = {
    pending: {
      icon: Clock,
      label: "Pending",
      iconClass: "text-muted-foreground",
      badgeClass: "bg-muted text-muted-foreground",
    },
    running: {
      icon: Loader2,
      label: "Running",
      iconClass: cn(pendingColors.text, "animate-spin"),
      badgeClass: cn(pendingColors.bg, pendingColors.text),
    },
    passed: {
      icon: CheckCircle,
      label: "Passed",
      iconClass: successColors.text,
      badgeClass: cn(successColors.bg, successColors.text),
    },
    failed: {
      icon: XCircle,
      label: "Failed",
      iconClass: errorColors.text,
      badgeClass: cn(errorColors.bg, errorColors.text),
    },
    skipped: {
      icon: SkipForward,
      label: "Skipped",
      iconClass: pausedColors.text,
      badgeClass: cn(pausedColors.bg, pausedColors.text),
    },
  };

  return configs[status] || configs.pending;
}

/**
 * VerificationSummary - Compact widget for dashboard summary row.
 */
export function VerificationSummary({
  isActive,
  onRequestFocus,
  onNavigateToDetail,
  className,
}: BaseWidgetProps) {
  const { data, isLoading } = useVerificationDataWithStatus();

  const handleClick = () => {
    if (onRequestFocus) {
      onRequestFocus();
    }
  };

  const handleViewDetails = (e: React.MouseEvent) => {
    e.stopPropagation();
    if (onNavigateToDetail) {
      onNavigateToDetail();
    }
  };

  const statusConfig = getStatusConfig(data.status);
  const StatusIcon = statusConfig.icon;

  // Note: teal not in design system, using cyan as closest match
  const cyanColors = getAccentColors("cyan");

  return (
    <button
      onClick={handleClick}
      className={cn(
        "flex items-center gap-3 px-3 py-2 rounded-lg border transition-all",
        `hover:${cyanColors.bg} hover:${cyanColors.border}`,
        isActive ? cn(cyanColors.bg, cyanColors.border) : "bg-card border-border",
        className,
      )}
    >
      {/* Main Status Icon */}
      <div className="flex-shrink-0">
        <FlaskConical className={cn("h-5 w-5", cyanColors.text)} />
      </div>

      {/* Content */}
      <div className="flex-1 min-w-0 text-left">
        {isLoading ? (
          <span className="text-sm text-muted-foreground">Loading...</span>
        ) : (
          <>
            {/* Status Badge */}
            <div className="flex items-center gap-2">
              <span
                className={cn(
                  "inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-xs font-medium",
                  statusConfig.badgeClass,
                )}
              >
                <StatusIcon className={cn("h-3 w-3", statusConfig.iconClass)} />
                {statusConfig.label}
              </span>
            </div>

            {/* Test Name or Description */}
            {data.testName ? (
              <p className="text-xs text-muted-foreground truncate mt-0.5">{data.testName}</p>
            ) : data.description ? (
              <p className="text-xs text-muted-foreground truncate mt-0.5">{data.description}</p>
            ) : (
              <p className="text-xs text-muted-foreground mt-0.5">
                {data.status === "pending" ? "Awaiting verification" : "Verification result"}
              </p>
            )}
          </>
        )}
      </div>

      {/* Right Side: Duration + View Details */}
      <div className="flex items-center gap-2 flex-shrink-0">
        {data.durationMs > 0 && (
          <span className="text-xs text-muted-foreground font-mono">
            {formatDuration(data.durationMs)}
          </span>
        )}
        {onNavigateToDetail && (
          <button
            onClick={handleViewDetails}
            className={cn("text-xs hover:underline", cyanColors.text)}
          >
            Details
          </button>
        )}
      </div>
    </button>
  );
}

export default VerificationSummary;
