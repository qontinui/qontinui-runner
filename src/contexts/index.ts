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
} from "./TaskContext";

export {
  RenderLogProvider,
  useRenderLog,
  useRenderLogOptional,
} from "./RenderLogContext";
export type { RenderLogEntry } from "./RenderLogContext";

// Re-export execution-related hooks for convenience
export {
  usePythonExecutor,
  useConfiguration,
  useWorkflowSelection,
  useMonitorDetection,
  useExecutionControl,
} from "../hooks";
