/**
 * useFlowDesigner.ts
 *
 * React hooks for the Flow Designer.
 * Provides data fetching and state management for flows.
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { flowService } from "../../services/flow-service";
import type { Flow, FlowSummary, FlowState, FlowExecutionSummary } from "../../types/flow";

// ============================================================================
// Undo/Redo Hook
// ============================================================================

const MAX_HISTORY_SIZE = 50;

interface UndoRedoState<T> {
  history: T[];
  currentIndex: number;
}

interface UseUndoRedoResult<T> {
  /** Current state value */
  value: T;
  /** Update state (adds to history) */
  setValue: (newValue: T | ((prev: T) => T)) => void;
  /** Replace state without adding to history (for non-user actions) */
  setValueWithoutHistory: (newValue: T | ((prev: T) => T)) => void;
  /** Undo to previous state */
  undo: () => void;
  /** Redo to next state */
  redo: () => void;
  /** Check if undo is available */
  canUndo: boolean;
  /** Check if redo is available */
  canRedo: boolean;
  /** Clear history and set new initial state */
  reset: (initialValue: T) => void;
  /** Number of undo steps available */
  undoCount: number;
  /** Number of redo steps available */
  redoCount: number;
}

/**
 * Hook to provide undo/redo functionality for any state.
 * Uses a history stack to track changes.
 */
export function useUndoRedo<T>(initialValue: T): UseUndoRedoResult<T> {
  const [state, setState] = useState<UndoRedoState<T>>({
    history: [initialValue],
    currentIndex: 0,
  });

  // Track if this is an internal navigation (undo/redo) to avoid adding to history
  const _isNavigatingRef = useRef(false);

  const canUndo = state.currentIndex > 0;
  const canRedo = state.currentIndex < state.history.length - 1;

  const value = state.history[state.currentIndex];

  const setValue = useCallback((newValue: T | ((prev: T) => T)) => {
    setState((prev) => {
      const currentValue = prev.history[prev.currentIndex];
      const resolvedValue =
        typeof newValue === "function" ? (newValue as (prev: T) => T)(currentValue) : newValue;

      // Don't add to history if value hasn't changed (deep compare via JSON)
      try {
        if (JSON.stringify(resolvedValue) === JSON.stringify(currentValue)) {
          return prev;
        }
      } catch {
        // If JSON stringify fails, proceed with update
      }

      // Truncate any redo history and add new state
      const newHistory = [
        ...prev.history.slice(0, prev.currentIndex + 1),
        structuredClone(resolvedValue),
      ];

      // Limit history size
      const trimmedHistory =
        newHistory.length > MAX_HISTORY_SIZE
          ? newHistory.slice(newHistory.length - MAX_HISTORY_SIZE)
          : newHistory;

      return {
        history: trimmedHistory,
        currentIndex: trimmedHistory.length - 1,
      };
    });
  }, []);

  const setValueWithoutHistory = useCallback((newValue: T | ((prev: T) => T)) => {
    setState((prev) => {
      const currentValue = prev.history[prev.currentIndex];
      const resolvedValue =
        typeof newValue === "function" ? (newValue as (prev: T) => T)(currentValue) : newValue;

      // Replace current state in history without adding new entry
      const newHistory = [...prev.history];
      newHistory[prev.currentIndex] = structuredClone(resolvedValue);

      return {
        ...prev,
        history: newHistory,
      };
    });
  }, []);

  const undo = useCallback(() => {
    setState((prev) => {
      if (prev.currentIndex <= 0) return prev;
      return {
        ...prev,
        currentIndex: prev.currentIndex - 1,
      };
    });
  }, []);

  const redo = useCallback(() => {
    setState((prev) => {
      if (prev.currentIndex >= prev.history.length - 1) return prev;
      return {
        ...prev,
        currentIndex: prev.currentIndex + 1,
      };
    });
  }, []);

  const reset = useCallback((initialValue: T) => {
    setState({
      history: [structuredClone(initialValue)],
      currentIndex: 0,
    });
  }, []);

  return {
    value,
    setValue,
    setValueWithoutHistory,
    undo,
    redo,
    canUndo,
    canRedo,
    reset,
    undoCount: state.currentIndex,
    redoCount: state.history.length - 1 - state.currentIndex,
  };
}

/**
 * Hook to handle keyboard shortcuts for undo/redo.
 * Automatically sets up and cleans up event listeners.
 */
export function useUndoRedoKeyboard(
  undo: () => void,
  redo: () => void,
  canUndo: boolean,
  canRedo: boolean,
  enabled: boolean = true,
) {
  useEffect(() => {
    if (!enabled) return;

    const handleKeyDown = (e: KeyboardEvent) => {
      // Check for Ctrl+Z (undo) or Cmd+Z (macOS)
      if ((e.ctrlKey || e.metaKey) && e.key === "z" && !e.shiftKey) {
        e.preventDefault();
        if (canUndo) undo();
      }
      // Check for Ctrl+Shift+Z or Ctrl+Y (redo) or Cmd+Shift+Z (macOS)
      if (
        ((e.ctrlKey || e.metaKey) && e.shiftKey && e.key === "z") ||
        ((e.ctrlKey || e.metaKey) && e.key === "y")
      ) {
        e.preventDefault();
        if (canRedo) redo();
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [undo, redo, canUndo, canRedo, enabled]);
}

// ============================================================================
// Autosave Hook
// ============================================================================

const DEFAULT_AUTOSAVE_DELAY_MS = 5000; // 5 seconds

interface UseAutosaveOptions {
  /** Delay in milliseconds before autosaving (default: 5000ms) */
  delayMs?: number;
  /** Called when autosave starts */
  onSaveStart?: () => void;
  /** Called when autosave succeeds */
  onSaveSuccess?: () => void;
  /** Called when autosave fails */
  onSaveError?: (error: Error) => void;
  /** Whether autosave is enabled (default: true) */
  enabled?: boolean;
}

interface UseAutosaveResult {
  /** Whether autosave is currently pending (debounce in progress) */
  isPending: boolean;
  /** Whether autosave is currently saving */
  isSaving: boolean;
  /** Last autosave timestamp */
  lastSaved: Date | null;
  /** Whether there are unsaved changes */
  hasUnsavedChanges: boolean;
  /** Trigger an immediate save */
  saveNow: () => Promise<void>;
  /** Mark as saved (e.g., after manual save) */
  markSaved: () => void;
}

/**
 * Hook to provide autosave functionality with debouncing.
 * Automatically saves when the value changes after a delay.
 */
export function useAutosave<T>(
  value: T,
  saveFn: (value: T) => Promise<void>,
  options: UseAutosaveOptions = {},
): UseAutosaveResult {
  const {
    delayMs = DEFAULT_AUTOSAVE_DELAY_MS,
    onSaveStart,
    onSaveSuccess,
    onSaveError,
    enabled = true,
  } = options;

  const [isPending, setIsPending] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [lastSaved, setLastSaved] = useState<Date | null>(null);
  const [hasUnsavedChanges, setHasUnsavedChanges] = useState(false);

  // Track the latest value for saving
  const valueRef = useRef(value);
  valueRef.current = value;

  // Track the last saved value to detect changes
  const lastSavedValueRef = useRef<string | null>(null);

  // Debounce timer ref
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Save function
  const performSave = useCallback(async () => {
    if (!enabled) return;

    setIsPending(false);
    setIsSaving(true);
    onSaveStart?.();

    try {
      await saveFn(valueRef.current);
      setLastSaved(new Date());
      setHasUnsavedChanges(false);
      lastSavedValueRef.current = JSON.stringify(valueRef.current);
      onSaveSuccess?.();
    } catch (err) {
      onSaveError?.(err instanceof Error ? err : new Error(String(err)));
    } finally {
      setIsSaving(false);
    }
  }, [enabled, saveFn, onSaveStart, onSaveSuccess, onSaveError]);

  // Trigger immediate save
  const saveNow = useCallback(async () => {
    if (timerRef.current) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
    await performSave();
  }, [performSave]);

  // Mark as saved (for manual saves)
  const markSaved = useCallback(() => {
    setHasUnsavedChanges(false);
    setLastSaved(new Date());
    lastSavedValueRef.current = JSON.stringify(valueRef.current);
    if (timerRef.current) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
      setIsPending(false);
    }
  }, []);

  // Detect changes and schedule autosave
  useEffect(() => {
    if (!enabled) return;

    const currentValueStr = JSON.stringify(value);

    // Check if value has actually changed from last saved
    if (lastSavedValueRef.current === currentValueStr) {
      return; // No change, don't schedule save
    }

    setHasUnsavedChanges(true);

    // Clear existing timer
    if (timerRef.current) {
      clearTimeout(timerRef.current);
    }

    // Schedule new save
    setIsPending(true);
    timerRef.current = setTimeout(() => {
      timerRef.current = null;
      performSave();
    }, delayMs);

    // Cleanup on unmount
    return () => {
      if (timerRef.current) {
        clearTimeout(timerRef.current);
      }
    };
  }, [value, enabled, delayMs, performSave]);

  return {
    isPending,
    isSaving,
    lastSaved,
    hasUnsavedChanges,
    saveNow,
    markSaved,
  };
}

interface UseFlowListResult {
  flows: FlowSummary[];
  isLoading: boolean;
  error: string | null;
  refetch: () => Promise<void>;
}

/**
 * Hook to fetch all flows
 */
export function useFlowList(): UseFlowListResult {
  const [flows, setFlows] = useState<FlowSummary[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetchFlows = useCallback(async () => {
    setIsLoading(true);
    setError(null);
    try {
      const result = await flowService.listFlows();
      setFlows(result);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to fetch flows");
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchFlows();
  }, [fetchFlows]);

  return { flows, isLoading, error, refetch: fetchFlows };
}

interface UseFlowResult {
  flow: Flow | null;
  isLoading: boolean;
  error: string | null;
  refetch: () => Promise<void>;
}

/**
 * Hook to fetch a single flow by ID
 */
export function useFlow(id: string | null): UseFlowResult {
  const [flow, setFlow] = useState<Flow | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const fetchFlow = useCallback(async () => {
    if (!id) {
      setFlow(null);
      return;
    }

    setIsLoading(true);
    setError(null);
    try {
      const result = await flowService.getFlow(id);
      setFlow(result);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to fetch flow");
    } finally {
      setIsLoading(false);
    }
  }, [id]);

  useEffect(() => {
    fetchFlow();
  }, [fetchFlow]);

  return { flow, isLoading, error, refetch: fetchFlow };
}

interface UseFlowExecutionsResult {
  executions: FlowExecutionSummary[];
  isLoading: boolean;
  refetch: () => Promise<void>;
}

/**
 * Hook to fetch flow executions
 */
export function useFlowExecutions(): UseFlowExecutionsResult {
  const [executions, setExecutions] = useState<FlowExecutionSummary[]>([]);
  const [isLoading, setIsLoading] = useState(true);

  const fetchExecutions = useCallback(async () => {
    setIsLoading(true);
    try {
      const result = await flowService.listFlowExecutions();
      setExecutions(result);
    } catch {
      setExecutions([]);
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchExecutions();
  }, [fetchExecutions]);

  return { executions, isLoading, refetch: fetchExecutions };
}

/**
 * Actions for flow management
 */
export const flowActions = {
  async saveFlow(flow: Flow): Promise<string> {
    return flowService.saveFlow(flow);
  },

  async deleteFlow(id: string): Promise<boolean> {
    return flowService.deleteFlow(id);
  },

  async validateFlow(flow: Flow): Promise<string[]> {
    return flowService.validateFlow(flow);
  },

  async startExecution(flowId: string, inputs: Record<string, unknown> = {}): Promise<string> {
    return flowService.startFlowExecution(flowId, inputs);
  },

  async cancelExecution(instanceId: string): Promise<boolean> {
    return flowService.cancelFlowExecution(instanceId);
  },

  async getExecution(instanceId: string): Promise<FlowState | null> {
    return flowService.getFlowExecution(instanceId);
  },

  async createSampleFlow(): Promise<Flow> {
    return flowService.createSampleFlow();
  },

  async addSampleFlow(): Promise<string> {
    return flowService.addSampleFlow();
  },
};
