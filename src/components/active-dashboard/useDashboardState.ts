/**
 * useDashboardState Hook
 *
 * Provides state management for the Active Dashboard.
 * Aggregates execution state, action logs, image recognition results,
 * and screenshot data for the live execution view.
 *
 * Data Sources:
 * - ExecutionContext: workflow status, config, selected workflow
 * - HTTP API: running tasks, action logs, screenshots
 * - Event system: real-time updates (future)
 */

import { useState, useEffect, useCallback, useMemo } from "react";
import { useExecution } from "../../contexts";
import type { ExecutionStatus, ExecutionState, ActionItem, ScreenshotsResult } from "./types";

const API_BASE = "http://localhost:9876";

interface RunningTask {
  id: string;
  task_name: string;
  status: string;
  created_at: string;
}

interface ActionLogResponse {
  actions: Array<{
    id: string;
    action_type: string;
    status: string;
    timestamp: number;
    end_timestamp?: number;
    duration?: number;
    target?: string;
    result?: string;
    error?: string;
    level: number;
    parent_action_id?: string | null;
    is_expandable?: boolean;
    metadata?: Record<string, unknown>;
  }>;
  visible_count: number;
  workflow_start_time?: number;
}

export interface DashboardState {
  // Execution state
  executionState: ExecutionState;
  // Actions for the action stream
  actions: ActionItem[];
  // Current action being executed
  currentAction: string | null;
  // Whether any task is running (GUI or AI)
  isRunning: boolean;
  // AI task info if running
  aiTaskName: string | null;
  aiTaskId: string | null;
  // Control functions
  onPlayPause: () => void;
  onStop: () => void;
  // Refresh function
  refresh: () => void;
  // Loading state
  loading: boolean;
  // Error state
  error: string | null;
}

/**
 * Hook for managing the active dashboard state
 */
export function useDashboardState(): DashboardState {
  const execution = useExecution();

  // Local state
  const [runningTasks, setRunningTasks] = useState<RunningTask[]>([]);
  const [actionLogData, setActionLogData] = useState<ActionLogResponse | null>(null);
  const [screenshots, setScreenshots] = useState<ScreenshotsResult>({
    annotated: [],
    playwright: [],
  });
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [elapsedTime, setElapsedTime] = useState(0);
  const [startTime, setStartTime] = useState<number | null>(null);

  // Fetch running tasks
  const fetchRunningTasks = useCallback(async () => {
    try {
      const response = await fetch(`${API_BASE}/task-runs/running`);
      if (response.ok) {
        const tasks = await response.json();
        setRunningTasks(Array.isArray(tasks) ? tasks : []);
      }
    } catch {
      // Silently ignore - API may not be available
    }
  }, []);

  // Fetch action log
  const fetchActionLog = useCallback(async () => {
    try {
      const response = await fetch(`${API_BASE}/action-log/view`);
      if (response.ok) {
        const data = await response.json();
        setActionLogData(data);
        if (data.workflow_start_time) {
          setStartTime(data.workflow_start_time);
        }
      }
    } catch {
      // Silently ignore
    }
  }, []);

  // Fetch screenshots
  const fetchScreenshots = useCallback(async () => {
    try {
      const response = await fetch(`${API_BASE}/screenshots/list`);
      if (response.ok) {
        const data = await response.json();
        if (data.success) {
          setScreenshots(data.data || { annotated: [], playwright: [] });
        }
      }
    } catch {
      // Silently ignore
    }
  }, []);

  // Combined refresh function
  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      await Promise.all([fetchRunningTasks(), fetchActionLog(), fetchScreenshots()]);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to fetch dashboard data");
    } finally {
      setLoading(false);
    }
  }, [fetchRunningTasks, fetchActionLog, fetchScreenshots]);

  // Initial fetch and polling
  useEffect(() => {
    refresh();
    const interval = setInterval(refresh, 2000);
    return () => clearInterval(interval);
  }, [refresh]);

  // Update elapsed time
  useEffect(() => {
    if (!startTime) {
      setElapsedTime(0);
      return;
    }

    const updateElapsed = () => {
      const now = Date.now() / 1000;
      setElapsedTime(Math.floor(now - startTime));
    };

    updateElapsed();
    const interval = setInterval(updateElapsed, 1000);
    return () => clearInterval(interval);
  }, [startTime]);

  // Determine execution status
  const status: ExecutionStatus = useMemo(() => {
    if (runningTasks.length > 0) return "running";
    if (execution.executionActive) return "running";
    return "idle";
  }, [runningTasks.length, execution.executionActive]);

  // Map action log data to ActionItem[]
  const actions: ActionItem[] = useMemo(() => {
    if (!actionLogData?.actions) return [];
    return actionLogData.actions.map((action) => ({
      id: action.id,
      action_type: action.action_type,
      status: action.status as ActionItem["status"],
      timestamp: action.timestamp,
      end_timestamp: action.end_timestamp,
      duration: action.duration,
      target: action.target,
      result: action.result,
      error: action.error,
      level: action.level,
      parent_action_id: action.parent_action_id,
      is_expandable: action.is_expandable,
      metadata: action.metadata,
    }));
  }, [actionLogData]);

  // Get current action
  const currentAction = useMemo(() => {
    const runningAction = actions.find((a) => a.status === "running");
    if (runningAction) {
      return `${runningAction.action_type}: ${runningAction.target || ""}`;
    }
    return null;
  }, [actions]);

  // Calculate statistics
  const stats = useMemo(() => {
    if (actions.length === 0) {
      return {
        actionsCompleted: 0,
        successRate: 100,
        averageConfidence: 0,
      };
    }

    const completed = actions.filter(
      (a) => a.status === "success" || a.status === "failed" || a.status === "error",
    ).length;
    const successful = actions.filter((a) => a.status === "success").length;
    const successRate = completed > 0 ? (successful / completed) * 100 : 100;

    return {
      actionsCompleted: completed,
      successRate,
      averageConfidence: 85, // Placeholder - would come from image recognition data
    };
  }, [actions]);

  // Build execution state
  const executionState: ExecutionState = useMemo(
    () => ({
      status,
      workflowName:
        runningTasks.length > 0 ? runningTasks[0].task_name : execution.selectedWorkflow || null,
      currentTransition: null,
      currentAction,
      elapsedTime,
      actionsCompleted: stats.actionsCompleted,
      successRate: stats.successRate,
      averageConfidence: stats.averageConfidence,
      iteration: 1,
      maxIterations: 1,
      screenshots,
    }),
    [
      status,
      runningTasks,
      execution.selectedWorkflow,
      currentAction,
      elapsedTime,
      stats,
      screenshots,
    ],
  );

  // Control handlers
  const onPlayPause = useCallback(async () => {
    // TODO: Implement pause/resume functionality
    console.log("[DashboardState] Play/Pause not yet implemented");
  }, []);

  const onStop = useCallback(async () => {
    try {
      // Stop AI tasks
      if (runningTasks.length > 0) {
        await fetch(`${API_BASE}/stop-ai-analysis`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
        });
      }
      // Stop GUI execution
      if (execution.executionActive) {
        execution.stopExecution();
      }
      // Refresh to update state
      setTimeout(refresh, 500);
    } catch (e) {
      console.error("[DashboardState] Failed to stop:", e);
    }
  }, [runningTasks, execution, refresh]);

  return {
    executionState,
    actions,
    currentAction,
    isRunning: status === "running",
    aiTaskName: runningTasks.length > 0 ? runningTasks[0].task_name : null,
    aiTaskId: runningTasks.length > 0 ? runningTasks[0].id : null,
    onPlayPause,
    onStop,
    refresh,
    loading,
    error,
  };
}

export default useDashboardState;
