/**
 * useRealtimeUpdates Hook
 *
 * Provides real-time event subscriptions for dashboard components.
 * Manages Tauri event listeners with proper cleanup on unmount.
 */

import { useEffect, useState, useCallback, useRef } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  LearningUpdatePayload,
  CheckpointCreatedPayload,
  TaskStatusChangePayload,
  TaskStatus,
} from "../types/realtimeEvents";
import {
  EVENT_LEARNING_UPDATE,
  EVENT_CHECKPOINT_CREATED,
  EVENT_TASK_STATUS_CHANGE,
} from "../types/realtimeEvents";

// ============================================================================
// Types
// ============================================================================

/** Callback type for learning update events */
export type LearningUpdateCallback = (payload: LearningUpdatePayload) => void;

/** Callback type for checkpoint created events */
export type CheckpointCreatedCallback = (payload: CheckpointCreatedPayload) => void;

/** Callback type for task status change events */
export type TaskStatusChangeCallback = (payload: TaskStatusChangePayload) => void;

/** Options for the useRealtimeUpdates hook */
export interface UseRealtimeUpdatesOptions {
  /** Callback when a learning update event is received */
  onLearningUpdate?: LearningUpdateCallback;
  /** Callback when a checkpoint is created */
  onCheckpointCreated?: CheckpointCreatedCallback;
  /** Callback when task status changes */
  onTaskStatusChange?: TaskStatusChangeCallback;
  /** Whether to enable the listeners (defaults to true) */
  enabled?: boolean;
}

/** Return type for the useRealtimeUpdates hook */
export interface UseRealtimeUpdatesReturn {
  /** Whether currently connected to events */
  isConnected: boolean;
  /** Last learning update received */
  lastLearningUpdate: LearningUpdatePayload | null;
  /** Last checkpoint created */
  lastCheckpointCreated: CheckpointCreatedPayload | null;
  /** Last task status change */
  lastTaskStatusChange: TaskStatusChangePayload | null;
  /** Current running task info (if any) */
  currentTask: CurrentTaskInfo | null;
}

/** Info about the currently running task */
export interface CurrentTaskInfo {
  taskRunId: string;
  status: TaskStatus;
  iteration: number;
  maxIterations: number;
  taskName: string | null;
  startedAt: number;
}

// ============================================================================
// Hook Implementation
// ============================================================================

export function useRealtimeUpdates(
  options: UseRealtimeUpdatesOptions = {},
): UseRealtimeUpdatesReturn {
  const { onLearningUpdate, onCheckpointCreated, onTaskStatusChange, enabled = true } = options;

  const [isConnected, setIsConnected] = useState(false);
  const [lastLearningUpdate, setLastLearningUpdate] = useState<LearningUpdatePayload | null>(null);
  const [lastCheckpointCreated, setLastCheckpointCreated] =
    useState<CheckpointCreatedPayload | null>(null);
  const [lastTaskStatusChange, setLastTaskStatusChange] = useState<TaskStatusChangePayload | null>(
    null,
  );
  const [currentTask, setCurrentTask] = useState<CurrentTaskInfo | null>(null);

  const unlistenRefs = useRef<UnlistenFn[]>([]);

  // Handle learning update event
  const handleLearningUpdate = useCallback(
    (payload: LearningUpdatePayload) => {
      console.log("[useRealtimeUpdates] Learning update:", payload.task_id, payload.status);
      setLastLearningUpdate(payload);
      onLearningUpdate?.(payload);
    },
    [onLearningUpdate],
  );

  // Handle checkpoint created event
  const handleCheckpointCreated = useCallback(
    (payload: CheckpointCreatedPayload) => {
      console.log(
        "[useRealtimeUpdates] Checkpoint created:",
        payload.checkpoint_id,
        payload.task_run_id,
      );
      setLastCheckpointCreated(payload);
      onCheckpointCreated?.(payload);
    },
    [onCheckpointCreated],
  );

  // Handle task status change event
  const handleTaskStatusChange = useCallback(
    (payload: TaskStatusChangePayload) => {
      console.log("[useRealtimeUpdates] Task status change:", payload.task_run_id, payload.status);
      setLastTaskStatusChange(payload);

      // Update current task info
      if (payload.status === "started" || payload.status === "executing") {
        setCurrentTask({
          taskRunId: payload.task_run_id,
          status: payload.status,
          iteration: payload.iteration,
          maxIterations: payload.max_iterations,
          taskName: payload.task_name,
          startedAt: payload.timestamp,
        });
      } else if (
        payload.status === "completed" ||
        payload.status === "failed" ||
        payload.status === "stopped"
      ) {
        // Clear current task when it completes
        setCurrentTask((prev) => (prev?.taskRunId === payload.task_run_id ? null : prev));
      } else {
        // Update status for other changes
        setCurrentTask((prev) =>
          prev?.taskRunId === payload.task_run_id
            ? {
                ...prev,
                status: payload.status,
                iteration: payload.iteration,
              }
            : prev,
        );
      }

      onTaskStatusChange?.(payload);
    },
    [onTaskStatusChange],
  );

  // Setup event listeners
  useEffect(() => {
    if (!enabled) {
      return;
    }

    let isMounted = true;

    const setupListeners = async () => {
      try {
        const unlistenFns: UnlistenFn[] = [];

        // Listen for learning updates
        const unlistenLearning = await listen<LearningUpdatePayload>(
          EVENT_LEARNING_UPDATE,
          (event) => {
            if (isMounted) {
              handleLearningUpdate(event.payload);
            }
          },
        );
        unlistenFns.push(unlistenLearning);

        // Listen for checkpoint created
        const unlistenCheckpoint = await listen<CheckpointCreatedPayload>(
          EVENT_CHECKPOINT_CREATED,
          (event) => {
            if (isMounted) {
              handleCheckpointCreated(event.payload);
            }
          },
        );
        unlistenFns.push(unlistenCheckpoint);

        // Listen for task status changes
        const unlistenTaskStatus = await listen<TaskStatusChangePayload>(
          EVENT_TASK_STATUS_CHANGE,
          (event) => {
            if (isMounted) {
              handleTaskStatusChange(event.payload);
            }
          },
        );
        unlistenFns.push(unlistenTaskStatus);

        if (isMounted) {
          unlistenRefs.current = unlistenFns;
          setIsConnected(true);
          console.log("[useRealtimeUpdates] Connected to realtime events");
        } else {
          // Cleanup if unmounted during setup
          unlistenFns.forEach((fn) => fn());
        }
      } catch (error) {
        console.error("[useRealtimeUpdates] Failed to set up event listeners:", error);
        if (isMounted) {
          setIsConnected(false);
        }
      }
    };

    setupListeners();

    return () => {
      isMounted = false;
      unlistenRefs.current.forEach((fn) => fn());
      unlistenRefs.current = [];
      setIsConnected(false);
    };
  }, [enabled, handleLearningUpdate, handleCheckpointCreated, handleTaskStatusChange]);

  return {
    isConnected,
    lastLearningUpdate,
    lastCheckpointCreated,
    lastTaskStatusChange,
    currentTask,
  };
}

// ============================================================================
// Specialized Hooks
// ============================================================================

/** Options for useLearningUpdates hook */
export interface UseLearningUpdatesOptions {
  /** Callback when a learning update event is received */
  onUpdate?: LearningUpdateCallback;
  /** Whether to enable the listener (defaults to true) */
  enabled?: boolean;
}

/** Return type for useLearningUpdates hook */
export interface UseLearningUpdatesReturn {
  /** Whether currently connected to events */
  isConnected: boolean;
  /** Last learning update received */
  lastUpdate: LearningUpdatePayload | null;
  /** History of learning updates received this session */
  updateHistory: LearningUpdatePayload[];
}

/**
 * Hook specifically for learning update events.
 * Maintains a history of recent updates.
 */
export function useLearningUpdates(
  options: UseLearningUpdatesOptions = {},
): UseLearningUpdatesReturn {
  const { onUpdate, enabled = true } = options;
  const [updateHistory, setUpdateHistory] = useState<LearningUpdatePayload[]>([]);

  const handleUpdate = useCallback(
    (payload: LearningUpdatePayload) => {
      setUpdateHistory((prev) => [...prev.slice(-49), payload]); // Keep last 50
      onUpdate?.(payload);
    },
    [onUpdate],
  );

  const { isConnected, lastLearningUpdate } = useRealtimeUpdates({
    onLearningUpdate: handleUpdate,
    enabled,
  });

  return {
    isConnected,
    lastUpdate: lastLearningUpdate,
    updateHistory,
  };
}

/** Options for useCheckpointUpdates hook */
export interface UseCheckpointUpdatesOptions {
  /** Callback when a checkpoint is created */
  onCheckpointCreated?: CheckpointCreatedCallback;
  /** Filter by task ID (optional) */
  taskId?: string;
  /** Whether to enable the listener (defaults to true) */
  enabled?: boolean;
}

/** Return type for useCheckpointUpdates hook */
export interface UseCheckpointUpdatesReturn {
  /** Whether currently connected to events */
  isConnected: boolean;
  /** Last checkpoint created */
  lastCheckpoint: CheckpointCreatedPayload | null;
  /** Checkpoints created this session */
  checkpointHistory: CheckpointCreatedPayload[];
}

/**
 * Hook specifically for checkpoint created events.
 * Can filter by task ID.
 */
export function useCheckpointUpdates(
  options: UseCheckpointUpdatesOptions = {},
): UseCheckpointUpdatesReturn {
  const { onCheckpointCreated, taskId, enabled = true } = options;
  const [checkpointHistory, setCheckpointHistory] = useState<CheckpointCreatedPayload[]>([]);
  const [lastCheckpoint, setLastCheckpoint] = useState<CheckpointCreatedPayload | null>(null);

  const handleCheckpoint = useCallback(
    (payload: CheckpointCreatedPayload) => {
      // Filter by task ID if specified
      if (taskId && payload.task_run_id !== taskId) {
        return;
      }

      setLastCheckpoint(payload);
      setCheckpointHistory((prev) => [...prev.slice(-49), payload]); // Keep last 50
      onCheckpointCreated?.(payload);
    },
    [onCheckpointCreated, taskId],
  );

  const { isConnected } = useRealtimeUpdates({
    onCheckpointCreated: handleCheckpoint,
    enabled,
  });

  return {
    isConnected,
    lastCheckpoint,
    checkpointHistory,
  };
}

/** Options for useTaskStatusUpdates hook */
export interface UseTaskStatusUpdatesOptions {
  /** Callback when task status changes */
  onStatusChange?: TaskStatusChangeCallback;
  /** Filter by task ID (optional) */
  taskId?: string;
  /** Whether to enable the listener (defaults to true) */
  enabled?: boolean;
}

/** Return type for useTaskStatusUpdates hook */
export interface UseTaskStatusUpdatesReturn {
  /** Whether currently connected to events */
  isConnected: boolean;
  /** Last status change */
  lastStatusChange: TaskStatusChangePayload | null;
  /** Current task info (if any) */
  currentTask: CurrentTaskInfo | null;
  /** Whether a task is currently running */
  isTaskRunning: boolean;
}

/**
 * Hook specifically for task status change events.
 * Tracks the current running task state.
 */
export function useTaskStatusUpdates(
  options: UseTaskStatusUpdatesOptions = {},
): UseTaskStatusUpdatesReturn {
  const { onStatusChange, taskId, enabled = true } = options;

  const handleStatusChange = useCallback(
    (payload: TaskStatusChangePayload) => {
      // Filter by task ID if specified
      if (taskId && payload.task_run_id !== taskId) {
        return;
      }
      onStatusChange?.(payload);
    },
    [onStatusChange, taskId],
  );

  const { isConnected, lastTaskStatusChange, currentTask } = useRealtimeUpdates({
    onTaskStatusChange: handleStatusChange,
    enabled,
  });

  return {
    isConnected,
    lastStatusChange: lastTaskStatusChange,
    currentTask,
    isTaskRunning: currentTask !== null,
  };
}
