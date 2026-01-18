/**
 * useExecutionStatusWidgetData Hook
 *
 * Data hook for the execution status dashboard widget.
 * Wraps the main useExecutionStatus hook.
 */

import { useExecutionStatus } from "../../../../hooks/useExecutionStatus";
import type { ExecutionStatusWidgetData } from "./types";

/**
 * Hook to fetch execution status data for the dashboard widget.
 */
export function useExecutionStatusWidgetData(): ExecutionStatusWidgetData {
  const { status, clearStatus, isConnected, lastEventTime } = useExecutionStatus();

  return {
    status,
    isConnected,
    lastEventTime,
    clearStatus,
  };
}
