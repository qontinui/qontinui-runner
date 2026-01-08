/**
 * useGuiBuilderState Hook
 *
 * Core state management hook for the GUI Workflow Builder.
 * Handles workflow editing, saving, loading, and execution.
 */

import { useState, useCallback, useEffect, useMemo } from "react";
import type { GuiActionType, GuiWorkflowStep, SavedGuiWorkflow } from "../../types/gui-workflow";
import { getDefaultStepName } from "../../types/gui-workflow";
import type {
  GuiBuilderContextValue,
  StateInfo,
  ImageInfo,
  WorkflowFormState,
  ResultMessage,
} from "./types";
import { useExecution } from "../../contexts/ExecutionContext";

const API_BASE = "http://localhost:9876";

interface UseGuiBuilderStateProps {
  editWorkflowId?: string | null;
}

export function useGuiBuilderState({
  editWorkflowId,
}: UseGuiBuilderStateProps): GuiBuilderContextValue {
  // Execution context for config data
  const execution = useExecution();

  // Workflow steps
  const [steps, setSteps] = useState<GuiWorkflowStep[]>([]);

  // Current workflow state
  const [currentWorkflowId, setCurrentWorkflowId] = useState<string | null>(null);
  const [savedWorkflows, setSavedWorkflows] = useState<SavedGuiWorkflow[]>([]);
  const [originalSteps, setOriginalSteps] = useState<GuiWorkflowStep[]>([]);

  // Form state
  const [formState, setFormState] = useState<WorkflowFormState>({
    name: "",
    description: "",
    category: "general",
    tags: [],
  });
  const [originalFormState, setOriginalFormState] = useState<WorkflowFormState>({
    name: "",
    description: "",
    category: "general",
    tags: [],
  });

  // UI state
  const [editingStepId, setEditingStepId] = useState<string | null>(null);
  const [showSaveDialog, setShowSaveDialog] = useState(false);
  const [showWorkflowsPanel, setShowWorkflowsPanel] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [isRunning, setIsRunning] = useState(false);
  const [lastResult, setLastResult] = useState<ResultMessage | null>(null);

  // Parse states and images from loaded config
  const { states, images } = useMemo(() => {
    const stateList: StateInfo[] = [];
    const imageList: ImageInfo[] = [];

    if (execution.config?.states && Array.isArray(execution.config.states)) {
      for (const state of execution.config.states) {
        const stateName = state.name || state.id || "Unknown";
        stateList.push({
          id: state.id || "",
          name: stateName,
          description: state.description,
        });

        // Extract images from state
        const stateImages = state.stateImages || state.images;
        if (stateImages && Array.isArray(stateImages)) {
          for (const img of stateImages) {
            const imgId = img.id || "";
            const imgName = img.name || img.id || "";
            if (imgId && imgName) {
              imageList.push({ id: imgId, name: imgName, stateName });
            }
          }
        }
      }
    }

    return { states: stateList, images: imageList };
  }, [execution.config]);

  // Check for unsaved changes
  const hasUnsavedChanges = useMemo(() => {
    if (JSON.stringify(steps) !== JSON.stringify(originalSteps)) {
      return true;
    }
    if (JSON.stringify(formState) !== JSON.stringify(originalFormState)) {
      return true;
    }
    return false;
  }, [steps, originalSteps, formState, originalFormState]);

  // Fetch all saved workflows
  const refreshWorkflows = useCallback(async () => {
    try {
      const response = await fetch(`${API_BASE}/gui-workflows`);
      if (response.ok) {
        const data = await response.json();
        if (data.success && Array.isArray(data.data)) {
          setSavedWorkflows(data.data);
        }
      }
    } catch (error) {
      console.error("Failed to fetch GUI workflows:", error);
    }
  }, []);

  // Load workflows on mount
  useEffect(() => {
    refreshWorkflows();
  }, [refreshWorkflows]);

  // Load workflow for editing
  useEffect(() => {
    if (editWorkflowId) {
      const workflow = savedWorkflows.find((w) => w.id === editWorkflowId);
      if (workflow) {
        loadWorkflow(workflow);
      }
    }
  }, [editWorkflowId, savedWorkflows]);

  // Add a new step
  const addStep = useCallback((actionType: GuiActionType) => {
    const newStep: GuiWorkflowStep = {
      id: crypto.randomUUID(),
      action_type: actionType,
      name: getDefaultStepName(actionType),
    };
    setSteps((prev) => [...prev, newStep]);
    setEditingStepId(newStep.id);
  }, []);

  // Update a step
  const updateStep = useCallback((stepId: string, updates: Partial<GuiWorkflowStep>) => {
    setSteps((prev) => prev.map((step) => (step.id === stepId ? { ...step, ...updates } : step)));
  }, []);

  // Remove a step
  const removeStep = useCallback(
    (stepId: string) => {
      setSteps((prev) => prev.filter((step) => step.id !== stepId));
      if (editingStepId === stepId) {
        setEditingStepId(null);
      }
    },
    [editingStepId],
  );

  // Move step up
  const moveStepUp = useCallback((index: number) => {
    if (index <= 0) return;
    setSteps((prev) => {
      const newSteps = [...prev];
      [newSteps[index - 1], newSteps[index]] = [newSteps[index], newSteps[index - 1]];
      return newSteps;
    });
  }, []);

  // Move step down
  const moveStepDown = useCallback((index: number) => {
    setSteps((prev) => {
      if (index >= prev.length - 1) return prev;
      const newSteps = [...prev];
      [newSteps[index], newSteps[index + 1]] = [newSteps[index + 1], newSteps[index]];
      return newSteps;
    });
  }, []);

  // Reorder steps (for drag and drop)
  const reorderSteps = useCallback((startIndex: number, endIndex: number) => {
    setSteps((prev) => {
      const newSteps = [...prev];
      const [removed] = newSteps.splice(startIndex, 1);
      newSteps.splice(endIndex, 0, removed);
      return newSteps;
    });
  }, []);

  // Load a workflow
  const loadWorkflow = useCallback((workflow: SavedGuiWorkflow) => {
    setCurrentWorkflowId(workflow.id);
    setSteps(workflow.steps);
    setOriginalSteps(workflow.steps);
    const newFormState: WorkflowFormState = {
      name: workflow.name,
      description: workflow.description,
      category: workflow.category,
      tags: workflow.tags,
    };
    setFormState(newFormState);
    setOriginalFormState(newFormState);
    setEditingStepId(null);
    setLastResult(null);
  }, []);

  // Delete a workflow
  const deleteWorkflow = useCallback(
    async (workflowId: string) => {
      try {
        const response = await fetch(`${API_BASE}/gui-workflows/${workflowId}`, {
          method: "DELETE",
        });
        if (response.ok) {
          await refreshWorkflows();
          if (currentWorkflowId === workflowId) {
            handleNewWorkflow();
          }
        } else {
          throw new Error("Failed to delete workflow");
        }
      } catch (error) {
        console.error("Failed to delete workflow:", error);
        throw error;
      }
    },
    [currentWorkflowId, refreshWorkflows],
  );

  // Start a new workflow
  const handleNewWorkflow = useCallback(() => {
    setCurrentWorkflowId(null);
    setSteps([]);
    setOriginalSteps([]);
    const emptyFormState: WorkflowFormState = {
      name: "",
      description: "",
      category: "general",
      tags: [],
    };
    setFormState(emptyFormState);
    setOriginalFormState(emptyFormState);
    setEditingStepId(null);
    setLastResult(null);
  }, []);

  // Save workflow (update existing)
  const handleSaveWorkflow = useCallback(async () => {
    if (!currentWorkflowId) {
      setShowSaveDialog(true);
      return;
    }

    setIsSaving(true);
    try {
      const response = await fetch(`${API_BASE}/gui-workflows/${currentWorkflowId}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: formState.name,
          description: formState.description,
          steps,
          category: formState.category,
          tags: formState.tags,
        }),
      });

      if (response.ok) {
        const data = await response.json();
        if (data.success) {
          setOriginalSteps(steps);
          setOriginalFormState(formState);
          await refreshWorkflows();
          setLastResult({ success: true, message: "Workflow saved" });
        }
      } else {
        throw new Error("Failed to save workflow");
      }
    } catch (error) {
      console.error("Failed to save workflow:", error);
      setLastResult({ success: false, message: "Failed to save workflow" });
    } finally {
      setIsSaving(false);
    }
  }, [currentWorkflowId, formState, steps, refreshWorkflows]);

  // Save as new workflow
  const handleSaveAsNew = useCallback(async () => {
    if (!formState.name.trim()) {
      setLastResult({ success: false, message: "Workflow name is required" });
      return;
    }

    setIsSaving(true);
    try {
      const response = await fetch(`${API_BASE}/gui-workflows`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: formState.name,
          description: formState.description,
          steps,
          category: formState.category,
          tags: formState.tags,
        }),
      });

      if (response.ok) {
        const data = await response.json();
        if (data.success && data.data) {
          setCurrentWorkflowId(data.data.id);
          setOriginalSteps(steps);
          setOriginalFormState(formState);
          await refreshWorkflows();
          setShowSaveDialog(false);
          setLastResult({ success: true, message: "Workflow created" });
        }
      } else {
        throw new Error("Failed to create workflow");
      }
    } catch (error) {
      console.error("Failed to create workflow:", error);
      setLastResult({ success: false, message: "Failed to create workflow" });
    } finally {
      setIsSaving(false);
    }
  }, [formState, steps, refreshWorkflows]);

  // Run the current workflow
  const runWorkflow = useCallback(async () => {
    if (steps.length === 0) {
      setLastResult({ success: false, message: "No steps to run" });
      return;
    }

    // If not saved, save first
    if (!currentWorkflowId) {
      if (!formState.name.trim()) {
        setFormState((prev) => ({
          ...prev,
          name: `Workflow ${new Date().toLocaleTimeString()}`,
        }));
      }
      await handleSaveAsNew();
      // After saving, we need to run with the new ID
      return;
    }

    setIsRunning(true);
    setLastResult(null);

    try {
      const response = await fetch(`${API_BASE}/gui-workflows/${currentWorkflowId}/run`, {
        method: "POST",
      });

      if (response.ok) {
        const data = await response.json();
        if (data.success && data.data) {
          const result = data.data;
          if (result.failed_steps === 0) {
            setLastResult({
              success: true,
              message: `Completed ${result.successful_steps}/${result.total_steps} steps in ${result.duration_ms}ms`,
            });
          } else {
            setLastResult({
              success: false,
              message: `${result.failed_steps}/${result.total_steps} steps failed`,
            });
          }
        }
      } else {
        throw new Error("Failed to run workflow");
      }
    } catch (error) {
      console.error("Failed to run workflow:", error);
      setLastResult({ success: false, message: "Failed to run workflow" });
    } finally {
      setIsRunning(false);
    }
  }, [currentWorkflowId, steps.length, formState.name, handleSaveAsNew]);

  return {
    // Workflow Steps
    steps,
    setSteps,
    addStep,
    updateStep,
    removeStep,
    moveStepUp,
    moveStepDown,
    reorderSteps,

    // Workflow Management
    currentWorkflowId,
    currentWorkflowName: formState.name,
    hasUnsavedChanges,
    savedWorkflows,
    loadWorkflow,
    deleteWorkflow,
    refreshWorkflows,

    // Form State
    formState,
    setFormState,

    // Editing State
    editingStepId,
    setEditingStepId,

    // Save Dialog
    showSaveDialog,
    setShowSaveDialog,
    isSaving,
    handleSaveWorkflow,
    handleSaveAsNew,

    // Workflows Panel
    showWorkflowsPanel,
    setShowWorkflowsPanel,

    // Config Data
    configLoaded: execution.configLoaded,
    states,
    images,

    // Execution
    isRunning,
    runWorkflow,
    lastResult,

    // Actions
    handleNewWorkflow,
  };
}
