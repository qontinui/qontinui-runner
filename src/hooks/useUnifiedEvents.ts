/**
 * useUnifiedEvents Hook
 *
 * Provides a unified event subscription system that automatically uses:
 * - Tauri events when running in a Tauri desktop app
 * - WebSocket events when running in a browser or external client
 *
 * Note: GraphQL subscriptions (useRunnerEvents, useTaskRunProgress) now provide
 * an additional event transport layer that works in both environments.
 * Components that only need cache invalidation on status changes can use
 * the GraphQL hooks directly instead of this hook. This hook remains
 * for components that need the full event payload (orchestrator state, step details).
 *
 * This enables the same frontend code to work seamlessly in both contexts.
 *
 * Usage:
 * ```typescript
 * const { isConnected, workflowStage, iteration, phase } = useUnifiedOrchestratorState({
 *   taskId: "task-123",
 *   onStateChange: (state) => console.log("State changed:", state),
 * });
 * ```
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { createLogger } from "@/lib/logger";

const logger = createLogger("UnifiedEvents");
import {
  useWebSocketEvents,
  useWsOrchestratorState,
  isTauriEnvironment,
  type OrchestratorStateChangePayload,
  type StepProgressPayload,
  type TaskRunUpdatePayload,
  type ExecutorEventPayload,
  type OrchestratorStateChangeCallback,
  type StepProgressCallback,
  type TaskRunUpdateCallback,
  type ExecutorEventCallback,
} from "./useWebSocketEvents";
import { useWebSocketLatency } from "./useWebSocketLatency";

// Re-export types for convenience
export type {
  OrchestratorStateChangePayload,
  StepProgressPayload,
  TaskRunUpdatePayload,
  ExecutorEventPayload,
  OrchestratorStateChangeCallback,
  StepProgressCallback,
  TaskRunUpdateCallback,
  ExecutorEventCallback,
};
export { isTauriEnvironment };

// ============================================================================
// Types
// ============================================================================

/** Options for the useUnifiedEvents hook */
export interface UseUnifiedEventsOptions {
  /** Whether to enable event listening (defaults to true) */
  enabled?: boolean;
  /** Callback when orchestrator state changes */
  onOrchestratorStateChange?: OrchestratorStateChangeCallback;
  /** Callback when step progress updates */
  onStepProgress?: StepProgressCallback;
  /** Callback when task run updates */
  onTaskRunUpdate?: TaskRunUpdateCallback;
  /** Callback for executor events */
  onExecutorEvent?: ExecutorEventCallback;
  /** Callback when connection is established */
  onConnected?: () => void;
  /** Callback when connection is lost */
  onDisconnected?: () => void;
}

/** Return type for the useUnifiedEvents hook */
export interface UseUnifiedEventsReturn {
  /** Whether events are connected (Tauri or WebSocket) */
  isConnected: boolean;
  /** Whether running in Tauri environment */
  isTauri: boolean;
  /** Connection method: "tauri" or "websocket" */
  connectionMethod: "tauri" | "websocket";
  /** Last orchestrator state received */
  lastOrchestratorState: OrchestratorStateChangePayload | null;
  /** Last step progress received */
  lastStepProgress: StepProgressPayload | null;
  /** Last task run update received */
  lastTaskRunUpdate: TaskRunUpdatePayload | null;
}

// ============================================================================
// Hook Implementation
// ============================================================================

export function useUnifiedEvents(options: UseUnifiedEventsOptions = {}): UseUnifiedEventsReturn {
  const {
    enabled = true,
    onOrchestratorStateChange,
    onStepProgress,
    onTaskRunUpdate,
    onExecutorEvent,
    onConnected,
    onDisconnected,
  } = options;

  const isTauri = isTauriEnvironment();

  // Latency tracking (dev mode only)
  const { recordMessage, setConnected: setLatencyConnected } = useWebSocketLatency();

  // Wrap callbacks to record latency from server timestamps
  const wrappedOnOrchestratorStateChange = useCallback(
    (payload: OrchestratorStateChangePayload) => {
      // OrchestratorStateChangePayload has no timestamp field — record without latency
      recordMessage();
      onOrchestratorStateChange?.(payload);
    },
    [onOrchestratorStateChange, recordMessage],
  );

  const wrappedOnStepProgress = useCallback(
    (payload: StepProgressPayload) => {
      recordMessage(payload.data.timestamp);
      onStepProgress?.(payload);
    },
    [onStepProgress, recordMessage],
  );

  const wrappedOnTaskRunUpdate = useCallback(
    (payload: TaskRunUpdatePayload) => {
      recordMessage(payload.data.timestamp);
      onTaskRunUpdate?.(payload);
    },
    [onTaskRunUpdate, recordMessage],
  );

  const wrappedOnExecutorEvent = useCallback(
    (payload: ExecutorEventPayload) => {
      recordMessage(payload.data.timestamp);
      onExecutorEvent?.(payload);
    },
    [onExecutorEvent, recordMessage],
  );

  const wrappedOnConnected = useCallback(() => {
    setLatencyConnected(true);
    onConnected?.();
  }, [onConnected, setLatencyConnected]);

  const wrappedOnDisconnected = useCallback(() => {
    setLatencyConnected(false);
    onDisconnected?.();
  }, [onDisconnected, setLatencyConnected]);

  // State for Tauri events
  const [tauriConnected, setTauriConnected] = useState(false);
  const [lastOrchestratorStateTauri, setLastOrchestratorStateTauri] =
    useState<OrchestratorStateChangePayload | null>(null);
  const [lastStepProgressTauri, setLastStepProgressTauri] = useState<StepProgressPayload | null>(
    null,
  );
  const [lastTaskRunUpdateTauri, setLastTaskRunUpdateTauri] = useState<TaskRunUpdatePayload | null>(
    null,
  );

  const unlistenRefs = useRef<UnlistenFn[]>([]);

  // Set up Tauri event listeners
  useEffect(() => {
    if (!isTauri || !enabled) {
      return;
    }

    let mounted = true;
    const setupListeners = async () => {
      try {
        const unlistenFns: UnlistenFn[] = [];

        // Listen for orchestrator state changes
        const unlistenOrchestrator = await listen<OrchestratorStateChangePayload>(
          "orchestrator-state-change",
          (event) => {
            if (!mounted) return;
            setLastOrchestratorStateTauri(event.payload);
            wrappedOnOrchestratorStateChange(event.payload);
          },
        );
        unlistenFns.push(unlistenOrchestrator);

        // Listen for step progress
        const unlistenStepProgress = await listen<StepProgressPayload>("step-progress", (event) => {
          if (!mounted) return;
          setLastStepProgressTauri(event.payload);
          wrappedOnStepProgress(event.payload);
        });
        unlistenFns.push(unlistenStepProgress);

        // Listen for task run updates
        const unlistenTaskRunUpdate = await listen<TaskRunUpdatePayload>(
          "task-run-update",
          (event) => {
            if (!mounted) return;
            setLastTaskRunUpdateTauri(event.payload);
            wrappedOnTaskRunUpdate(event.payload);
          },
        );
        unlistenFns.push(unlistenTaskRunUpdate);

        // Listen for executor events
        const unlistenExecutorEvent = await listen<ExecutorEventPayload>(
          "executor-event",
          (event) => {
            if (!mounted) return;
            wrappedOnExecutorEvent(event.payload);
          },
        );
        unlistenFns.push(unlistenExecutorEvent);

        if (mounted) {
          unlistenRefs.current = unlistenFns;
          setTauriConnected(true);
          wrappedOnConnected();
          logger.info("Tauri event listeners connected");
        } else {
          // Cleanup if unmounted during setup
          unlistenFns.forEach((fn) => fn());
        }
      } catch (error) {
        console.error("[useUnifiedEvents] Failed to set up Tauri listeners:", error);
        if (mounted) {
          setTauriConnected(false);
        }
      }
    };

    setupListeners();

    return () => {
      mounted = false;
      unlistenRefs.current.forEach((fn) => fn());
      unlistenRefs.current = [];
      setTauriConnected(false);
      wrappedOnDisconnected();
    };
  }, [
    isTauri,
    enabled,
    wrappedOnOrchestratorStateChange,
    wrappedOnStepProgress,
    wrappedOnTaskRunUpdate,
    wrappedOnExecutorEvent,
    wrappedOnConnected,
    wrappedOnDisconnected,
  ]);

  // Use WebSocket events when not in Tauri
  const {
    isConnected: wsConnected,
    lastOrchestratorState: lastOrchestratorStateWs,
    lastStepProgress: lastStepProgressWs,
    lastTaskRunUpdate: lastTaskRunUpdateWs,
  } = useWebSocketEvents({
    enabled: enabled && !isTauri,
    onOrchestratorStateChange: wrappedOnOrchestratorStateChange,
    onStepProgress: wrappedOnStepProgress,
    onTaskRunUpdate: wrappedOnTaskRunUpdate,
    onExecutorEvent: wrappedOnExecutorEvent,
    onConnected: wrappedOnConnected,
    onDisconnected: wrappedOnDisconnected,
  });

  // Return appropriate values based on environment
  if (isTauri) {
    return {
      isConnected: tauriConnected,
      isTauri: true,
      connectionMethod: "tauri",
      lastOrchestratorState: lastOrchestratorStateTauri,
      lastStepProgress: lastStepProgressTauri,
      lastTaskRunUpdate: lastTaskRunUpdateTauri,
    };
  } else {
    return {
      isConnected: wsConnected,
      isTauri: false,
      connectionMethod: "websocket",
      lastOrchestratorState: lastOrchestratorStateWs,
      lastStepProgress: lastStepProgressWs,
      lastTaskRunUpdate: lastTaskRunUpdateWs,
    };
  }
}

// ============================================================================
// Specialized Unified Hooks
// ============================================================================

/**
 * Unified hook for orchestrator state changes.
 *
 * Automatically uses Tauri events in desktop app, WebSocket in browser.
 */
export interface UseUnifiedOrchestratorStateOptions {
  /** Task ID to filter events for */
  taskId?: string;
  /** Callback when state changes */
  onStateChange?: OrchestratorStateChangeCallback;
  /** Whether to enable the listener (defaults to true) */
  enabled?: boolean;
}

export interface UseUnifiedOrchestratorStateReturn {
  /** Whether connected to events */
  isConnected: boolean;
  /** Whether running in Tauri environment */
  isTauri: boolean;
  /** Last state received (matching taskId if specified) */
  lastState: OrchestratorStateChangePayload | null;
  /** Current workflow stage */
  workflowStage: string | null;
  /** Current iteration */
  iteration: number | null;
  /** Current phase */
  phase: string | null;
}

export function useUnifiedOrchestratorState(
  options: UseUnifiedOrchestratorStateOptions = {},
): UseUnifiedOrchestratorStateReturn {
  const { taskId, onStateChange, enabled = true } = options;
  const isTauri = isTauriEnvironment();

  // State for filtered results
  const [filteredState, setFilteredState] = useState<OrchestratorStateChangePayload | null>(null);

  const handleStateChange = useCallback(
    (payload: OrchestratorStateChangePayload) => {
      // Filter by task ID if specified
      if (taskId && payload.data?.task_run_id !== taskId) {
        return;
      }
      setFilteredState(payload);
      onStateChange?.(payload);
    },
    [taskId, onStateChange],
  );

  // Use Tauri events when in Tauri environment
  const [tauriConnected, setTauriConnected] = useState(false);
  const unlistenRef = useRef<UnlistenFn | null>(null);

  useEffect(() => {
    if (!isTauri || !enabled) {
      return;
    }

    let mounted = true;

    const setupListener = async () => {
      try {
        const unlisten = await listen<OrchestratorStateChangePayload>(
          "orchestrator-state-change",
          (event) => {
            if (!mounted) return;
            handleStateChange(event.payload);
          },
        );

        if (mounted) {
          unlistenRef.current = unlisten;
          setTauriConnected(true);
        } else {
          unlisten();
        }
      } catch (error) {
        logger.debug("Tauri events not available:", error);
        if (mounted) {
          setTauriConnected(false);
        }
      }
    };

    setupListener();

    return () => {
      mounted = false;
      if (unlistenRef.current) {
        unlistenRef.current();
        unlistenRef.current = null;
      }
      setTauriConnected(false);
    };
  }, [isTauri, enabled, handleStateChange]);

  // Use WebSocket when not in Tauri
  const wsResult = useWsOrchestratorState({
    taskId,
    onStateChange: !isTauri ? onStateChange : undefined,
    enabled: enabled && !isTauri,
  });

  // Return appropriate values based on environment
  if (isTauri) {
    return {
      isConnected: tauriConnected,
      isTauri: true,
      lastState: filteredState,
      workflowStage: filteredState?.data?.workflow_stage ?? null,
      iteration: filteredState?.data?.iteration ?? null,
      phase: filteredState?.data?.phase ?? null,
    };
  } else {
    return {
      isConnected: wsResult.isConnected,
      isTauri: false,
      lastState: wsResult.lastState,
      workflowStage: wsResult.workflowStage,
      iteration: wsResult.iteration,
      phase: wsResult.phase,
    };
  }
}

export default useUnifiedEvents;
