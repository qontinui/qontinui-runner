/**
 * Contexts barrel export
 *
 * Centralized export point for all context providers and hooks.
 */

export { ExecutionProvider, useExecution } from "./ExecutionContext";
export type { Config, Workflow } from "./ExecutionContext";

export { EventManagerProvider, useEventManager } from "./EventManagerContext";

export { AutoContinueProvider, useAutoContinue } from "./AutoContinueContext";

export {
  TaskProvider,
  useTaskContext,
  useCurrentTaskRunId,
  useIsTaskRunning,
  useTaskStartTime,
} from "./TaskContext";

export { RenderLogProvider, useRenderLog, useRenderLogOptional } from "./RenderLogContext";
export type { RenderLogEntry } from "./RenderLogContext";

export {
  ActiveRunsProvider,
  useActiveRuns,
  useActiveRunsOptional,
  useSelectedRunTaskInfo,
  useSelectedRunHasGuiLock,
} from "./ActiveRunsContext";
export type { ActiveRun, GuiLockInfo } from "./ActiveRunsContext";

// Unified Workflow Execution Context (single source of truth)
export {
  WorkflowExecutionProvider,
  useWorkflowExecution,
  useWorkflowExecutionOptional,
} from "./WorkflowExecutionContext";
export type {
  WorkflowExecutionState,
  WorkflowExecutionContextValue,
  StepCheckpoint,
  StepProgress,
  ResumePoint,
} from "./WorkflowExecutionContext";

// Shared Step Data Context (centralized dashboard data fetching)
export {
  SharedStepDataProvider,
  useSharedStepData,
  useSharedStepDataRaw,
} from "./SharedStepDataContext";

// Re-export execution-related hooks for convenience
export {
  usePythonExecutor,
  useConfiguration,
  useWorkflowSelection,
  useMonitorDetection,
  useExecutionControl,
} from "../hooks";
