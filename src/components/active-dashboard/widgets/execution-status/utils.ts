/**
 * Execution Status Widget Utilities
 *
 * Shared helpers for the execution status widget components.
 */

import { getStatusColors } from "@/design-system";
import type { ExecutionStatus } from "../../../../types/executionStatus";

export interface StatusInfo {
  color: { bg: string; text: string; border?: string };
  label: string;
  dotClass: string;
}

/**
 * Get display information for a given execution status.
 */
export function getStatusInfo(status: ExecutionStatus["status"]): StatusInfo {
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
    case "paused":
      return {
        color: getStatusColors("warning"),
        label: "Paused",
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
