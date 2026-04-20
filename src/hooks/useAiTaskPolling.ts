/**
 * useAiTaskPolling Hook
 *
 * Provides async AI task triggering with polling for completion.
 * Uses /prompts/run endpoint for async task execution.
 */

import { useState, useCallback, useRef, useEffect } from "react";
import type { TaskRun, RunPromptRequest, RunPromptResponse } from "../types/taskRun";
import { isTaskFinished } from "../types/taskRun";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { useTaskRunProgress } from "@/hooks/graphql";

// Polling is now fallback only - GraphQL subscription is primary
const DEFAULT_POLL_INTERVAL_MS = 15000; // Relaxed from 5s since subscription handles real-time
const DEFAULT_TIMEOUT_MS = 600000; // 10 minutes

export interface UseAiTaskPollingOptions {
  /** Polling interval in milliseconds (default: 5000, fallback since real-time events are primary) */
  pollIntervalMs?: number;
  /** Timeout in milliseconds (default: 600000 = 10 minutes) */
  timeoutMs?: number;
  /** Callback when task completes successfully */
  onComplete?: (task: TaskRun) => void;
  /** Callback when task fails */
  onError?: (error: string, task?: TaskRun) => void;
  /** Callback on each poll update */
  onProgress?: (task: TaskRun) => void;
}

export interface UseAiTaskPollingResult {
  /** Whether a task is currently running */
  isRunning: boolean;
  /** Current task run data (if any) */
  currentTask: TaskRun | null;
  /** Last error message */
  error: string | null;
  /** Trigger a new AI analysis task */
  triggerTask: (request: RunPromptRequest) => Promise<TaskRun | null>;
  /** Stop polling for current task (doesn't stop the task itself) */
  stopPolling: () => void;
  /** Stop the current AI task */
  stopTask: () => Promise<boolean>;
}

/**
 * Hook for triggering and polling AI tasks
 */
export function useAiTaskPolling(options: UseAiTaskPollingOptions = {}): UseAiTaskPollingResult {
  const {
    pollIntervalMs = DEFAULT_POLL_INTERVAL_MS,
    timeoutMs = DEFAULT_TIMEOUT_MS,
    onComplete,
    onError,
    onProgress,
  } = options;

  const [isRunning, setIsRunning] = useState(false);
  const [currentTask, setCurrentTask] = useState<TaskRun | null>(null);
  const [error, setError] = useState<string | null>(null);

  const pollIntervalRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const startTimeRef = useRef<number>(0);
  const taskIdRef = useRef<string | null>(null);
  const isMountedRef = useRef(true);
  const [activeTaskId, setActiveTaskId] = useState<string>("");

  // Stable refs for callbacks (avoids re-subscribing on every render)
  const onCompleteRef = useRef(onComplete);
  const onErrorRef = useRef(onError);
  const onProgressRef = useRef(onProgress);
  useEffect(() => {
    onCompleteRef.current = onComplete;
    onErrorRef.current = onError;
    onProgressRef.current = onProgress;
  }, [onComplete, onError, onProgress]);

  // GraphQL subscription for real-time task progress (replaces frequent polling).
  // The subscription streams events for the active task; polling is kept as a
  // slow fallback (15s) in case the WS connection drops.
  const { data: progressData } = useTaskRunProgress(activeTaskId, !activeTaskId);

  /**
   * Fetch task run status by ID
   */
  const fetchTaskRun = useCallback(async (taskId: string): Promise<TaskRun | null> => {
    try {
      const response = await tracedFetch(`${getApiBase()}/task-runs/${taskId}`);
      if (!response.ok) {
        console.error(`[useAiTaskPolling] Failed to fetch task ${taskId}: ${response.status}`);
        return null;
      }
      const result = await response.json();
      if (result.success && result.data) {
        return result.data as TaskRun;
      }
      return null;
    } catch (err) {
      console.error(`[useAiTaskPolling] Error fetching task ${taskId}:`, err);
      return null;
    }
  }, []);

  // React to subscription events — detect completion/status changes instantly
  useEffect(() => {
    const event = progressData?.taskRunProgress;
    if (!event || !activeTaskId || !isMountedRef.current) return;

    // TaskRunUpdate events carry status changes
    if (event.__typename === "TaskRunUpdateEvent" && event.status) {
      const taskStatus = event.status;
      if (taskStatus === "complete" || taskStatus === "failed" || taskStatus === "stopped") {
        // Fetch final state and trigger callbacks
        fetchTaskRun(activeTaskId).then((task) => {
          if (!task || !isMountedRef.current) return;
          setCurrentTask(task);
          if (pollIntervalRef.current) {
            clearInterval(pollIntervalRef.current);
            pollIntervalRef.current = null;
          }
          setIsRunning(false);
          setActiveTaskId("");
          taskIdRef.current = null;
          if (taskStatus === "complete") {
            onCompleteRef.current?.(task);
          } else {
            const errorMsg = task.errorMessage || `Task ${taskStatus}`;
            setError(errorMsg);
            onErrorRef.current?.(errorMsg, task);
          }
        });
      }
    }

    // StepProgress events — update current task state
    if (event.__typename === "StepProgressEvent") {
      fetchTaskRun(activeTaskId).then((task) => {
        if (task && isMountedRef.current) {
          setCurrentTask(task);
          onProgressRef.current?.(task);
        }
      });
    }
  }, [progressData, activeTaskId, fetchTaskRun]);

  /**
   * Stop polling for current task
   */
  const stopPolling = useCallback(() => {
    if (pollIntervalRef.current) {
      clearInterval(pollIntervalRef.current);
      pollIntervalRef.current = null;
    }
    taskIdRef.current = null;
    setActiveTaskId("");
    if (isMountedRef.current) {
      setIsRunning(false);
    }
  }, []);

  // Cleanup on unmount — cancel polling and mark as unmounted
  useEffect(() => {
    return () => {
      isMountedRef.current = false;
      if (pollIntervalRef.current) {
        clearInterval(pollIntervalRef.current);
      }
    };
  }, []);

  /**
   * Poll for task completion
   */
  const startPolling = useCallback(
    (taskId: string) => {
      taskIdRef.current = taskId;
      setActiveTaskId(taskId); // Activate GraphQL subscription
      startTimeRef.current = Date.now();

      const poll = async () => {
        if (!isMountedRef.current || taskIdRef.current !== taskId) {
          return;
        }

        // Check timeout
        if (Date.now() - startTimeRef.current > timeoutMs) {
          stopPolling();
          const timeoutError = "Task polling timed out";
          setError(timeoutError);
          onErrorRef.current?.(timeoutError);
          return;
        }

        const task = await fetchTaskRun(taskId);
        if (!task || !isMountedRef.current || taskIdRef.current !== taskId) {
          return;
        }

        setCurrentTask(task);
        onProgressRef.current?.(task);

        if (isTaskFinished(task)) {
          stopPolling();
          if (task.status === "complete") {
            onCompleteRef.current?.(task);
          } else {
            const errorMsg = task.errorMessage || `Task ${task.status}`;
            setError(errorMsg);
            onErrorRef.current?.(errorMsg, task);
          }
        }
      };

      // Initial poll
      poll();

      // Set up interval
      pollIntervalRef.current = setInterval(poll, pollIntervalMs);
    },
    [fetchTaskRun, pollIntervalMs, timeoutMs, stopPolling],
  );

  /**
   * Trigger a new AI analysis task
   */
  const triggerTask = useCallback(
    async (request: RunPromptRequest): Promise<TaskRun | null> => {
      // Stop any existing polling
      stopPolling();
      setError(null);
      setIsRunning(true);

      try {
        const response = await tracedFetch(`${getApiBase()}/prompts/run`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(request),
        });

        const result: RunPromptResponse = await response.json();

        if (!result.success) {
          const errorMsg = result.error || "Failed to trigger AI analysis";
          setError(errorMsg);
          setIsRunning(false);
          onErrorRef.current?.(errorMsg);
          return null;
        }

        // Check for task_run_id (async mode)
        if (result.taskRunId) {
          // Fetch initial task state and start polling
          const task = await fetchTaskRun(result.taskRunId);
          if (task) {
            setCurrentTask(task);
            startPolling(result.taskRunId);
            return task;
          } else {
            const errorMsg = "Failed to fetch initial task state";
            setError(errorMsg);
            setIsRunning(false);
            onErrorRef.current?.(errorMsg);
            return null;
          }
        }

        // Legacy: synchronous response (backward compatibility)
        // Create a synthetic completed task
        const output = result.output || result.data?.output || result.data?.response || "";
        const syntheticTask: TaskRun = {
          id: `legacy-${Date.now()}`,
          taskName: request.displayPrompt || "AI Analysis",
          prompt: request.content,
          taskType: "task",
          status: "complete",
          sessionsCount: 1,
          autoContinue: true,
          outputLog: output,
          createdAt: new Date().toISOString(),
          updatedAt: new Date().toISOString(),
          completedAt: new Date().toISOString(),
        };
        setCurrentTask(syntheticTask);
        setIsRunning(false);
        onCompleteRef.current?.(syntheticTask);
        return syntheticTask;
      } catch (err) {
        const errorMsg = err instanceof Error ? err.message : String(err);
        setError(errorMsg);
        setIsRunning(false);
        onErrorRef.current?.(errorMsg);
        return null;
      }
    },
    [stopPolling, fetchTaskRun, startPolling],
  );

  /**
   * Stop the current AI task
   */
  const stopTask = useCallback(async (): Promise<boolean> => {
    const taskId = taskIdRef.current;
    if (!taskId) {
      // No task running, try to stop via legacy endpoint
      try {
        const response = await tracedFetch(`${getApiBase()}/stop-ai-analysis`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
        });
        const result = await response.json();
        stopPolling();
        return result.success;
      } catch {
        return false;
      }
    }

    try {
      const response = await tracedFetch(`${getApiBase()}/task-runs/${taskId}/stop`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
      });
      const result = await response.json();
      if (result.success) {
        stopPolling();
      }
      return result.success;
    } catch {
      return false;
    }
  }, [stopPolling]);

  return {
    isRunning,
    currentTask,
    error,
    triggerTask,
    stopPolling,
    stopTask,
  };
}

/**
 * Utility function for one-shot AI task execution with polling
 * Returns the completed task or throws an error
 */
export async function executeAiTask(
  request: RunPromptRequest,
  options: {
    pollIntervalMs?: number;
    timeoutMs?: number;
  } = {},
): Promise<TaskRun> {
  const { pollIntervalMs = DEFAULT_POLL_INTERVAL_MS, timeoutMs = DEFAULT_TIMEOUT_MS } = options;

  // Trigger the task
  const triggerResponse = await tracedFetch(`${getApiBase()}/prompts/run`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(request),
  });

  const result: RunPromptResponse = await triggerResponse.json();

  if (!result.success) {
    throw new Error(result.error || "Failed to trigger AI analysis");
  }

  // Handle async mode
  if (result.taskRunId) {
    const startTime = Date.now();
    const taskId = result.taskRunId;

    // Poll until complete or timeout
    while (Date.now() - startTime < timeoutMs) {
      const taskResponse = await tracedFetch(`${getApiBase()}/task-runs/${taskId}`);
      if (!taskResponse.ok) {
        throw new Error(`Failed to fetch task: ${taskResponse.status}`);
      }

      const taskResult = await taskResponse.json();
      if (!taskResult.success || !taskResult.data) {
        throw new Error("Invalid task response");
      }

      const task = taskResult.data as TaskRun;

      if (isTaskFinished(task)) {
        if (task.status === "complete") {
          return task;
        }
        throw new Error(task.errorMessage || `Task ${task.status}`);
      }

      // Wait before next poll
      await new Promise((resolve) => setTimeout(resolve, pollIntervalMs));
    }

    throw new Error("Task polling timed out");
  }

  // Legacy: synchronous response
  const output = result.output || result.data?.output || result.data?.response || "";
  return {
    id: `legacy-${Date.now()}`,
    taskName: request.displayPrompt || "AI Analysis",
    prompt: request.content,
    taskType: "task" as const,
    status: "complete" as const,
    sessionsCount: 1,
    autoContinue: true,
    outputLog: output,
    createdAt: new Date().toISOString(),
    updatedAt: new Date().toISOString(),
    completedAt: new Date().toISOString(),
  };
}
