/**
 * Status Icon Utilities
 *
 * Provides consistent status icons and colors across the application.
 */

import type { ReactElement } from "react";
import { CheckCircle2, XCircle, AlertCircle, Clock, Activity } from "lucide-react";

export type StatusType = "success" | "complete" | "failed" | "running" | "skipped" | "pending";

/**
 * Status color configuration.
 */
export const STATUS_COLORS: Record<StatusType | string, { text: string; bg: string }> = {
  success: { text: "text-green-500", bg: "bg-green-500/10" },
  complete: { text: "text-green-500", bg: "bg-green-500/10" },
  failed: { text: "text-red-500", bg: "bg-red-500/10" },
  running: { text: "text-blue-500", bg: "bg-blue-500/10" },
  skipped: { text: "text-yellow-500", bg: "bg-yellow-500/10" },
  pending: { text: "text-yellow-500", bg: "bg-yellow-500/10" },
};

/**
 * Get status colors for a given status.
 *
 * @param status - The status string
 * @returns Object with text and bg color classes
 */
export function getStatusColors(status: string): { text: string; bg: string } {
  return STATUS_COLORS[status] || { text: "text-muted-foreground", bg: "bg-muted" };
}

/**
 * Get the status icon component for a given status.
 *
 * @param status - The status string
 * @param className - Optional additional class names
 * @returns JSX element for the status icon
 */
export function getStatusIcon(status: string, className?: string): ReactElement {
  const baseClass = className || "w-4 h-4";

  switch (status) {
    case "success":
    case "complete":
      return <CheckCircle2 className={`${baseClass} text-green-500`} />;
    case "failed":
      return <XCircle className={`${baseClass} text-red-500`} />;
    case "running":
      return <Activity className={`${baseClass} text-blue-500 animate-pulse`} />;
    case "skipped":
    case "pending":
      return <AlertCircle className={`${baseClass} text-yellow-500`} />;
    default:
      return <Clock className={`${baseClass} text-muted-foreground`} />;
  }
}

/**
 * Get the label text for a status.
 *
 * @param status - The status string
 * @param goalAchieved - Optional goal achievement flag
 * @returns Human-readable status label
 */
export function getStatusLabel(status: string, goalAchieved?: boolean): string {
  switch (status) {
    case "success":
    case "complete":
      return goalAchieved ? "Goal Achieved" : "Run Completed Successfully";
    case "failed":
      return "Run Failed";
    case "running":
      return "Run In Progress";
    case "skipped":
      return "Skipped";
    case "pending":
      return "Pending";
    default:
      return "Unknown Status";
  }
}
