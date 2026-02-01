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

export {
  RenderLogProvider,
  useRenderLog,
  useRenderLogOptional,
} from "./RenderLogContext";
export type { RenderLogEntry } from "./RenderLogContext";

export {
  ActiveRunsProvider,
  useActiveRuns,
  useActiveRunsOptional,
  useSelectedRunTaskInfo,
  useSelectedRunHasGuiLock,
} from "./ActiveRunsContext";
export type { ActiveRun, GuiLockInfo } from "./ActiveRunsContext";

// Re-export execution-related hooks for convenience
export {
  usePythonExecutor,
  useConfiguration,
  useWorkflowSelection,
  useMonitorDetection,
  useExecutionControl,
} from "../hooks";
