/**
 * Contexts barrel export
 *
 * Centralized export point for all context providers and hooks.
 */

export { ExecutionProvider, useExecution } from "./ExecutionContext";
export type { Config, Workflow } from "./ExecutionContext";

export { EventManagerProvider, useEventManager } from "./EventManagerContext";

// Re-export execution-related hooks for convenience
export {
  usePythonExecutor,
  useConfiguration,
  useWorkflowSelection,
  useMonitorDetection,
  useExecutionControl,
} from "../hooks";
