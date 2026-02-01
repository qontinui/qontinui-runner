/**
 * useMacroBuilderState Hook
 *
 * Core state management hook for the Macro Builder.
 * Handles macro editing, saving, loading, and execution.
 */

import { useState, useCallback, useEffect, useMemo } from "react";
import type { MacroActionType, MacroStep, SavedMacro } from "../../types/macro";
import { getDefaultStepName } from "../../types/macro";
import type {
  MacroBuilderContextValue,
  StateInfo,
  ImageInfo,
  MacroFormState,
  ResultMessage,
} from "./types";
import { useExecution } from "../../contexts/ExecutionContext";

const API_BASE = "http://localhost:9876";

interface UseMacroBuilderStateProps {
  editMacroId?: string | null;
}

export function useMacroBuilderState({
  editMacroId,
}: UseMacroBuilderStateProps): MacroBuilderContextValue {
  // Execution context for config data
  const execution = useExecution();

  // Macro steps
  const [steps, setSteps] = useState<MacroStep[]>([]);

  // Current macro state
  const [currentMacroId, setCurrentMacroId] = useState<string | null>(null);
  const [savedMacros, setSavedMacros] = useState<SavedMacro[]>([]);
  const [originalSteps, setOriginalSteps] = useState<MacroStep[]>([]);

  // Form state
  const [formState, setFormState] = useState<MacroFormState>({
    name: "",
    description: "",
    category: "general",
    tags: [],
  });
  const [originalFormState, setOriginalFormState] = useState<MacroFormState>({
    name: "",
    description: "",
    category: "general",
    tags: [],
  });

  // UI state
  const [editingStepId, setEditingStepId] = useState<string | null>(null);
  const [showSaveDialog, setShowSaveDialog] = useState(false);
  const [showMacrosPanel, setShowMacrosPanel] = useState(false);
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

  // Fetch all saved macros
  const refreshMacros = useCallback(async () => {
    try {
      const response = await fetch(`${API_BASE}/macros`);
      if (response.ok) {
        const data = await response.json();
        if (data.success && Array.isArray(data.data)) {
          setSavedMacros(data.data);
        }
      }
    } catch (error) {
      console.error("Failed to fetch macros:", error);
    }
  }, []);

  // Load macros on mount
  useEffect(() => {
    refreshMacros();
  }, [refreshMacros]);

  // Load macro for editing
  useEffect(() => {
    if (editMacroId) {
      const macro = savedMacros.find((m) => m.id === editMacroId);
      if (macro) {
        // Inline the loadMacro logic here to avoid dependency issues
        setCurrentMacroId(macro.id);
        setSteps(macro.steps);
        setOriginalSteps(macro.steps);
        const newFormState: MacroFormState = {
          name: macro.name,
          description: macro.description,
          category: macro.category,
          tags: macro.tags,
        };
        setFormState(newFormState);
        setOriginalFormState(newFormState);
        setEditingStepId(null);
        setLastResult(null);
      }
    }
  }, [editMacroId, savedMacros]);

  // Add a new step
  const addStep = useCallback((actionType: MacroActionType) => {
    const newStep: MacroStep = {
      id: crypto.randomUUID(),
      action_type: actionType,
      name: getDefaultStepName(actionType),
    };
    setSteps((prev) => [...prev, newStep]);
    setEditingStepId(newStep.id);
  }, []);

  // Update a step
  const updateStep = useCallback((stepId: string, updates: Partial<MacroStep>) => {
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

  // Load a macro
  const loadMacro = useCallback((macro: SavedMacro) => {
    setCurrentMacroId(macro.id);
    setSteps(macro.steps);
    setOriginalSteps(macro.steps);
    const newFormState: MacroFormState = {
      name: macro.name,
      description: macro.description,
      category: macro.category,
      tags: macro.tags,
    };
    setFormState(newFormState);
    setOriginalFormState(newFormState);
    setEditingStepId(null);
    setLastResult(null);
  }, []);

  // Delete a macro
  const deleteMacro = useCallback(
    async (macroId: string) => {
      try {
        const response = await fetch(`${API_BASE}/macros/${macroId}`, {
          method: "DELETE",
        });
        if (response.ok) {
          await refreshMacros();
          if (currentMacroId === macroId) {
            // Inline handleNewMacro logic to avoid circular dependency
            setCurrentMacroId(null);
            setSteps([]);
            setOriginalSteps([]);
            const emptyFormState: MacroFormState = {
              name: "",
              description: "",
              category: "general",
              tags: [],
            };
            setFormState(emptyFormState);
            setOriginalFormState(emptyFormState);
            setEditingStepId(null);
            setLastResult(null);
          }
        } else {
          throw new Error("Failed to delete macro");
        }
      } catch (error) {
        console.error("Failed to delete macro:", error);
        throw error;
      }
    },
    [currentMacroId, refreshMacros],
  );

  // Start a new macro
  const handleNewMacro = useCallback(() => {
    setCurrentMacroId(null);
    setSteps([]);
    setOriginalSteps([]);
    const emptyFormState: MacroFormState = {
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

  // Save macro (update existing)
  const handleSaveMacro = useCallback(async () => {
    if (!currentMacroId) {
      setShowSaveDialog(true);
      return;
    }

    setIsSaving(true);
    try {
      const response = await fetch(`${API_BASE}/macros/${currentMacroId}`, {
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
          await refreshMacros();
          setLastResult({ success: true, message: "Macro saved" });
        }
      } else {
        throw new Error("Failed to save macro");
      }
    } catch (error) {
      console.error("Failed to save macro:", error);
      setLastResult({ success: false, message: "Failed to save macro" });
    } finally {
      setIsSaving(false);
    }
  }, [currentMacroId, formState, steps, refreshMacros]);

  // Save as new macro
  const handleSaveAsNew = useCallback(async () => {
    if (!formState.name.trim()) {
      setLastResult({ success: false, message: "Macro name is required" });
      return;
    }

    setIsSaving(true);
    try {
      const response = await fetch(`${API_BASE}/macros`, {
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
          setCurrentMacroId(data.data.id);
          setOriginalSteps(steps);
          setOriginalFormState(formState);
          await refreshMacros();
          setShowSaveDialog(false);
          setLastResult({ success: true, message: "Macro created" });
        }
      } else {
        throw new Error("Failed to create macro");
      }
    } catch (error) {
      console.error("Failed to create macro:", error);
      setLastResult({ success: false, message: "Failed to create macro" });
    } finally {
      setIsSaving(false);
    }
  }, [formState, steps, refreshMacros]);

  // Run the current macro
  const runMacro = useCallback(async () => {
    if (steps.length === 0) {
      setLastResult({ success: false, message: "No steps to run" });
      return;
    }

    // If not saved, save first
    if (!currentMacroId) {
      if (!formState.name.trim()) {
        setFormState((prev) => ({
          ...prev,
          name: `Macro ${new Date().toLocaleTimeString()}`,
        }));
      }
      await handleSaveAsNew();
      // After saving, we need to run with the new ID
      return;
    }

    setIsRunning(true);
    setLastResult(null);

    try {
      const response = await fetch(`${API_BASE}/macros/${currentMacroId}/run`, {
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
        throw new Error("Failed to run macro");
      }
    } catch (error) {
      console.error("Failed to run macro:", error);
      setLastResult({ success: false, message: "Failed to run macro" });
    } finally {
      setIsRunning(false);
    }
  }, [currentMacroId, steps.length, formState.name, handleSaveAsNew]);

  return {
    // Macro Steps
    steps,
    setSteps,
    addStep,
    updateStep,
    removeStep,
    moveStepUp,
    moveStepDown,
    reorderSteps,

    // Macro Management
    currentMacroId,
    currentMacroName: formState.name,
    hasUnsavedChanges,
    savedMacros,
    loadMacro,
    deleteMacro,
    refreshMacros,

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
    handleSaveMacro,
    handleSaveAsNew,

    // Macros Panel
    showMacrosPanel,
    setShowMacrosPanel,

    // Config Data
    configLoaded: execution.configLoaded,
    states,
    images,

    // Execution
    isRunning,
    runMacro,
    lastResult,

    // Actions
    handleNewMacro,
  };
}
