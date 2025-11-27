/**
 * useWorkflowSelection
 *
 * Hook for managing workflow selection state.
 * Responsibility: Track selected workflow and provide selection functionality.
 */

import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

interface UseWorkflowSelectionReturn {
  selectedWorkflow: string;
  setSelectedWorkflow: (id: string) => void;
  selectWorkflowWithPersistence: (workflowId: string) => Promise<void>;
}

/**
 * Hook to manage workflow selection
 */
export function useWorkflowSelection(): UseWorkflowSelectionReturn {
  const [selectedWorkflow, setSelectedWorkflow] = useState("");

  /**
   * Select a workflow and save it as the last used
   */
  const selectWorkflowWithPersistence = useCallback(async (workflowId: string) => {
    setSelectedWorkflow(workflowId);

    // Save the workflow ID as the last used workflow
    try {
      await invoke("save_last_workflow_id", { workflowId });
      console.log("[WORKFLOW_SELECTION] Saved last workflow ID:", workflowId);
    } catch (saveError) {
      console.error("[WORKFLOW_SELECTION] Failed to save last workflow ID:", saveError);
      // Non-critical error, don't throw
    }
  }, []);

  return {
    selectedWorkflow,
    setSelectedWorkflow,
    selectWorkflowWithPersistence,
  };
}
