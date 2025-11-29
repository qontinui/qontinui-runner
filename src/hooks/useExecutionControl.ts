/**
 * useExecutionControl
 *
 * Hook for managing execution start/stop and related state.
 * Responsibility: Control workflow execution lifecycle and window behavior.
 */

import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { windowManager } from "../managers";
import { Workflow } from "./useConfiguration";

interface UseExecutionControlOptions {
  onLog?: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void;
  onConfigurationPanelCollapse?: (collapsed: boolean) => void;
  onExecutionPanelCollapse?: (collapsed: boolean) => void;
}

interface UseExecutionControlReturn {
  executionActive: boolean;
  setExecutionActive: (active: boolean) => void;
  autoMinimize: boolean;
  setAutoMinimize: (enabled: boolean) => void;
  startExecution: (params: {
    selectedWorkflow: string;
    selectedMonitor: number;
    workflows: Workflow[];
    availableMonitors: number[];
  }) => Promise<void>;
  stopExecution: () => Promise<void>;
}

/**
 * Hook to manage execution control
 */
export function useExecutionControl(
  options: UseExecutionControlOptions = {},
): UseExecutionControlReturn {
  const { onLog, onConfigurationPanelCollapse, onExecutionPanelCollapse } = options;

  const [executionActive, setExecutionActive] = useState(false);
  const [autoMinimize, setAutoMinimize] = useState(true);

  /**
   * Start execution
   */
  const startExecution = useCallback(
    async (params: {
      selectedWorkflow: string;
      selectedMonitor: number;
      workflows: Workflow[];
      availableMonitors: number[];
    }) => {
      console.log("[EXECUTION_CONTROL] startExecution called");
      const { selectedWorkflow, selectedMonitor, workflows, availableMonitors } = params;

      try {
        if (!selectedWorkflow) {
          onLog?.("warning", "Please select a workflow before starting execution");
          console.log("[EXECUTION_CONTROL] No workflow selected. Available workflows:", workflows);
          return;
        }

        console.log("[EXECUTION_CONTROL] Selected workflow:", selectedWorkflow);

        // Auto-collapse panels when starting
        if (onConfigurationPanelCollapse) onConfigurationPanelCollapse(true);
        if (onExecutionPanelCollapse) onExecutionPanelCollapse(true);

        const invokeParams: any = {
          processId: selectedWorkflow,
          monitorIndex: selectedMonitor,
        };

        const workflowName = workflows.find((w) => w.id === selectedWorkflow)?.name;
        console.log("Starting execution with workflow:", selectedWorkflow, workflowName);

        // Minimize window if auto-minimize is enabled and only one monitor
        console.log(
          "[EXECUTION_CONTROL] Auto-minimize check: autoMinimize=",
          autoMinimize,
          "monitors=",
          availableMonitors.length,
        );
        if (autoMinimize && availableMonitors.length === 1) {
          console.log("[EXECUTION_CONTROL] Minimizing window...");
          await windowManager.minimize();
          onLog?.("info", "Window minimized - waiting 1 second before starting automation");
          // Wait 1 second to allow window minimization and user to refocus previous window
          await new Promise((resolve) => setTimeout(resolve, 1000));
        } else {
          console.log("[EXECUTION_CONTROL] Skipping auto-minimize");
        }

        console.log("Invoking start_execution with params:", invokeParams);
        const result: any = await invoke("start_execution", invokeParams);
        if (result.success) {
          setExecutionActive(true);
          const workflowInfo = ` (Workflow: ${workflowName})`;
          const monitorInfo = selectedMonitor > 0 ? ` on monitor ${selectedMonitor}` : "";
          onLog?.("success", `Execution started${workflowInfo}${monitorInfo}`);

          // Save the workflow ID as the last used workflow
          try {
            await invoke("save_last_workflow_id", { workflowId: selectedWorkflow });
            console.log("[EXECUTION_CONTROL] Saved last workflow ID:", selectedWorkflow);
          } catch (saveError) {
            console.error("[EXECUTION_CONTROL] Failed to save last workflow ID:", saveError);
            // Non-critical error, don't show to user
          }
        }
      } catch (error) {
        onLog?.("error", `Failed to start execution: ${error}`);
      }
    },
    [autoMinimize, onLog, onConfigurationPanelCollapse, onExecutionPanelCollapse],
  );

  /**
   * Stop execution
   */
  const stopExecution = useCallback(async () => {
    try {
      console.log("[EXECUTION_CONTROL] Stop execution called");
      const result: any = await invoke("stop_execution");
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
