/**
 * VerificationSummary Component
 *
 * Compact summary view for the Verification widget.
 * Shows overall pass/fail status, check step counts, and progress.
 */

import { CheckCircle, XCircle, Clock, Loader2, SkipForward, FlaskConical, CheckCircle2 } from "lucide-react";
import { cn } from "../../../../lib/utils";
import { Badge } from "../../../ui";
import { StepStatsBar } from "../shared";
import type { BaseWidgetProps } from "../../../../types/dashboard/widget-props";
import type { VerificationStatus, VerificationData } from "./types";
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
 * Compact check stats display.
 */
function CheckStats({ data }: { data: VerificationData }) {
  const successColors = getStatusColors("success");
  const errorColors = getStatusColors("error");

  if (data.checkSteps.length === 0) return null;

  return (
    <div className="flex items-center gap-3 mt-1.5">
      <StepStatsBar stats={data.stats} compact />
    </div>
  );
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
  const cyanColors = getAccentColors("cyan");
  const successColors = getStatusColors("success");
  const errorColors = getStatusColors("error");

  return (
    <div className={cn("space-y-2", className)}>
      {isLoading ? (
        <div className="flex items-center justify-center py-2">
          <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
        </div>
      ) : (
        <>
          {/* Status Badge Row */}
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

            {/* Check count badge */}
            {data.checkSteps.length > 0 && (
              <Badge variant="muted" className="text-[10px]">
                {data.stats.completed}/{data.stats.total}
              </Badge>
            )}

            {/* Duration */}
            {data.durationMs > 0 && (
              <span className="text-xs text-muted-foreground font-mono ml-auto">
                {formatDuration(data.durationMs)}
              </span>
            )}
          </div>

          {/* Check Stats (if we have steps) */}
          {data.checkSteps.length > 0 ? (
            <div className="flex items-center gap-3 text-xs">
              {data.stats.successful > 0 && (
                <div className="flex items-center gap-1">
                  <CheckCircle2 className={cn("h-3 w-3", successColors.text)} />
                  <span className={successColors.text}>{data.stats.successful}</span>
                </div>
              )}
              {data.stats.failed > 0 && (
                <div className="flex items-center gap-1">
                  <XCircle className={cn("h-3 w-3", errorColors.text)} />
                  <span className={errorColors.text}>{data.stats.failed}</span>
                </div>
              )}
              {data.stats.pending > 0 && (
                <div className="flex items-center gap-1 text-muted-foreground">
                  <Clock className="h-3 w-3" />
                  <span>{data.stats.pending} pending</span>
                </div>
              )}
            </div>
          ) : (
            /* Legacy: Test name or description */
            <>
              {data.testName ? (
                <p className="text-xs text-muted-foreground truncate">{data.testName}</p>
              ) : data.description ? (
                <p className="text-xs text-muted-foreground truncate">{data.description}</p>
              ) : (
                <p className="text-xs text-muted-foreground">
                  {data.status === "pending" ? "Awaiting verification" : "Verification result"}
                </p>
              )}
            </>
          )}
        </>
      )}
    </div>
  );
}

export default VerificationSummary;
