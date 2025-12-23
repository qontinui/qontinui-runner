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
import type { CommandResponse } from "../types/displayProfile";

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
    selectedMonitors: number[];
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
      selectedMonitors: number[];
      workflows: Workflow[];
      availableMonitors: number[];
    }) => {
      console.log("[EXECUTION_CONTROL] startExecution called");
      const { selectedWorkflow, selectedMonitors, workflows, availableMonitors } = params;

      try {
        if (!selectedWorkflow) {
          onLog?.("warning", "Please select a workflow before starting execution");
          console.log("[EXECUTION_CONTROL] No workflow selected. Available workflows:", workflows);
          return;
        }

        console.log("[EXECUTION_CONTROL] Selected workflow:", selectedWorkflow);
        console.log("[EXECUTION_CONTROL] Selected monitors:", selectedMonitors);

        // Auto-collapse panels when starting
        if (onConfigurationPanelCollapse) onConfigurationPanelCollapse(true);
        if (onExecutionPanelCollapse) onExecutionPanelCollapse(true);

        const invokeParams = {
          processId: selectedWorkflow,
          monitorIndices: selectedMonitors,
        };

        const workflowName = workflows.find((w) => w.id === selectedWorkflow)?.name;
        console.log("Starting execution with workflow:", selectedWorkflow, workflowName);

        // Minimize window if auto-minimize is enabled and only one monitor selected
        console.log(
          "[EXECUTION_CONTROL] Auto-minimize check: autoMinimize=",
          autoMinimize,
          "selectedMonitors=",
          selectedMonitors.length,
          "availableMonitors=",
          availableMonitors.length,
        );
        if (autoMinimize && selectedMonitors.length === 1 && availableMonitors.length === 1) {
          console.log("[EXECUTION_CONTROL] Minimizing window...");
          await windowManager.minimize();
          onLog?.("info", "Window minimized - waiting 1 second before starting automation");
          // Wait 1 second to allow window minimization and user to refocus previous window
          await new Promise((resolve) => setTimeout(resolve, 1000));
        } else {
          console.log("[EXECUTION_CONTROL] Skipping auto-minimize (multi-monitor or disabled)");
        }

        console.log("Invoking start_execution with params:", invokeParams);
        const result = await invoke<CommandResponse>("start_execution", invokeParams);
        if (result.success) {
          setExecutionActive(true);
          const workflowInfo = ` (Workflow: ${workflowName})`;
          const monitorInfo =
            selectedMonitors.length > 1
              ? ` on monitors ${selectedMonitors.join(", ")}`
              : selectedMonitors[0] > 0
                ? ` on monitor ${selectedMonitors[0]}`
                : "";
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
