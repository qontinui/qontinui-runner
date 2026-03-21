/**
 * ProgressBar Component
 *
 * A flexible progress bar component with support for:
 * - Percentage or current/total display
 * - Different colors based on progress type
 * - Indeterminate mode for unknown totals
 * - Animated transitions and pulsing effects
 * - Success/error states for completed progress
 * - Defensive parsing to handle invalid data gracefully
 *
 * @example
 * // Basic progress bar
 * <ProgressBar current={50} total={100} />
 *
 * // With label showing "X of Y"
 * <ProgressBar current={5} total={10} showLabel labelFormat="count" />
 *
 * // Indeterminate mode (total unknown)
 * <ProgressBar current={42} indeterminate />
 *
 * // Different progress types with semantic colors
 * <ProgressBar current={3} total={10} progressType="test_progress" />
 */

import * as React from "react";
import { cn } from "../../lib/utils";
import { getAccentColors, type AccentColor } from "@/design-system";

/**
 * Safely parse a number value, returning a default if invalid.
 * Handles null, undefined, NaN, Infinity, and non-numeric values.
 */
function safeNumber(value: unknown, defaultValue: number): number {
  if (value === null || value === undefined) {
    return defaultValue;
  }
  const num = typeof value === "number" ? value : Number(value);
  if (!Number.isFinite(num)) {
    return defaultValue;
  }
  return num;
}

/**
 * Safely parse a nullable number value.
 * Returns null for invalid values instead of a default.
 */
function safeNullableNumber(value: unknown): number | null {
  if (value === null || value === undefined) {
    return null;
  }
  const num = typeof value === "number" ? value : Number(value);
  if (!Number.isFinite(num)) {
    return null;
  }
  return num;
}

export type ProgressType =
  | "file_progress"
  | "test_progress"
  | "analysis_progress"
  | "review_progress"
  | "iteration_progress"
  | "default";

export type ProgressStatus = "active" | "success" | "error" | "idle";

export type LabelFormat = "percentage" | "count" | "both" | "none";

export interface ProgressBarProps {
  /** Current progress value */
  current: number;
  /** Total value (if known) */
  total?: number | null;
  /** Progress type determines color scheme */
  progressType?: ProgressType | string;
  /** Override the auto-detected status */
  status?: ProgressStatus;
  /** Show indeterminate animation when total is unknown */
  indeterminate?: boolean;
  /** Whether to show label */
  showLabel?: boolean;
  /** Label format */
  labelFormat?: LabelFormat;
  /** Custom label text (overrides automatic label) */
  label?: string;
  /** Description text below progress bar */
  description?: string;
  /** Size variant */
  size?: "xs" | "sm" | "md" | "lg";
  /** Additional CSS classes for container */
  className?: string;
  /** Additional CSS classes for the progress track */
  trackClassName?: string;
  /** Additional CSS classes for the progress fill */
  fillClassName?: string;
  /** ARIA label for accessibility */
  ariaLabel?: string;
}

/**
 * Map progress types to accent colors.
 */
function getProgressColor(type: ProgressType | string, status: ProgressStatus): AccentColor {
  // Status overrides type colors
  if (status === "success") return "green";
  if (status === "error") return "red";

  // Type-specific colors
  const typeColors: Record<string, AccentColor> = {
    file_progress: "blue",
    test_progress: "purple",
    analysis_progress: "cyan",
    review_progress: "amber",
    iteration_progress: "emerald",
    default: "blue",
  };

  return typeColors[type] || "blue";
}

/**
 * Get size classes for different size variants.
 */
function getSizeClasses(size: ProgressBarProps["size"]) {
  switch (size) {
    case "xs":
      return { track: "h-1", text: "text-[10px]" };
    case "sm":
      return { track: "h-1.5", text: "text-xs" };
    case "md":
      return { track: "h-2", text: "text-xs" };
    case "lg":
      return { track: "h-3", text: "text-sm" };
    default:
      return { track: "h-2", text: "text-xs" };
  }
}

/**
 * Calculate percentage from current/total values.
 */
function calculatePercentage(current: number, total: number | null | undefined): number {
  if (total === null || total === undefined || total === 0) {
    return 0;
  }
  return Math.min(100, Math.max(0, Math.round((current / total) * 100)));
}

/**
 * Format label based on format preference.
 */
function formatLabel(
  current: number,
  total: number | null | undefined,
  format: LabelFormat,
): string {
  const percentage = calculatePercentage(current, total);

  switch (format) {
    case "percentage":
      return `${percentage}%`;
    case "count":
      if (total === null || total === undefined) {
        return `${current}`;
      }
      return `${current}/${total}`;
    case "both":
      if (total === null || total === undefined) {
        return `${current}`;
      }
      return `${current}/${total} (${percentage}%)`;
    case "none":
    default:
      return "";
  }
}

/**
 * Detect status based on progress values.
 */
function detectStatus(
  current: number,
  total: number | null | undefined,
  indeterminate: boolean,
): ProgressStatus {
  if (indeterminate || total === null || total === undefined) {
    return "active";
  }
  if (current >= total) {
    return "success";
  }
  if (current > 0) {
    return "active";
  }
  return "idle";
}

/**
 * ProgressBar component with animations and semantic coloring.
 * Includes defensive parsing to handle invalid data gracefully.
 */
export const ProgressBar = React.forwardRef<HTMLDivElement, ProgressBarProps>(
  (
    {
      current: rawCurrent,
      total: rawTotal,
      progressType = "default",
      status: statusProp,
      indeterminate = false,
      showLabel = false,
      labelFormat = "count",
      label,
      description,
      size = "md",
      className,
      trackClassName,
      fillClassName,
      ariaLabel,
    },
    ref,
  ) => {
    // Defensive parsing: ensure current is a valid number
    const current = safeNumber(rawCurrent, 0);
    // Defensive parsing: total can be null/undefined (indeterminate) or a valid number
    const total = safeNullableNumber(rawTotal);

    // Auto-detect indeterminate mode if total is null/undefined
    const isIndeterminate = indeterminate || total === null || total === undefined;

    // Auto-detect status or use provided status
    const status = statusProp || detectStatus(current, total, isIndeterminate);

    // Calculate percentage for determinate progress
    const percentage = isIndeterminate ? 0 : calculatePercentage(current, total);

    // Get colors based on type and status
    const color = getProgressColor(progressType, status);
    const colorClasses = getAccentColors(color);

    // Get size classes
    const sizeClasses = getSizeClasses(size);

    // Generate label text
    const labelText = label || (showLabel ? formatLabel(current, total, labelFormat) : "");

    // Determine fill color class
    const fillColorClass =
      status === "success"
        ? "bg-green-500"
        : status === "error"
          ? "bg-red-500"
          : colorClasses.bgSolid;

    return (
      <div ref={ref} className={cn("w-full", className)}>
        {/* Header with label */}
        {(labelText || description) && (
          <div className="flex items-center justify-between mb-1.5">
            {labelText && (
              <span
                className={cn(
                  "font-medium tabular-nums",
                  sizeClasses.text,
                  status === "success"
                    ? "text-green-500"
                    : status === "error"
                      ? "text-red-500"
                      : "text-muted-foreground",
                )}
              >
                {labelText}
              </span>
            )}
            {description && (
              <span className={cn("text-muted-foreground truncate ml-2", sizeClasses.text)}>
                {description}
              </span>
            )}
          </div>
        )}

        {/* Progress track */}
        <div
          role="progressbar"
          aria-valuenow={isIndeterminate ? undefined : current}
          aria-valuemin={0}
          aria-valuemax={total || undefined}
          aria-label={ariaLabel || `Progress: ${labelText || `${percentage}%`}`}
          aria-busy={status === "active"}
          className={cn(
            "relative w-full min-w-[60px] overflow-hidden rounded-full bg-muted/50",
            sizeClasses.track,
            trackClassName,
          )}
        >
          {isIndeterminate ? (
            /* Indeterminate animation - uses will-change for performance */
            <div
              className={cn(
                "absolute inset-y-0 w-1/3 animate-indeterminate-progress rounded-full will-change-transform",
                fillColorClass,
                fillClassName,
              )}
            />
          ) : (
            /* Determinate progress fill - uses will-change for smooth width transitions */
            <div
              className={cn(
                "h-full rounded-full transition-all duration-300 ease-out will-change-[width]",
                fillColorClass,
                status === "active" && "animate-pulse-subtle",
                fillClassName,
              )}
              style={{ width: `${percentage}%` }}
            />
          )}
        </div>
      </div>
    );
  },
);

ProgressBar.displayName = "ProgressBar";

/**
 * Compact inline progress indicator for step rows.
 * Shows a small progress bar with count.
 * Includes defensive parsing to handle invalid data gracefully.
 */
export function InlineProgressBar({
  current: rawCurrent,
  total: rawTotal,
  progressType = "default",
  className,
}: {
  current: number;
  total: number | null;
  progressType?: ProgressType | string;
  className?: string;
}) {
  // Defensive parsing: ensure current is a valid number
  const current = safeNumber(rawCurrent, 0);
  // Defensive parsing: total can be null (indeterminate) or a valid number
  const total = safeNullableNumber(rawTotal);

  const status = detectStatus(current, total, total === null);
  const percentage = calculatePercentage(current, total);
  const color = getProgressColor(progressType, status);
  const colorClasses = getAccentColors(color);

  const fillColorClass =
    status === "success"
      ? "bg-green-500"
      : status === "error"
        ? "bg-red-500"
        : colorClasses.bgSolid;

  const labelText = total !== null ? `${current}/${total}` : String(current);

  return (
    <div
      className={cn("flex items-center gap-1.5 shrink-0", className)}
      role="progressbar"
      aria-valuenow={total === null ? undefined : current}
      aria-valuemin={0}
      aria-valuemax={total || undefined}
      aria-label={`Progress: ${labelText}`}
    >
      <div className="relative h-1 w-12 min-w-[48px] overflow-hidden rounded-full bg-muted/50">
        {total === null ? (
          <div
            className={cn(
              "absolute inset-y-0 w-1/3 animate-indeterminate-progress rounded-full will-change-transform",
              fillColorClass,
            )}
          />
        ) : (
          <div
            className={cn(
              "h-full rounded-full transition-all duration-300 ease-out will-change-[width]",
              fillColorClass,
            )}
            style={{ width: `${percentage}%` }}
          />
        )}
      </div>
      <span className="text-xs text-muted-foreground tabular-nums whitespace-nowrap">
        {labelText}
      </span>
    </div>
  );
}

export default ProgressBar;
