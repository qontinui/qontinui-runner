/**
 * useExecutionStatusWidgetData Hook
 *
 * Data hook for the execution status dashboard widget.
 * Wraps the main useExecutionStatus hook and uses TaskContext
 * to show running state even when events haven't been received.
 */

import { useMemo } from "react";
import { useExecutionStatus } from "../../../../hooks/useExecutionStatus";
import { useIsTaskRunning, useTaskContext } from "../../../../contexts/TaskContext";
import type { ExecutionStatusWidgetData } from "./types";

/**
 * Hook to fetch execution status data for the dashboard widget.
 * Uses TaskContext to derive status when event-based status is unavailable.
 */
export function useExecutionStatusWidgetData(): ExecutionStatusWidgetData {
  const { status, clearStatus, isConnected, lastEventTime } = useExecutionStatus();
  const isTaskRunning = useIsTaskRunning();
  const taskContext = useTaskContext();

  // Merge event-based status with task running state
  // If events haven't set the status but a task is running, show "running"
  const derivedStatus = useMemo(() => {
    // If we've received events (lastEventTime is set), use the event-based status
    if (lastEventTime !== null) {
      return status;
    }

    // Otherwise, derive status from task running state
    if (isTaskRunning) {
      return {
        ...status,
        status: "running" as const,
        taskRunId: taskContext?.taskRunId ?? status.taskRunId,
        taskName: taskContext?.taskInfo?.taskName ?? status.taskName,
        iteration: taskContext?.taskInfo?.iteration ?? status.iteration,
        lastUpdated: Date.now(),
      };
    }

    // No task running and no events - show idle
    return status;
  }, [status, lastEventTime, isTaskRunning, taskContext]);

  return {
    status: derivedStatus,
    isConnected,
    lastEventTime,
    clearStatus,
  };
}
