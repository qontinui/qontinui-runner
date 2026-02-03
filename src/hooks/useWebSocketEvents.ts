/**
 * useWebSocketEvents Hook
 *
 * Provides WebSocket-based event subscriptions as a fallback for non-Tauri contexts.
 * When running in a browser or external client (not in Tauri desktop app), this hook
 * connects to the runner's WebSocket endpoint to receive real-time events.
 *
 * Events supported:
 * - orchestrator-state-change: Workflow stage transitions
 * - step-progress: Progress through individual steps
 * - task-run-update: Task run status changes
 * - executor-event: Execution events (tree events, image recognition, etc.)
 *
 * The hook automatically:
 * - Detects if running in Tauri or browser environment
 * - Reconnects on disconnect with exponential backoff
 * - Handles connection errors gracefully
 */

import { useState, useEffect, useCallback, useRef } from "react";

// ============================================================================
// Types
// ============================================================================

/**
 * WebSocket message from the runner.
 * All events are wrapped with a channel name and payload.
 */
export interface WebSocketMessage<T = unknown> {
  channel: string;
  payload: T;
}

/**
 * Orchestrator state change event payload.
 */
export interface OrchestratorStateChangePayload {
  event_type: "OrchestratorStateChange";
  data: {
    task_run_id: string;
    workflow_stage: string;
    iteration: number;
    phase: string;
    state_data?: unknown;
  };
}

/**
 * Step progress event payload.
 */
export interface StepProgressPayload {
  event_type: "StepProgress";
  data: {
    task_run_id: string;
    step_index: number;
    step_name: string;
    status: string;
    details?: unknown;
    timestamp: number;
  };
}

/**
 * Task run update event payload.
 */
export interface TaskRunUpdatePayload {
  event_type: "TaskRunUpdate";
  data: {
    task_run_id: string;
    status: string;
    iteration?: number;
    details?: unknown;
    timestamp: number;
  };
}

/**
 * Executor event payload (generic events from workflow execution).
 */
export interface ExecutorEventPayload {
  event_type: "ExecutorEvent";
  data: {
    event: string;
    timestamp: number;
    sequence: number;
    data: unknown;
  };
}

/** Callback types for event handlers */
export type OrchestratorStateChangeCallback = (payload: OrchestratorStateChangePayload) => void;
export type StepProgressCallback = (payload: StepProgressPayload) => void;
export type TaskRunUpdateCallback = (payload: TaskRunUpdatePayload) => void;
export type ExecutorEventCallback = (payload: ExecutorEventPayload) => void;

/** Options for the useWebSocketEvents hook */
export interface UseWebSocketEventsOptions {
  /** WebSocket URL (default: ws://localhost:9876/ws/events) */
  url?: string;
  /** Whether to enable the WebSocket connection (defaults to true) */
  enabled?: boolean;
  /** Auto-reconnect on disconnect (defaults to true) */
  autoReconnect?: boolean;
  /** Maximum reconnect attempts (defaults to 10) */
  maxReconnectAttempts?: number;
  /** Base delay for reconnect in ms (defaults to 1000) */
  reconnectBaseDelay?: number;
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
  /** Callback on connection error */
  onError?: (error: Error) => void;
}

/** Return type for the useWebSocketEvents hook */
export interface UseWebSocketEventsReturn {
  /** Whether currently connected to WebSocket */
  isConnected: boolean;
  /** Whether currently attempting to connect */
  isConnecting: boolean;
  /** Last connection error (if any) */
  error: Error | null;
  /** Number of reconnect attempts made */
  reconnectAttempts: number;
  /** Whether running in Tauri environment */
  isTauriEnvironment: boolean;
  /** Manually connect to WebSocket */
  connect: () => void;
  /** Manually disconnect from WebSocket */
  disconnect: () => void;
  /** Last orchestrator state received */
  lastOrchestratorState: OrchestratorStateChangePayload | null;
  /** Last step progress received */
  lastStepProgress: StepProgressPayload | null;
  /** Last task run update received */
  lastTaskRunUpdate: TaskRunUpdatePayload | null;
}

// ============================================================================
// Environment Detection
// ============================================================================

/**
 * Detect if running in a Tauri environment.
 *
 * Tauri injects __TAURI_INTERNALS__ into the window object.
 */
export function isTauriEnvironment(): boolean {
  if (typeof window === "undefined") return false;

  // Check for Tauri 2.0 internals
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  if ((window as any).__TAURI_INTERNALS__) return true;

  // Check for Tauri 1.x compatibility
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  if ((window as any).__TAURI__) return true;

  return false;
}

// ============================================================================
// Hook Implementation
// ============================================================================

const DEFAULT_WS_URL = "ws://localhost:9876/ws/events";

export function useWebSocketEvents(
  options: UseWebSocketEventsOptions = {}
): UseWebSocketEventsReturn {
  const {
    url = DEFAULT_WS_URL,
    enabled = true,
    autoReconnect = true,
    maxReconnectAttempts = 10,
    reconnectBaseDelay = 1000,
    onOrchestratorStateChange,
    onStepProgress,
    onTaskRunUpdate,
    onExecutorEvent,
    onConnected,
    onDisconnected,
    onError,
  } = options;

  const [isConnected, setIsConnected] = useState(false);
  const [isConnecting, setIsConnecting] = useState(false);
  const [error, setError] = useState<Error | null>(null);
  const [reconnectAttempts, setReconnectAttempts] = useState(0);
  const [lastOrchestratorState, setLastOrchestratorState] =
    useState<OrchestratorStateChangePayload | null>(null);
  const [lastStepProgress, setLastStepProgress] = useState<StepProgressPayload | null>(null);
  const [lastTaskRunUpdate, setLastTaskRunUpdate] = useState<TaskRunUpdatePayload | null>(null);

  const wsRef = useRef<WebSocket | null>(null);
  const reconnectTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const shouldReconnectRef = useRef(true);
  const isTauriRef = useRef(isTauriEnvironment());

  // Handle incoming WebSocket messages
  const handleMessage = useCallback(
    (event: MessageEvent) => {
      try {
        const message = JSON.parse(event.data) as WebSocketMessage;

        switch (message.channel) {
          case "orchestrator-state-change": {
            const payload = message.payload as OrchestratorStateChangePayload;
            setLastOrchestratorState(payload);
            onOrchestratorStateChange?.(payload);
            break;
          }
          case "step-progress": {
            const payload = message.payload as StepProgressPayload;
            setLastStepProgress(payload);
            onStepProgress?.(payload);
            break;
          }
          case "task-run-update": {
            const payload = message.payload as TaskRunUpdatePayload;
            setLastTaskRunUpdate(payload);
            onTaskRunUpdate?.(payload);
            break;
          }
          case "executor-event": {
            const payload = message.payload as ExecutorEventPayload;
            onExecutorEvent?.(payload);
            break;
          }
          default:
            // Unknown channel - could be other event types, ignore silently
            console.debug("[useWebSocketEvents] Unknown channel:", message.channel);
        }
      } catch (err) {
        console.error("[useWebSocketEvents] Failed to parse message:", err);
      }
    },
    [onOrchestratorStateChange, onStepProgress, onTaskRunUpdate, onExecutorEvent]
  );

  // Connect to WebSocket
  const connect = useCallback(() => {
    // Don't connect if already connected or connecting
    if (wsRef.current?.readyState === WebSocket.OPEN || isConnecting) {
      return;
    }

    // Clean up any existing connection
    if (wsRef.current) {
      wsRef.current.close();
      wsRef.current = null;
    }

    setIsConnecting(true);
    setError(null);
    shouldReconnectRef.current = true;

    try {
      const ws = new WebSocket(url);

      ws.onopen = () => {
        console.log("[useWebSocketEvents] Connected to", url);
        setIsConnected(true);
        setIsConnecting(false);
        setReconnectAttempts(0);
        setError(null);
        onConnected?.();
      };

      ws.onclose = (event) => {
        console.log("[useWebSocketEvents] Disconnected:", event.code, event.reason);
        setIsConnected(false);
        setIsConnecting(false);
        wsRef.current = null;
        onDisconnected?.();

        // Attempt reconnect if auto-reconnect is enabled
        if (shouldReconnectRef.current && autoReconnect && reconnectAttempts < maxReconnectAttempts) {
          const delay = reconnectBaseDelay * Math.pow(2, reconnectAttempts);
          console.log(`[useWebSocketEvents] Reconnecting in ${delay}ms (attempt ${reconnectAttempts + 1})`);

          reconnectTimeoutRef.current = setTimeout(() => {
            setReconnectAttempts((prev) => prev + 1);
            connect();
          }, delay);
        }
      };

      ws.onerror = (event) => {
        console.error("[useWebSocketEvents] WebSocket error:", event);
        const err = new Error("WebSocket connection error");
        setError(err);
        onError?.(err);
      };

      ws.onmessage = handleMessage;

      wsRef.current = ws;
    } catch (err) {
      const error = err instanceof Error ? err : new Error(String(err));
      console.error("[useWebSocketEvents] Failed to create WebSocket:", error);
      setError(error);
      setIsConnecting(false);
      onError?.(error);
    }
  }, [
    url,
    isConnecting,
    autoReconnect,
    maxReconnectAttempts,
    reconnectBaseDelay,
    reconnectAttempts,
    handleMessage,
    onConnected,
    onDisconnected,
    onError,
  ]);

  // Disconnect from WebSocket
  const disconnect = useCallback(() => {
    shouldReconnectRef.current = false;

    // Clear any pending reconnect
    if (reconnectTimeoutRef.current) {
      clearTimeout(reconnectTimeoutRef.current);
      reconnectTimeoutRef.current = null;
    }

    if (wsRef.current) {
      wsRef.current.close();
      wsRef.current = null;
    }

    setIsConnected(false);
    setIsConnecting(false);
    setReconnectAttempts(0);
  }, []);

  // Auto-connect when enabled and not in Tauri environment
  useEffect(() => {
    // Skip WebSocket connection in Tauri environment - Tauri events are used instead
    if (isTauriRef.current) {
      console.log("[useWebSocketEvents] Tauri environment detected, skipping WebSocket");
      return;
    }

    if (!enabled) {
      disconnect();
      return;
    }

    connect();

    return () => {
      disconnect();
    };
  }, [enabled, connect, disconnect]);

  return {
    isConnected,
    isConnecting,
    error,
    reconnectAttempts,
    isTauriEnvironment: isTauriRef.current,
    connect,
    disconnect,
    lastOrchestratorState,
    lastStepProgress,
    lastTaskRunUpdate,
  };
}

// ============================================================================
// Specialized Hooks
// ============================================================================

/**
 * Hook for orchestrator state changes via WebSocket.
 *
 * Use this when you need to track workflow stage transitions in a non-Tauri context.
 */
export interface UseWsOrchestratorStateOptions {
  /** Task ID to filter events for */
  taskId?: string;
  /** Callback when state changes */
  onStateChange?: OrchestratorStateChangeCallback;
  /** Whether to enable the listener (defaults to true) */
  enabled?: boolean;
}

export interface UseWsOrchestratorStateReturn {
  /** Whether connected to WebSocket */
  isConnected: boolean;
  /** Last state received */
  lastState: OrchestratorStateChangePayload | null;
  /** Current workflow stage (if taskId matches) */
  workflowStage: string | null;
  /** Current iteration (if taskId matches) */
  iteration: number | null;
  /** Current phase (if taskId matches) */
  phase: string | null;
}

export function useWsOrchestratorState(
  options: UseWsOrchestratorStateOptions = {}
): UseWsOrchestratorStateReturn {
  const { taskId, onStateChange, enabled = true } = options;
  const [filteredState, setFilteredState] = useState<OrchestratorStateChangePayload | null>(null);

  const handleStateChange = useCallback(
    (payload: OrchestratorStateChangePayload) => {
      // Filter by task ID if specified
      if (taskId && payload.data.task_run_id !== taskId) {
        return;
      }
      setFilteredState(payload);
      onStateChange?.(payload);
    },
    [taskId, onStateChange]
  );

  const { isConnected, lastOrchestratorState } = useWebSocketEvents({
    onOrchestratorStateChange: handleStateChange,
    enabled,
  });

  // Use filtered state if we have a taskId filter, otherwise use the last received
  const effectiveState = taskId ? filteredState : lastOrchestratorState;

  return {
    isConnected,
    lastState: effectiveState,
    workflowStage: effectiveState?.data.workflow_stage ?? null,
    iteration: effectiveState?.data.iteration ?? null,
    phase: effectiveState?.data.phase ?? null,
  };
}

export default useWebSocketEvents;
