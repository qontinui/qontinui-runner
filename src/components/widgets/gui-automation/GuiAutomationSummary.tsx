/**
 * GuiAutomationSummary Component
 *
 * Compact summary view for GUI automation widget.
 * Shows quick stats, the last 3 actions with status icons, and a screenshot thumbnail.
 */

import { useState, useEffect } from "react";
import {
  CheckCircle2,
  XCircle,
  Loader2,
  AlertCircle,
  Activity,
  Target,
  ImageIcon,
  X,
} from "lucide-react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui";
import type { GuiAutomationSummaryProps } from "./types";
import type { ActionItem, ActionStatus, ScreenshotInfo } from "@/components/active-dashboard/types";
import { getStatusColors, getAccentColors } from "@/design-system";

/**
 * Compact status icon for action items.
 */
function StatusIcon({ status, className }: { status: ActionStatus; className?: string }) {
  const pendingColors = getStatusColors("pending");
  const successColors = getStatusColors("success");
  const errorColors = getStatusColors("error");
  const pausedColors = getStatusColors("paused");

  switch (status) {
    case "running":
      return <Loader2 className={cn("h-3 w-3 animate-spin", pendingColors.text, className)} />;
    case "success":
      return <CheckCircle2 className={cn("h-3 w-3", successColors.text, className)} />;
    case "failed":
    case "error":
      return <XCircle className={cn("h-3 w-3", errorColors.text, className)} />;
    case "timeout":
      return <AlertCircle className={cn("h-3 w-3", pausedColors.text, className)} />;
    default:
      return (
        <div
          className={cn("h-3 w-3 rounded-full border-2 border-muted-foreground/50", className)}
        />
      );
  }
}

/**
 * Screenshot thumbnail with popup expansion.
 */
function ScreenshotThumbnail({ screenshot }: { screenshot: ScreenshotInfo | undefined }) {
  const [imageSrc, setImageSrc] = useState<string | null>(null);
  const [imageError, setImageError] = useState(false);
  const [showPopup, setShowPopup] = useState(false);

  // Convert file path to displayable URL
  useEffect(() => {
    if (screenshot?.path) {
      try {
        const src = convertFileSrc(screenshot.path);
        setImageSrc(src);
        setImageError(false);
      } catch {
        setImageError(true);
      }
    } else {
      setImageSrc(null);
    }
  }, [screenshot?.path]);

  if (!screenshot || !imageSrc || imageError) {
    return (
      <div className="w-12 h-12 rounded bg-muted/50 flex items-center justify-center flex-shrink-0">
        <ImageIcon className="h-5 w-5 text-muted-foreground/50" />
      </div>
    );
  }

  return (
    <>
      {/* Thumbnail */}
      <button
        onClick={(e) => {
          e.stopPropagation();
          setShowPopup(true);
        }}
        className="w-12 h-12 rounded bg-black/90 flex items-center justify-center flex-shrink-0 overflow-hidden border border-border hover:ring-2 hover:ring-blue-500/50 transition-all cursor-pointer"
        title="Click to expand screenshot"
      >
        <img
          src={imageSrc}
          alt={screenshot.filename || "Screenshot"}
          className="max-h-full max-w-full object-contain"
          onError={() => setImageError(true)}
        />
      </button>

      {/* Popup Modal */}
      {showPopup && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/80"
          onClick={() => setShowPopup(false)}
        >
          <div
            className="relative max-w-[90vw] max-h-[90vh] bg-background rounded-lg overflow-hidden shadow-2xl"
            onClick={(e) => e.stopPropagation()}
          >
            {/* Close button */}
            <button
              onClick={() => setShowPopup(false)}
              className="absolute top-2 right-2 z-10 p-1.5 rounded-full bg-black/70 text-white/90 hover:bg-black hover:text-white transition-colors"
            >
              <X className="h-5 w-5" />
            </button>

            {/* Filename badge */}
            <div className="absolute top-2 left-2 z-10">
              <Badge className="bg-black/70 text-white/90 text-xs border-0">
                {screenshot.filename}
              </Badge>
            </div>

            {/* Full-size image */}
            <img
              src={imageSrc}
              alt={screenshot.filename || "Screenshot"}
              className="max-w-[90vw] max-h-[90vh] object-contain"
            />
          </div>
        </div>
      )}
    </>
  );
}

/**
 * Compact action row for summary view.
 */
function CompactActionRow({ action }: { action: ActionItem }) {
  const pendingColors = getStatusColors("pending");
  const successColors = getStatusColors("success");
  const errorColors = getStatusColors("error");
  const slateColors = getAccentColors("slate");

  const getBadgeClasses = () => {
    if (action.status === "running") {
      return cn(pendingColors.bg, pendingColors.text, "border", pendingColors.border);
    }
    if (action.status === "success") {
      return cn(successColors.bg, successColors.text, "border", successColors.border);
    }
    if (action.status === "failed" || action.status === "error") {
      return cn(errorColors.bg, errorColors.text, "border", errorColors.border);
    }
    return cn(slateColors.bg, slateColors.text, "border", slateColors.border);
  };

  return (
    <div className="flex items-center gap-2 py-1">
      <StatusIcon status={action.status} />
      <Badge className={cn("text-[10px] px-1.5 py-0", getBadgeClasses())}>
        {action.action_type}
      </Badge>
      <span className="text-xs text-muted-foreground truncate flex-1">{action.target || ""}</span>
    </div>
  );
}

/**
 * Compact summary widget for GUI automation.
 */
export function GuiAutomationSummary({
  isActive: _isActive,
  isSummary: _isSummary,
  status,
  data,
}: GuiAutomationSummaryProps) {
  const { actions, stats, screenshots } = data;
  const recentActions = actions.slice(0, 3);
  const successColors = getStatusColors("success");
  const pendingColors = getStatusColors("pending");
  const blueColors = getAccentColors("blue");

  // Get the latest screenshot (first in array, sorted by modified desc)
  const latestScreenshot = screenshots?.[0];

  return (
    <div className="space-y-3">
      {/* Top row: Screenshot thumbnail + Quick Stats */}
      <div className="flex items-start gap-3">
        {/* Screenshot Thumbnail */}
        <ScreenshotThumbnail screenshot={latestScreenshot} />

        {/* Quick Stats */}
        <div className="flex flex-wrap items-center gap-x-4 gap-y-1 flex-1">
          {/* Actions Count */}
          <div className="flex items-center gap-1.5">
            <Activity className="h-3.5 w-3.5 text-muted-foreground" />
            <span className="text-sm font-medium text-foreground">{stats.actionsCompleted}</span>
            <span className="text-xs text-muted-foreground">actions</span>
          </div>

          {/* Success Rate */}
          <div className="flex items-center gap-1.5">
            <CheckCircle2 className={cn("h-3.5 w-3.5", successColors.text)} />
            <span className={cn("text-sm font-medium", successColors.text)}>
              {stats.successRate.toFixed(0)}%
            </span>
          </div>

          {/* Confidence */}
          {stats.averageConfidence > 0 && (
            <div className="flex items-center gap-1.5">
              <Target className={cn("h-3.5 w-3.5", blueColors.text)} />
              <span className={cn("text-sm font-medium", blueColors.text)}>
                {stats.averageConfidence.toFixed(0)}%
              </span>
            </div>
          )}
        </div>
      </div>

      {/* Recent Actions */}
      {recentActions.length > 0 ? (
        <div className="space-y-0.5">
          {recentActions.map((action) => (
            <CompactActionRow key={action.id} action={action} />
          ))}
        </div>
      ) : (
        <p className="text-xs text-muted-foreground">No actions executed yet</p>
      )}

      {/* Running Indicator */}
      {status === "running" && data.currentAction && (
        <div className="flex items-center gap-2 pt-1 border-t border-border/50">
          <Loader2 className={cn("h-3 w-3 animate-spin", pendingColors.text)} />
          <span className={cn("text-xs truncate", pendingColors.text)}>{data.currentAction}</span>
        </div>
      )}
    </div>
  );
}

export default GuiAutomationSummary;
