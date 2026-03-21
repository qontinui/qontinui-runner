import { useReducer, useCallback } from "react";
import type { UnifiedWorkflow } from "../../types";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { createLogger } from "@/lib/logger";

const log = createLogger("WorkflowBuilder");

interface ExecutionState {
  isExecuting: boolean;
  executionError: string | null;
  executionSuccess: string | null;
  showSaveBeforeRunDialog: boolean;
  isSavingBeforeRun: boolean;
  showRunOptions: boolean;
  pendingOverrides: Record<string, unknown> | null;
}

type ExecutionAction =
  | { type: "START_EXECUTING" }
  | { type: "STOP_EXECUTING" }
  | { type: "SET_ERROR"; error: string | null }
  | { type: "SET_SUCCESS"; message: string | null }
  | { type: "SHOW_SAVE_DIALOG"; show: boolean }
  | { type: "SET_SAVING_BEFORE_RUN"; saving: boolean }
  | { type: "SHOW_RUN_OPTIONS"; show: boolean }
  | { type: "SET_PENDING_OVERRIDES"; overrides: Record<string, unknown> | null }
  | { type: "CLEAR_AFTER_SAVE_RUN" };

const initialExecutionState: ExecutionState = {
  isExecuting: false,
  executionError: null,
  executionSuccess: null,
  showSaveBeforeRunDialog: false,
  isSavingBeforeRun: false,
  showRunOptions: false,
  pendingOverrides: null,
};

function executionReducer(state: ExecutionState, action: ExecutionAction): ExecutionState {
  switch (action.type) {
    case "START_EXECUTING":
      return { ...state, isExecuting: true, executionError: null, executionSuccess: null };
    case "STOP_EXECUTING":
      return { ...state, isExecuting: false };
    case "SET_ERROR":
      return { ...state, executionError: action.error };
    case "SET_SUCCESS":
      return { ...state, executionSuccess: action.message };
    case "SHOW_SAVE_DIALOG":
      return { ...state, showSaveBeforeRunDialog: action.show };
    case "SET_SAVING_BEFORE_RUN":
      return { ...state, isSavingBeforeRun: action.saving };
    case "SHOW_RUN_OPTIONS":
      return { ...state, showRunOptions: action.show };
    case "SET_PENDING_OVERRIDES":
      return { ...state, pendingOverrides: action.overrides };
    case "CLEAR_AFTER_SAVE_RUN":
      return { ...state, isSavingBeforeRun: false, pendingOverrides: null };
    default:
      return state;
  }
}

interface UseWorkflowExecutionArgs {
  workflowId: string | undefined;
  workflowName: string;
  isEmpty: boolean;
  hasUnsavedChanges: boolean;
  saveWorkflow: () => Promise<UnifiedWorkflow | null>;
  setWorkflow: (workflow: UnifiedWorkflow) => void;
  onNavigateToActive?: () => void;
}

export function useWorkflowExecution({
  workflowId,
  workflowName,
  isEmpty,
  hasUnsavedChanges,
  saveWorkflow,
  setWorkflow,
  onNavigateToActive,
}: UseWorkflowExecutionArgs) {
  const [execState, execDispatch] = useReducer(executionReducer, initialExecutionState);

  const executeWorkflowRun = useCallback(
    async (runWorkflowId: string, overrides?: Record<string, unknown>) => {
      execDispatch({ type: "START_EXECUTING" });

      try {
        log.debug("Running workflow:", runWorkflowId, workflowName);

        const runUrl = `${getApiBase()}/unified-workflows/${runWorkflowId}/run`;

        const compareArchitectures = overrides?._compareArchitectures as string[] | undefined;
        if (compareArchitectures && compareArchitectures.length > 1) {
          log.debug("Comparing architectures:", compareArchitectures);
          for (const arch of compareArchitectures) {
            const archOverrides: Record<string, unknown> = {
              use_worktree: overrides?.use_worktree ?? true,
            };
            if (arch !== "traditional") {
              archOverrides.workflow_architecture = arch;
            }
            tracedFetch(runUrl, {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({ overrides: archOverrides, force_fresh_start: true }),
            })
              .then((response) => {
                log.debug(`${arch} run response:`, response.status);
              })
              .catch((error) => {
                console.error(`[WorkflowBuilder] ${arch} run error:`, error);
              });
            await new Promise((resolve) => setTimeout(resolve, 1000));
          }

          setTimeout(() => {
            onNavigateToActive?.();
          }, 100);

          execDispatch({
            type: "SET_SUCCESS",
            message: `Launched ${compareArchitectures.length} architecture comparison runs! Check the Active tab.`,
          });
        } else {
          const body: Record<string, unknown> = {
            monitor_index: 0,
          };
          if (overrides && Object.keys(overrides).length > 0) {
            const cleanOverrides = { ...overrides };
            delete cleanOverrides._compareArchitectures;
            if (Object.keys(cleanOverrides).length > 0) {
              body.overrides = cleanOverrides;
            }
          }

          tracedFetch(runUrl, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(body),
          })
            .then((response) => {
              log.debug("Workflow run response:", response.status);
            })
            .catch((error) => {
              console.error("[WorkflowBuilder] Workflow run error:", error);
            });

          setTimeout(() => {
            onNavigateToActive?.();
          }, 100);

          execDispatch({
            type: "SET_SUCCESS",
            message: "Workflow started! Check the Active tab to monitor progress.",
          });
        }
        setTimeout(() => execDispatch({ type: "SET_SUCCESS", message: null }), 5000);
      } catch (error) {
        const errorMsg = error instanceof Error ? error.message : "Failed to start workflow";
        execDispatch({ type: "SET_ERROR", error: errorMsg });
      } finally {
        execDispatch({ type: "STOP_EXECUTING" });
      }
    },
    [workflowName, onNavigateToActive],
  );

  const handleSaveAndRun = useCallback(async () => {
    execDispatch({ type: "SET_SAVING_BEFORE_RUN", saving: true });
    try {
      const savedWorkflow = await saveWorkflow();
      if (!savedWorkflow) {
        execDispatch({ type: "SET_ERROR", error: "Failed to save workflow. Cannot run." });
        execDispatch({ type: "SHOW_SAVE_DIALOG", show: false });
        return;
      }
      execDispatch({ type: "SHOW_SAVE_DIALOG", show: false });
      await executeWorkflowRun(savedWorkflow.id, execState.pendingOverrides ?? undefined);
    } finally {
      execDispatch({ type: "CLEAR_AFTER_SAVE_RUN" });
    }
  }, [saveWorkflow, executeWorkflowRun, execState.pendingOverrides]);

  const handleRun = useCallback(
    async (overrides?: Record<string, unknown>) => {
      if (execState.isExecuting) return;

      const isComparison = !!overrides?._compareArchitectures;

      if (isEmpty) {
        execDispatch({
          type: "SET_ERROR",
          error: isComparison
            ? "Cannot compare architectures on an empty workflow. Use 'Generate & Run' first to create the workflow, then run a comparison."
            : "Cannot run an empty workflow. Add some steps first.",
        });
        return;
      }

      const needsSave = !workflowId || hasUnsavedChanges || isComparison;
      if (needsSave) {
        if (overrides) execDispatch({ type: "SET_PENDING_OVERRIDES", overrides });
        execDispatch({ type: "SHOW_SAVE_DIALOG", show: true });
        return;
      }

      await executeWorkflowRun(workflowId, overrides);
    },
    [workflowId, isEmpty, hasUnsavedChanges, execState.isExecuting, executeWorkflowRun],
  );

  const handleStop = useCallback(async () => {
    try {
      const response = await tracedFetch(`${getApiBase()}/stop-execution`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
      });
      const result = await response.json();
      if (result.success) {
        execDispatch({ type: "STOP_EXECUTING" });
        log.debug("Execution stopped");
      }
    } catch (error) {
      console.error("[WorkflowBuilder] Failed to stop execution:", error);
    }
  }, []);

  const handleSaveAndRunSpecWorkflow = useCallback(
    async (workflow: UnifiedWorkflow) => {
      try {
        const body = {
          name: workflow.name || "Untitled Workflow",
          description: workflow.description,
          category: workflow.category,
          tags: workflow.tags,
          setup_steps: workflow.setup_steps,
          verification_steps: workflow.verification_steps,
          agentic_steps: workflow.agentic_steps,
          completion_steps: workflow.completion_steps ?? [],
          max_iterations: workflow.max_iterations,
          provider: workflow.provider,
          model: workflow.model,
          stop_on_failure: workflow.stop_on_failure,
          reflection_mode: workflow.reflection_mode,
          use_worktree: workflow.use_worktree,
          multi_agent_mode: workflow.multi_agent_mode,
        };

        const response = await tracedFetch(`${getApiBase()}/unified-workflows`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(body),
        });

        const data = await response.json();
        if (!data.success || !data.data) {
          execDispatch({
            type: "SET_ERROR",
            error: data.error || "Failed to save spec workflow. Cannot run.",
          });
          return;
        }

        const savedWorkflow = data.data as UnifiedWorkflow;
        setWorkflow(savedWorkflow);
        await executeWorkflowRun(savedWorkflow.id);
      } catch (err) {
        const msg = err instanceof Error ? err.message : "Failed to save and run spec workflow";
        execDispatch({ type: "SET_ERROR", error: msg });
      }
    },
    [setWorkflow, executeWorkflowRun],
  );

  return {
    execState,
    execDispatch,
    handleRun,
    handleStop,
    handleSaveAndRun,
    handleSaveAndRunSpecWorkflow,
    executeWorkflowRun,
  };
}
