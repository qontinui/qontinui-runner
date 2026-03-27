/**
 * useTaskRunLive — Real-time task detail composite hook.
 *
 * Combines multiple GraphQL queries and subscriptions into a single hook
 * that provides a complete, live view of a task run with zero polling:
 *
 * - Task run metadata (useTaskRunGql)
 * - Live progress events (useTaskRunProgress subscription)
 * - Live findings (useFindingUpdates subscription)
 * - Task run output with pagination (useTaskRunOutputGql)
 * - Finding summary stats (useFindingSummaryGql)
 *
 * Usage:
 *   const { taskRun, events, findings, output, summary, loading } = useTaskRunLive("task-123");
 */

import { useState, useEffect, useRef } from "react";
import { useTaskRunGql, useTaskRunOutputGql, useFindingSummaryGql } from "./queries";
import type { TaskRunData, TaskRunOutputData, FindingSummaryData } from "./queries";
import { useTaskRunProgress, useFindingUpdates } from "./subscriptions";
import type { RunnerEventData, FindingData } from "./subscriptions";

export interface TaskRunLiveState {
  /** The task run data (from initial query + cache updates). */
  taskRun: TaskRunData | null;
  /** Most recent progress event from the subscription. */
  latestEvent: RunnerEventData | null;
  /** All accumulated progress events in this session. */
  events: RunnerEventData[];
  /** Current findings list (live from subscription). */
  findings: FindingData[];
  /** Finding summary stats. */
  summary: FindingSummaryData | null;
  /** Task run output (paginated). */
  output: TaskRunOutputData | null;
  /** Whether the task is in a running state. */
  isRunning: boolean;
  /** Whether the task has reached a terminal state. */
  isTerminal: boolean;
  /** Whether any data is still loading. */
  loading: boolean;
  /** Any error from queries or subscriptions. */
  error: string | null;
  /** Fetch the next page of output. */
  fetchMoreOutput: (offset: number) => void;
}

/**
 * Real-time task detail hook — zero-polling live task view.
 *
 * @param taskRunId - The task run ID to observe
 * @param options - Configuration options
 */
export function useTaskRunLive(
  taskRunId: string,
  options: {
    /** Skip all queries/subscriptions (e.g., when component is hidden) */
    skip?: boolean;
    /** Output page size in characters (default 10000) */
    outputLimit?: number;
    /** Finding poll interval for subscription (default 2000ms) */
    findingIntervalMs?: number;
  } = {},
): TaskRunLiveState {
  const { skip = false, outputLimit = 10000, findingIntervalMs = 2000 } = options;
  const active = !skip && !!taskRunId;

  // Accumulated events
  const [events, setEvents] = useState<RunnerEventData[]>([]);
  const [outputOffset, setOutputOffset] = useState(0);
  const prevTaskIdRef = useRef(taskRunId);

  // Reset state when task ID changes
  useEffect(() => {
    if (taskRunId !== prevTaskIdRef.current) {
      prevTaskIdRef.current = taskRunId;
      // eslint-disable-next-line react-hooks/set-state-in-effect -- resetting state on task ID change
      setEvents([]);
      setOutputOffset(0);
    }
  }, [taskRunId]);

  // --- Queries ---
  const {
    data: taskRunData,
    loading: taskRunLoading,
    error: taskRunError,
  } = useTaskRunGql(taskRunId, !active);

  const taskRun = taskRunData?.taskRun ?? null;
  const isRunning = taskRun?.status === "running" || taskRun?.status === "paused";
  const isTerminal =
    taskRun?.status === "complete" ||
    taskRun?.status === "failed" ||
    taskRun?.status === "stopped";

  const { data: outputData } = useTaskRunOutputGql(
    taskRunId,
    outputOffset,
    outputLimit,
    !active,
  );

  const { data: summaryData } = useFindingSummaryGql(taskRunId, !active);

  // --- Subscriptions (only when task is running) ---
  const { data: progressData } = useTaskRunProgress(taskRunId, !active || isTerminal);

  const { data: findingsData } = useFindingUpdates(
    taskRunId,
    findingIntervalMs,
    !active || isTerminal,
  );

  // Accumulate progress events (capped at 500 to prevent memory growth)
  useEffect(() => {
    const event = progressData?.taskRunProgress;
    if (event) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- accumulating events from subscription
      setEvents((prev) => {
        const next = [...prev, event];
        return next.length > 500 ? next.slice(-500) : next;
      });
    }
  }, [progressData]);

  // Error aggregation
  const error = taskRunError?.message ?? null;

  return {
    taskRun,
    latestEvent: events.length > 0 ? events[events.length - 1] : null,
    events,
    findings: findingsData?.findingUpdates ?? [],
    summary: summaryData?.findingSummary ?? null,
    output: outputData?.taskRunOutput ?? null,
    isRunning,
    isTerminal,
    loading: taskRunLoading,
    error,
    fetchMoreOutput: setOutputOffset,
  };
}
