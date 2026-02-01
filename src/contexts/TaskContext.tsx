/**
 * TaskContext
 *
 * React context for sharing current task state across the dashboard.
 * This allows widget data hooks to access task info without prop drilling.
 */

import { createContext, useContext, type ReactNode } from "react";
import type { TaskActivityInfo } from "../types/dashboard/widget-registry";

/**
 * Task context value.
 */
interface TaskContextValue {
  /** Current task information */
  taskInfo: TaskActivityInfo | null;
  /** Whether a task is currently running */
  isRunning: boolean;
  /** Current task run ID (for filtering data to current run) */
  taskRunId: string | null;
  /** Task start time (ms since epoch) - used for filtering AI output */
  taskStartTime: number | null;
}

const TaskContext = createContext<TaskContextValue | null>(null);

/**
 * Provider props.
 */
interface TaskProviderProps {
  children: ReactNode;
  taskInfo: TaskActivityInfo | null;
  isRunning: boolean;
}

/**
 * Task context provider.
 */
export function TaskProvider({ children, taskInfo, isRunning }: TaskProviderProps) {
  const value: TaskContextValue = {
    taskInfo,
    isRunning,
    taskRunId: taskInfo?.taskId ?? null,
    taskStartTime: taskInfo?.startTime ?? null,
  };

  return <TaskContext.Provider value={value}>{children}</TaskContext.Provider>;
}

/**
 * Hook to access task context.
 * Returns null if used outside of TaskProvider.
 */
export function useTaskContext(): TaskContextValue | null {
  return useContext(TaskContext);
}

/**
 * Hook to get the current task run ID.
 * Returns null if no task is running or if used outside of TaskProvider.
 */
export function useCurrentTaskRunId(): string | null {
  const context = useContext(TaskContext);
  return context?.isRunning ? context.taskRunId : null;
}

/**
 * Hook to check if a task is currently running.
 */
export function useIsTaskRunning(): boolean {
  const context = useContext(TaskContext);
  return context?.isRunning ?? false;
}

/**
 * Hook to get the current task's start time.
 * Returns the task's start time in ms since epoch, or null if no task is running.
 */
export function useTaskStartTime(): number | null {
  const context = useContext(TaskContext);
  return context?.isRunning ? context.taskStartTime : null;
}
