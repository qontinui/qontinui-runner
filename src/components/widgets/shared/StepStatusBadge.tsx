/**
 * StepStatusBadge Component
 *
 * Displays a status badge with icon for step execution status.
 * Used across all step-type widgets for consistent status display.
 */

import { CheckCircle2, XCircle, Loader2, Clock, SkipForward } from "lucide-react";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui";
import { getStatusColors } from "@/design-system";
import type { StepExecutionStatus } from "./types";

interface StepStatusBadgeProps {
  status: StepExecutionStatus;
  /** Show only the icon without badge background */
  iconOnly?: boolean;
  /** Additional CSS classes */
  className?: string;
  /** Size variant */
  size?: "sm" | "md";
}

/**
 * Get icon and colors for a status.
 */
function getStatusConfig(status: StepExecutionStatus) {
  switch (status) {
    case "running":
      return {
        icon: Loader2,
        colors: getStatusColors("running"),
        label: "Running",
        iconClassName: "animate-spin",
      };
    case "success":
      return {
        icon: CheckCircle2,
        colors: getStatusColors("success"),
        label: "Success",
        iconClassName: "",
      };
    case "failed":
      return {
        icon: XCircle,
        colors: getStatusColors("error"),
        label: "Failed",
        iconClassName: "",
      };
    case "pending":
      return {
        icon: Clock,
        colors: getStatusColors("pending"),
        label: "Pending",
        iconClassName: "",
      };
    case "skipped":
      return {
        icon: SkipForward,
        colors: getStatusColors("muted"),
        label: "Skipped",
        iconClassName: "",
      };
    default:
      return {
        icon: Clock,
        colors: getStatusColors("muted"),
        label: "Unknown",
        iconClassName: "",
      };
  }
}

/**
 * Status badge component for step execution.
 */
export function StepStatusBadge({
  status,
  iconOnly = false,
  className,
  size = "md",
}: StepStatusBadgeProps) {
  const config = getStatusConfig(status);
  const Icon = config.icon;
  const iconSize = size === "sm" ? "h-3 w-3" : "h-4 w-4";

  if (iconOnly) {
    return <Icon className={cn(iconSize, config.colors.text, config.iconClassName, className)} />;
  }

  return (
    <Badge
      className={cn(
        "gap-1 border",
        config.colors.bg,
        config.colors.text,
        config.colors.border,
        size === "sm" ? "text-[10px] px-1.5 py-0" : "text-xs",
        className,
      )}
    >
      <Icon className={cn(size === "sm" ? "h-2.5 w-2.5" : "h-3 w-3", config.iconClassName)} />
      {config.label}
    </Badge>
  );
}

export default StepStatusBadge;
