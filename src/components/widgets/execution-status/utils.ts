/**
 * Execution Status Widget Utilities
 *
 * Shared helpers for the execution status widget components.
 */

import { getStatusColors } from "@/design-system";
import type { ExecutionStatusWidgetData } from "./types";

export interface StatusInfo {
  color: { bg: string; text: string; border?: string };
  label: string;
  dotClass: string;
}

/**
 * Get display information for a given execution status.
 */
export function getStatusInfo(status: ExecutionStatusWidgetData["status"]): StatusInfo {
  switch (status) {
    case "running":
      return {
        color: getStatusColors("running"),
        label: "Running",
        dotClass: "bg-green-500 animate-pulse",
      };
    case "completed":
      return {
        color: getStatusColors("success"),
        label: "Completed",
        dotClass: "bg-green-500",
      };
    case "failed":
      return {
        color: getStatusColors("error"),
        label: "Failed",
        dotClass: "bg-red-500",
      };
    case "stopped":
      return {
        color: getStatusColors("warning"),
        label: "Stopped",
        dotClass: "bg-amber-500",
      };
    case "idle":
    default:
      return {
        color: { bg: "bg-muted/50", text: "text-muted-foreground", border: "border-muted" },
        label: "Idle",
        dotClass: "bg-muted-foreground",
      };
  }
}

/**
 * Format seconds into human-readable elapsed time.
 */
export function formatElapsedTime(seconds: number): string {
  if (seconds < 60) return `${Math.floor(seconds)}s`;
  if (seconds < 3600) {
    const mins = Math.floor(seconds / 60);
    const secs = Math.floor(seconds % 60);
    return `${mins}m ${secs}s`;
  }
  const hours = Math.floor(seconds / 3600);
  const mins = Math.floor((seconds % 3600) / 60);
  return `${hours}h ${mins}m`;
}

/**
 * Get session budget color based on usage percentage.
 * Blue < 70%, Amber 70-89%, Red >= 90%
 */
export function getSessionBudgetColor(sessionsUsed: number, maxSessions: number | null) {
  if (maxSessions === null || maxSessions === 0) return "blue" as const;
  const pct = (sessionsUsed / maxSessions) * 100;
  if (pct >= 90) return "red" as const;
  if (pct >= 70) return "amber" as const;
  return "blue" as const;
}

/**
 * Get pass rate for an iteration result.
 */
export function getPassRate(passed: number, total: number): number {
  if (total === 0) return 0;
  return (passed / total) * 100;
}
