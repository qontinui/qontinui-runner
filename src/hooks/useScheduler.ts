/**
 * useScheduler
 *
 * Hook for managing scheduled tasks in the CI/CD scheduler.
 * Provides CRUD operations, status monitoring, and task execution.
 */

import { useState, useEffect, useCallback, useRef } from "react";
import type {
  ScheduledTask,
  TaskExecutionRecord,
  SchedulerSettings,
  SchedulerStatus,
  CreateScheduledTaskRequest,
  UpdateScheduledTaskRequest,
} from "../types/scheduler";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

interface ApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

interface UseSchedulerReturn {
  // State
  tasks: ScheduledTask[];
  settings: SchedulerSettings | null;
  status: SchedulerStatus | null;
  selectedTask: ScheduledTask | null;
  taskHistory: TaskExecutionRecord[];
  loading: boolean;
  error: string | null;

  // Actions
  loadTasks: () => Promise<void>;
  loadSettings: () => Promise<void>;
  loadStatus: () => Promise<void>;
  createTask: (request: CreateScheduledTaskRequest) => Promise<ScheduledTask | null>;
  updateTask: (id: string, request: UpdateScheduledTaskRequest) => Promise<ScheduledTask | null>;
  deleteTask: (id: string) => Promise<boolean>;
  runTaskNow: (id: string) => Promise<boolean>;
  loadTaskHistory: (id: string) => Promise<void>;
  selectTask: (task: ScheduledTask | null) => void;
  updateSettings: (settings: SchedulerSettings) => Promise<boolean>;
  triggerAutoFix: () => Promise<boolean>;
  refresh: () => Promise<void>;
}

/**
 * Hook to manage the CI/CD scheduler
 */
export function useScheduler(autoRefresh = true, refreshInterval = 30000): UseSchedulerReturn {
  const [tasks, setTasks] = useState<ScheduledTask[]>([]);
  const [settings, setSettings] = useState<SchedulerSettings | null>(null);
  const [status, setStatus] = useState<SchedulerStatus | null>(null);
  const [selectedTask, setSelectedTask] = useState<ScheduledTask | null>(null);
  const [taskHistory, setTaskHistory] = useState<TaskExecutionRecord[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const refreshTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  /**
   * Make an API request
   */
  const apiRequest = async <T>(
    method: string,
    endpoint: string,
    body?: unknown,
  ): Promise<T | null> => {
    try {
      const options: RequestInit = {
        method,
        headers: {
          "Content-Type": "application/json",
        },
      };

      if (body) {
        options.body = JSON.stringify(body);
      }

      const response = await tracedFetch(`${getApiBase()}${endpoint}`, options);
      const result: ApiResponse<T> = await response.json();

      if (!result.success) {
        throw new Error(result.error || "API request failed");
      }

      return result.data ?? null;
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      console.error(`[SCHEDULER] API error: ${message}`);
      throw err;
    }
  };

  /**
   * Load all scheduled tasks
   */
  const loadTasks = useCallback(async () => {
    try {
      const result = await apiRequest<ScheduledTask[]>("GET", "/scheduler/tasks");
      setTasks(result || []);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to load tasks");
    }
  }, []);

  /**
   * Load scheduler settings
   */
  const loadSettings = useCallback(async () => {
    try {
      const result = await apiRequest<SchedulerSettings>("GET", "/scheduler/settings");
      setSettings(result);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to load settings");
    }
  }, []);

  /**
   * Load scheduler status
   */
  const loadStatus = useCallback(async () => {
    try {
      const result = await apiRequest<SchedulerStatus>("GET", "/scheduler/status");
      setStatus(result);
    } catch (err) {
      // Don't show error for status polling
      console.warn("[SCHEDULER] Failed to load status:", err);
    }
  }, []);

  /**
   * Create a new scheduled task
   */
  const createTask = useCallback(
    async (request: CreateScheduledTaskRequest): Promise<ScheduledTask | null> => {
      setLoading(true);
      setError(null);
      try {
        const result = await apiRequest<ScheduledTask>("POST", "/scheduler/tasks", request);
        if (result) {
          setTasks((prev) => [...prev, result]);
        }
        return result;
      } catch (err) {
        setError(err instanceof Error ? err.message : "Failed to create task");
        return null;
      } finally {
        setLoading(false);
      }
    },
    [],
  );

  /**
   * Update an existing task
   */
  const updateTask = useCallback(
    async (id: string, request: UpdateScheduledTaskRequest): Promise<ScheduledTask | null> => {
      setLoading(true);
      setError(null);
      try {
        const result = await apiRequest<ScheduledTask>("PUT", `/scheduler/tasks/${id}`, request);
        if (result) {
          setTasks((prev) => prev.map((t) => (t.id === id ? result : t)));
          if (selectedTask?.id === id) {
            setSelectedTask(result);
          }
        }
        return result;
      } catch (err) {
        setError(err instanceof Error ? err.message : "Failed to update task");
        return null;
      } finally {
        setLoading(false);
      }
    },
    [selectedTask],
  );

  /**
   * Delete a task
   */
  const deleteTask = useCallback(
    async (id: string): Promise<boolean> => {
      setLoading(true);
      setError(null);
      try {
        await apiRequest("DELETE", `/scheduler/tasks/${id}`);
        setTasks((prev) => prev.filter((t) => t.id !== id));
        if (selectedTask?.id === id) {
          setSelectedTask(null);
          setTaskHistory([]);
        }
        return true;
      } catch (err) {
        setError(err instanceof Error ? err.message : "Failed to delete task");
        return false;
      } finally {
        setLoading(false);
      }
    },
    [selectedTask],
  );

  /**
   * Run a task immediately
   */
  const runTaskNow = useCallback(
    async (id: string): Promise<boolean> => {
      setLoading(true);
      setError(null);
      try {
        await apiRequest("POST", `/scheduler/tasks/${id}/run`);
        // Refresh tasks to get updated last_run
        await loadTasks();
        return true;
      } catch (err) {
        setError(err instanceof Error ? err.message : "Failed to run task");
        return false;
      } finally {
        setLoading(false);
      }
    },
    [loadTasks],
  );

  /**
   * Load execution history for a task
   */
  const loadTaskHistory = useCallback(async (id: string) => {
    try {
      const result = await apiRequest<TaskExecutionRecord[]>(
        "GET",
        `/scheduler/tasks/${id}/history`,
      );
      setTaskHistory(result || []);
    } catch (err) {
      console.warn("[SCHEDULER] Failed to load task history:", err);
      setTaskHistory([]);
    }
  }, []);

  /**
   * Select a task (also loads its history)
   */
  const selectTask = useCallback(
    (task: ScheduledTask | null) => {
      setSelectedTask(task);
      if (task) {
        loadTaskHistory(task.id);
      } else {
        setTaskHistory([]);
      }
    },
    [loadTaskHistory],
  );

  /**
   * Update scheduler settings
   */
  const updateSettings = useCallback(async (newSettings: SchedulerSettings): Promise<boolean> => {
    setLoading(true);
    setError(null);
    try {
      await apiRequest("PUT", "/scheduler/settings", newSettings);
      setSettings(newSettings);
      return true;
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to update settings");
      return false;
    } finally {
      setLoading(false);
    }
  }, []);

  /**
   * Trigger auto-fix manually
   */
  const triggerAutoFix = useCallback(async (): Promise<boolean> => {
    setLoading(true);
    setError(null);
    try {
      await apiRequest("POST", "/scheduler/trigger-auto-fix");
      return true;
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to trigger auto-fix");
      return false;
    } finally {
      setLoading(false);
    }
  }, []);

  /**
   * Refresh all data
   */
  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      await Promise.all([loadTasks(), loadSettings(), loadStatus()]);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to refresh");
    } finally {
      setLoading(false);
    }
  }, [loadTasks, loadSettings, loadStatus]);

  // Initial load
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    refresh();
  }, [refresh]);

  // Auto-refresh status
  useEffect(() => {
    if (!autoRefresh) return;

    refreshTimerRef.current = setInterval(() => {
      loadStatus();
      // Also refresh tasks to get updated last_run
      loadTasks();
    }, refreshInterval);

    return () => {
      if (refreshTimerRef.current) {
        clearInterval(refreshTimerRef.current);
      }
    };
  }, [autoRefresh, refreshInterval, loadStatus, loadTasks]);

  return {
    tasks,
    settings,
    status,
    selectedTask,
    taskHistory,
    loading,
    error,
    loadTasks,
    loadSettings,
    loadStatus,
    createTask,
    updateTask,
    deleteTask,
    runTaskNow,
    loadTaskHistory,
    selectTask,
    updateSettings,
    triggerAutoFix,
    refresh,
  };
}
