/**
 * useExecutionControl
 *
 * Hook for managing execution start/stop and related state.
 * Responsibility: Control workflow execution lifecycle and window behavior.
 *
 * Uses the unified /execute-steps HTTP API endpoint, which provides:
 * - Consistent execution path with AI Builder multi-step workflows
 * - Running a single workflow is just a special case (one step of type "workflow")
 * - Same UnifiedActionService code path for all executions
 */

import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { windowManager } from "../managers";
import { Workflow } from "./useConfiguration";
import type { CommandResponse } from "../types/displayProfile";

interface UseExecutionControlOptions {
  onLog?: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void;
}

/** Step configuration for the unified /execute-steps endpoint */
interface ExecutionStep {
  type: "workflow" | "state" | "action" | "screenshot" | "playwright" | "prompt";
  name?: string;
  actionType?: string | null;
  targetImageId?: string | null;
  targetImageName?: string | null;
  monitorIndex?: number | null;
  takeScreenshot?: boolean;
  screenshotDelay?: number;
  screenshotMonitor?: number | string | null;
  playwrightScriptId?: string | null;
  promptContent?: string | null;
  timeoutSeconds?: number | null;
  /** Initial state IDs for workflow steps (overrides default initial states) */
  initialStateIds?: string[] | null;
}

/** Response from /execute-steps endpoint */
interface ExecuteStepsResponse {
  success: boolean;
  data?: {
    success: boolean;
    total_steps: number;
    successful_steps: number;
    failed_steps: number;
    total_duration_ms: number;
    steps: Array<{
      step_index: number;
      step_type: string;
      step_name: string;
      success: boolean;
      error?: string;
      screenshot_path?: string;
      duration_ms: number;
    }>;
  };
  error?: string;
}

interface UseExecutionControlReturn {
  executionActive: boolean;
  setExecutionActive: (active: boolean) => void;
  autoMinimize: boolean;
  setAutoMinimize: (enabled: boolean) => void;
  startExecution: (params: {
    selectedWorkflow: string;
    selectedMonitors: number[];
    workflows: Workflow[];
    availableMonitors: number[];
    /** Optional override for initial active states (highest priority) */
    initialStateIds?: string[] | null;
  }) => Promise<void>;
  stopExecution: () => Promise<void>;
}

/**
 * Hook to manage execution control
 */
export function useExecutionControl(
  options: UseExecutionControlOptions = {},
): UseExecutionControlReturn {
  const { onLog } = options;

  const [executionActive, setExecutionActive] = useState(false);
  const [autoMinimize, setAutoMinimize] = useState(true);

  /**
   * Start execution using the unified /execute-steps HTTP API.
   *
   * Running a single workflow from the Run page is modeled as a multi-step
   * execution with one step of type "workflow". This ensures:
   * - Same code path as AI Builder multi-step workflows
   * - Consistent behavior and error handling
   * - Unified logging and monitoring
   */
  const startExecution = useCallback(
    async (params: {
      selectedWorkflow: string;
      selectedMonitors: number[];
      workflows: Workflow[];
      availableMonitors: number[];
      initialStateIds?: string[] | null;
    }) => {
      console.log("[EXECUTION_CONTROL] startExecution called (unified path)");
      const { selectedWorkflow, selectedMonitors, workflows, initialStateIds } = params;

      try {
        if (!selectedWorkflow) {
          onLog?.("warning", "Please select a workflow before starting execution");
          console.log("[EXECUTION_CONTROL] No workflow selected. Available workflows:", workflows);
          return;
        }

        const workflowName =
          workflows.find((w) => w.id === selectedWorkflow)?.name || selectedWorkflow;
        console.log("[EXECUTION_CONTROL] Selected workflow:", selectedWorkflow, workflowName);
        console.log("[EXECUTION_CONTROL] Selected monitors:", selectedMonitors);

        // Minimize window if auto-minimize is enabled
        console.log("[EXECUTION_CONTROL] Auto-minimize check: autoMinimize=", autoMinimize);
        if (autoMinimize) {
          console.log("[EXECUTION_CONTROL] Minimizing window...");
          await windowManager.minimize();
          onLog?.("info", "Window minimized - waiting 1 second before starting automation");
          await new Promise((resolve) => setTimeout(resolve, 1000));
        } else {
          console.log("[EXECUTION_CONTROL] Skipping auto-minimize (disabled)");
        }

        // Build a single workflow step for the unified /execute-steps endpoint
        // This models running a single workflow as a multi-step execution with one step
        const step: ExecutionStep = {
          type: "workflow",
          name: workflowName,
          monitorIndex: selectedMonitors[0] ?? 0,
          timeoutSeconds: 300, // 5 minute timeout for workflows
          initialStateIds: initialStateIds ?? null, // Pass through initial state override
        };

        if (initialStateIds && initialStateIds.length > 0) {
          console.log("[EXECUTION_CONTROL] Using initial state override:", initialStateIds);
        }

        console.log("[EXECUTION_CONTROL] Executing via unified /execute-steps:", step);
        setExecutionActive(true);

        const response = await fetch("http://localhost:9876/execute-steps", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            steps: [step],
            execution_id: `run-${Date.now()}`,
          }),
        });

        const result: ExecuteStepsResponse = await response.json();
        console.log("[EXECUTION_CONTROL] Execute-steps response:", result);

        if (result.success && result.data?.success) {
          const stepResult = result.data.steps[0];
          const workflowInfo = ` (Workflow: ${workflowName})`;
          const monitorInfo =
            selectedMonitors.length > 1
              ? ` on monitors ${selectedMonitors.join(", ")}`
              : selectedMonitors[0] > 0
                ? ` on monitor ${selectedMonitors[0]}`
                : "";
          const durationInfo = stepResult
            ? ` in ${(stepResult.duration_ms / 1000).toFixed(1)}s`
            : "";
          onLog?.("success", `Execution completed${workflowInfo}${monitorInfo}${durationInfo}`);

          // Save the workflow ID as the last used workflow
          try {
            await invoke("save_last_workflow_id", { workflowId: selectedWorkflow });
            console.log("[EXECUTION_CONTROL] Saved last workflow ID:", selectedWorkflow);
          } catch (saveError) {
            console.error("[EXECUTION_CONTROL] Failed to save last workflow ID:", saveError);
          }
        } else {
          // Execution failed
          const errorMsg = result.data?.steps[0]?.error || result.error || "Unknown error";
          onLog?.("error", `Workflow execution failed: ${errorMsg}`);
        }

        setExecutionActive(false);

        // Restore window if it was auto-minimized
        if (autoMinimize) {
          await windowManager.restoreIfMinimized();
        }
      } catch (error) {
        console.error("[EXECUTION_CONTROL] Failed to start execution:", error);
        onLog?.("error", `Failed to start execution: ${error}`);
        setExecutionActive(false);

        // Restore window on error too
        if (autoMinimize) {
          await windowManager.restoreIfMinimized();
        }
      }
    },
    [autoMinimize, onLog],
  );

  /**
   * Stop execution
   */
  const stopExecution = useCallback(async () => {
    try {
      console.log("[EXECUTION_CONTROL] Stop execution called");
      const result = await invoke<CommandResponse>("stop_execution");
      if (result.success) {
        setExecutionActive(false);
        onLog?.("info", "Execution stopped");
        console.log("[EXECUTION_CONTROL] Calling restoreWindowIfMinimized");
        // Restore window if it was auto-minimized
        await windowManager.restoreIfMinimized();
      }
    } catch (error) {
      onLog?.("error", `Failed to stop execution: ${error}`);
    }
  }, [onLog]);

  return {
    executionActive,
    setExecutionActive,
    autoMinimize,
    setAutoMinimize,
    startExecution,
    stopExecution,
  };
}
