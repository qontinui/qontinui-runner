/**
 * FlowExecutionContext.tsx
 *
 * Context for sharing flow execution state with Flow Designer canvas nodes.
 * Listens to Tauri flow-event and provides step execution status.
 */

import { createContext, useContext, useState, useEffect, useMemo, type ReactNode } from "react";
import { listen } from "@tauri-apps/api/event";

/**
 * Status of a step in the flow execution.
 */
export type StepExecutionStatus = "pending" | "running" | "completed" | "failed";

/**
 * Execution information for a single step.
 */
export interface StepExecutionInfo {
  status: StepExecutionStatus;
  durationMs?: number;
  error?: string | null;
}

/**
 * Flow event payload from Tauri backend.
 */
interface FlowEventPayload {
  type: string;
  instance_id: string;
  flow_id?: string;
  flow_name?: string;
  step_id?: string;
  step_name?: string;
  step_type?: string;
  success?: boolean;
  outputs?: Record<string, unknown>;
  error?: string;
  duration_ms?: number;
  total_steps?: number;
}

/**
 * Context value for flow execution state.
 */
export interface FlowExecutionContextValue {
  /** Whether a flow is currently executing */
  isExecuting: boolean;
  /** Current flow ID being executed (null if no execution) */
  currentFlowId: string | null;
  /** Current flow instance ID */
  instanceId: string | null;
  /** ID of the step currently executing (null if none) */
  currentStepId: string | null;
  /** Map of step ID to execution info */
  stepStatuses: Map<string, StepExecutionInfo>;
  /** Get execution status for a specific step */
  getStepStatus: (stepId: string) => StepExecutionInfo | undefined;
  /** Whether the flow completed successfully */
  flowSuccess: boolean | null;
  /** Error message if flow failed */
  flowError: string | null;
  /** Reset execution state */
  reset: () => void;
}

const FlowExecutionContext = createContext<FlowExecutionContextValue | null>(null);

interface FlowExecutionProviderProps {
  children: ReactNode;
  /** Optional: Only track execution for a specific flow ID */
  flowIdFilter?: string | null;
}

/**
 * Initial state for the context.
 */
const initialState = {
  isExecuting: false,
  currentFlowId: null as string | null,
  instanceId: null as string | null,
  currentStepId: null as string | null,
  stepStatuses: new Map<string, StepExecutionInfo>(),
  flowSuccess: null as boolean | null,
  flowError: null as string | null,
};

/**
 * Provider component that listens to flow execution events
 * and provides execution state to the Flow Designer.
 */
export function FlowExecutionProvider({ children, flowIdFilter }: FlowExecutionProviderProps) {
  const [isExecuting, setIsExecuting] = useState(initialState.isExecuting);
  const [currentFlowId, setCurrentFlowId] = useState(initialState.currentFlowId);
  const [instanceId, setInstanceId] = useState(initialState.instanceId);
  const [currentStepId, setCurrentStepId] = useState(initialState.currentStepId);
  const [stepStatuses, setStepStatuses] = useState(initialState.stepStatuses);
  const [flowSuccess, setFlowSuccess] = useState(initialState.flowSuccess);
  const [flowError, setFlowError] = useState(initialState.flowError);

  // Reset function
  const reset = () => {
    setIsExecuting(false);
    setCurrentFlowId(null);
    setInstanceId(null);
    setCurrentStepId(null);
    setStepStatuses(new Map());
    setFlowSuccess(null);
    setFlowError(null);
  };

  // Listen to Tauri flow events
  useEffect(() => {
    const unlistenPromise = listen<FlowEventPayload>("flow-event", (event) => {
      const data = event.payload;

      // If filtering by flow ID and this event doesn't match, ignore
      // (but allow flow_started events through to potentially start tracking)
      if (flowIdFilter && data.flow_id && data.flow_id !== flowIdFilter && data.type !== "flow_started") {
        return;
      }

      switch (data.type) {
        case "flow_started":
          // If filtering by flow ID and this doesn't match, ignore
          if (flowIdFilter && data.flow_id !== flowIdFilter) {
            return;
          }
          // Reset state for new execution
          setIsExecuting(true);
          setCurrentFlowId(data.flow_id || null);
          setInstanceId(data.instance_id);
          setCurrentStepId(null);
          setStepStatuses(new Map());
          setFlowSuccess(null);
          setFlowError(null);
          break;

        case "step_started":
          if (data.step_id) {
            setCurrentStepId(data.step_id);
            setStepStatuses((prev) => {
              const next = new Map(prev);
              next.set(data.step_id!, {
                status: "running",
              });
              return next;
            });
          }
          break;

        case "step_completed":
          if (data.step_id) {
            const success = data.success ?? true;
            setStepStatuses((prev) => {
              const next = new Map(prev);
              next.set(data.step_id!, {
                status: success ? "completed" : "failed",
                durationMs: data.duration_ms,
                error: data.error || null,
              });
              return next;
            });
            // Clear current step since it's now complete
            setCurrentStepId((current) => (current === data.step_id ? null : current));
          }
          break;

        case "flow_paused":
          // Keep executing state but note flow is paused
          break;

        case "flow_resumed":
          // Continue execution
          break;

        case "flow_completed":
          setIsExecuting(false);
          setCurrentStepId(null);
          setFlowSuccess(data.success ?? true);
          setFlowError(data.error || null);
          break;

        default:
          // Ignore other event types
          break;
      }
    });

    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, [flowIdFilter]);

  // Memoized getStepStatus function
  const getStepStatus = useMemo(() => {
    return (stepId: string): StepExecutionInfo | undefined => {
      return stepStatuses.get(stepId);
    };
  }, [stepStatuses]);

  const value: FlowExecutionContextValue = useMemo(
    () => ({
      isExecuting,
      currentFlowId,
      instanceId,
      currentStepId,
      stepStatuses,
      getStepStatus,
      flowSuccess,
      flowError,
      reset,
    }),
    [isExecuting, currentFlowId, instanceId, currentStepId, stepStatuses, getStepStatus, flowSuccess, flowError]
  );

  return (
    <FlowExecutionContext.Provider value={value}>
      {children}
    </FlowExecutionContext.Provider>
  );
}

/**
 * Hook to access flow execution context.
 * Must be used within a FlowExecutionProvider.
 */
export function useFlowExecution(): FlowExecutionContextValue {
  const context = useContext(FlowExecutionContext);
  if (!context) {
    throw new Error("useFlowExecution must be used within a FlowExecutionProvider");
  }
  return context;
}

/**
 * Hook to access flow execution context (optional version).
 * Returns null if not within a FlowExecutionProvider.
 */
export function useFlowExecutionOptional(): FlowExecutionContextValue | null {
  return useContext(FlowExecutionContext);
}

/**
 * Hook to get execution status for a specific step.
 * Returns the step's execution info or undefined if not yet executed.
 */
export function useStepExecutionStatus(stepId: string): {
  status: StepExecutionStatus;
  isRunning: boolean;
  isComplete: boolean;
  hasError: boolean;
  durationMs?: number;
  error?: string | null;
} {
  const context = useFlowExecutionOptional();

  if (!context) {
    return {
      status: "pending",
      isRunning: false,
      isComplete: false,
      hasError: false,
    };
  }

  const stepInfo = context.getStepStatus(stepId);
  const isCurrentlyRunning = context.currentStepId === stepId;

  if (isCurrentlyRunning) {
    return {
      status: "running",
      isRunning: true,
      isComplete: false,
      hasError: false,
    };
  }

  if (stepInfo) {
    return {
      status: stepInfo.status,
      isRunning: stepInfo.status === "running",
      isComplete: stepInfo.status === "completed",
      hasError: stepInfo.status === "failed",
      durationMs: stepInfo.durationMs,
      error: stepInfo.error,
    };
  }

  return {
    status: "pending",
    isRunning: false,
    isComplete: false,
    hasError: false,
  };
}
