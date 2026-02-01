/**
 * WidgetHeader Component
 *
 * Shared header component for dashboard widgets.
 * Shows title, icon, status, and optional progress/item count.
 */

import {
  Monitor,
  Globe,
  Bot,
  CheckCircle,
  AlertTriangle,
  ExternalLink,
  Loader2,
  XCircle,
  PauseCircle,
  CheckCircle2,
} from "lucide-react";
import { cn } from "../../lib/utils";
import type { WidgetHeaderProps } from "../../types/dashboard/widget-props";
import type { ActivityStatus } from "../../types/dashboard/activity-types";
import { getStatusColors, getAccentColors } from "@/design-system";

/**
 * Icon mapping for widget types.
 */
const iconMap: Record<string, React.ComponentType<{ className?: string }>> = {
  Monitor,
  Globe,
  Bot,
  CheckCircle,
  AlertTriangle,
};

/**
 * Get the status icon for the current activity status.
 */
function getStatusIcon(status: ActivityStatus) {
  const successColors = getStatusColors("success");
  const errorColors = getStatusColors("error");
  const pausedColors = getStatusColors("paused");

  switch (status) {
    case "running":
    case "loading":
      return <Loader2 className="w-3 h-3 animate-spin" />;
    case "completed":
      return <CheckCircle2 className={cn("w-3 h-3", successColors.text)} />;
    case "failed":
      return <XCircle className={cn("w-3 h-3", errorColors.text)} />;
    case "paused":
      return <PauseCircle className={cn("w-3 h-3", pausedColors.text)} />;
    case "stopped":
      return <XCircle className="w-3 h-3 text-muted-foreground" />;
    default:
      return null;
  }
}

/**
 * Get accent color classes based on color name.
 */
function getAccentClasses(color: string, isActive: boolean) {
  // Map teal to cyan since teal isn't in the design system
  const mappedColor = color === "teal" ? "cyan" : color;
  const accentColors = getAccentColors(mappedColor);

  // For inactive state, use a lighter border
  const borderClass = isActive ? accentColors.border : accentColors.border.replace("/30", "/20");

  return {
    text: accentColors.text,
    bg: accentColors.bg,
    border: borderClass,
  };
}

/**
 * WidgetHeader component.
 */
export function WidgetHeader({
  title,
  icon,
  accentColor,
  status,
  isActive,
  compact = false,
  progress,
  itemCount,
  itemLabel,
  detailRoute: _detailRoute,
  onViewAll,
}: WidgetHeaderProps) {
  const IconComponent = iconMap[icon] ?? Monitor;
  const colors = getAccentClasses(accentColor, isActive);
  const statusIcon = getStatusIcon(status);

  return (
    <div
      data-ui-id={`widget-header-${title.toLowerCase().replace(/\s+/g, '-')}`}
      className={cn(
        "flex items-center justify-between border-b border-border",
        compact ? "px-3 py-2" : "px-4 py-3",
        colors.bg,
      )}
    >
      {/* Left: Icon + Title + Status */}
      <div className="flex items-center gap-2 min-w-0">
        <div className={cn("flex-shrink-0", colors.text)}>
          <IconComponent className={compact ? "w-4 h-4" : "w-5 h-5"} />
        </div>
        <span className={cn("font-medium truncate", compact ? "text-sm" : "text-base")}>
          {title}
        </span>
        {statusIcon && <span className="flex-shrink-0">{statusIcon}</span>}
      </div>

      {/* Right: Progress/Count + View All */}
      <div className="flex items-center gap-3 flex-shrink-0">
        {/* Progress bar */}
        {progress !== undefined && !compact && (
          <div className="flex items-center gap-2">
            <div className="w-24 h-1.5 bg-muted rounded-full overflow-hidden">
              <div
                className={cn(
                  "h-full rounded-full transition-all",
                  colors.text.replace("text-", "bg-"),
                )}
                style={{ width: `${Math.min(100, progress)}%` }}
              />
            </div>
            <span className="text-xs text-muted-foreground">{Math.round(progress)}%</span>
          </div>
        )}

        {/* Item count */}
        {itemCount !== undefined && (
          <span className="text-xs text-muted-foreground">
            {itemCount} {itemLabel ?? "items"}
          </span>
        )}

        {/* View All link - hidden when widget is already active (in main container) */}
        {onViewAll && !isActive && (
          <button
            data-ui-id="widget-view-all-btn"
            onClick={onViewAll}
            className={cn("flex items-center gap-1 text-xs hover:underline", colors.text)}
          >
            View All
            <ExternalLink className="w-3 h-3" />
          </button>
        )}
      </div>
    </div>
  );
}

export default WidgetHeader;
