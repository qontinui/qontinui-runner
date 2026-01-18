/**
 * WorkflowBuilderContext.tsx
 *
 * State management for the unified Workflow Builder.
 * Handles workflow state, step management, and feature detection.
 */

import React, {
  createContext,
  useContext,
  useReducer,
  useCallback,
  useMemo,
  useEffect,
} from "react";
import { emit } from "@tauri-apps/api/event";
import type {
  UnifiedWorkflow,
  UnifiedStep,
  SetupStep,
  VerificationStep,
  AgenticStep,
  CompletionStep,
  WorkflowPhase,
  WorkflowFeatures,
  WorkflowExport,
  WorkflowImportResult,
  PromptStep,
} from "../../types";
import {
  detectWorkflowFeatures,
  generateStepId,
  createDefaultWorkflow,
  isWorkflowEmpty,
} from "../../types";

// =============================================================================
// Constants
// =============================================================================

const STORAGE_KEY = "qontinui-workflow-builder-draft";
const STORAGE_KEY_ORIGINAL = "qontinui-workflow-builder-original";
const API_BASE = "http://localhost:9876";

// =============================================================================
// State Types
// =============================================================================

interface WorkflowBuilderState {
  // Workflow data
  workflow: UnifiedWorkflow;
  originalWorkflow: UnifiedWorkflow | null; // For tracking unsaved changes

  // UI state
  selectedStepId: string | null;
  expandedPhases: Record<WorkflowPhase, boolean>;
  showSaveDialog: boolean;
  showAddDropdown: boolean;
  addDropdownPhase: WorkflowPhase | null;

  // Loading/saving state
  isLoading: boolean;
  isSaving: boolean;
  error: string | null;
}

// =============================================================================
// Actions
// =============================================================================

type WorkflowBuilderAction =
  | { type: "SET_WORKFLOW"; payload: UnifiedWorkflow }
  | { type: "UPDATE_WORKFLOW"; payload: Partial<UnifiedWorkflow> }
  | { type: "ADD_STEP"; payload: { step: UnifiedStep; phase: WorkflowPhase } }
  | { type: "REMOVE_STEP"; payload: { stepId: string; phase: WorkflowPhase } }
  | { type: "UPDATE_STEP"; payload: { step: UnifiedStep; phase: WorkflowPhase } }
  | {
      type: "MOVE_STEP";
      payload: { stepId: string; phase: WorkflowPhase; direction: "up" | "down" };
    }
  | { type: "SELECT_STEP"; payload: string | null }
  | { type: "TOGGLE_PHASE"; payload: WorkflowPhase }
  | { type: "SET_PHASE_EXPANDED"; payload: { phase: WorkflowPhase; expanded: boolean } }
  | { type: "SHOW_SAVE_DIALOG"; payload: boolean }
  | { type: "SHOW_ADD_DROPDOWN"; payload: { show: boolean; phase?: WorkflowPhase | null } }
  | { type: "SET_LOADING"; payload: boolean }
  | { type: "SET_SAVING"; payload: boolean }
  | { type: "SET_ERROR"; payload: string | null }
  | { type: "RESET_TO_NEW" }
  | { type: "MARK_SAVED" };

// =============================================================================
// Reducer
// =============================================================================

function workflowBuilderReducer(
  state: WorkflowBuilderState,
  action: WorkflowBuilderAction,
): WorkflowBuilderState {
  switch (action.type) {
    case "SET_WORKFLOW":
      return {
        ...state,
        workflow: action.payload,
        originalWorkflow: action.payload,
        selectedStepId: null,
      };

    case "UPDATE_WORKFLOW":
      return {
        ...state,
        workflow: { ...state.workflow, ...action.payload },
      };

    case "ADD_STEP": {
      const { step, phase } = action.payload;
      const stepWithId = { ...step, id: step.id || generateStepId() };

      // Check for duplicate step ID across all phases to prevent double-adding
      const allStepIds = new Set([
        ...state.workflow.setup_steps.map((s) => s.id),
        ...state.workflow.verification_steps.map((s) => s.id),
        ...state.workflow.agentic_steps.map((s) => s.id),
        ...(state.workflow.completion_steps ?? []).map((s) => s.id),
      ]);

      if (allStepIds.has(stepWithId.id)) {
        console.log("[WorkflowBuilder] Skipping duplicate step ID:", stepWithId.id);
        return state; // Skip duplicate
      }

      switch (phase) {
        case "setup":
          return {
            ...state,
            workflow: {
              ...state.workflow,
              setup_steps: [...state.workflow.setup_steps, stepWithId as SetupStep],
            },
            selectedStepId: stepWithId.id,
            expandedPhases: { ...state.expandedPhases, setup: true },
          };
        case "verification":
          return {
            ...state,
            workflow: {
              ...state.workflow,
              verification_steps: [
                ...state.workflow.verification_steps,
                stepWithId as VerificationStep,
              ],
            },
            selectedStepId: stepWithId.id,
            expandedPhases: { ...state.expandedPhases, verification: true },
          };
        case "agentic":
          return {
            ...state,
            workflow: {
              ...state.workflow,
              agentic_steps: [...state.workflow.agentic_steps, stepWithId as AgenticStep],
            },
            selectedStepId: stepWithId.id,
            expandedPhases: { ...state.expandedPhases, agentic: true },
          };
        case "completion": {
          const existingSteps = state.workflow.completion_steps ?? [];
          // Find the summary step (if it exists) to insert before it
          const summaryIndex = existingSteps.findIndex(
            (s) => s.type === "prompt" && (s as PromptStep).is_summary_step === true,
          );

          let newSteps: CompletionStep[];
          if (summaryIndex >= 0) {
            // Insert before the summary step (keep summary at the end)
            newSteps = [
              ...existingSteps.slice(0, summaryIndex),
              stepWithId as CompletionStep,
              ...existingSteps.slice(summaryIndex),
            ];
          } else {
            // No summary step, just append
            newSteps = [...existingSteps, stepWithId as CompletionStep];
          }

          return {
            ...state,
            workflow: {
              ...state.workflow,
              completion_steps: newSteps,
            },
            selectedStepId: stepWithId.id,
            expandedPhases: { ...state.expandedPhases, completion: true },
          };
        }
        default:
          return state;
      }
    }

    case "REMOVE_STEP": {
      const { stepId, phase } = action.payload;

      switch (phase) {
        case "setup":
          return {
            ...state,
            workflow: {
              ...state.workflow,
              setup_steps: state.workflow.setup_steps.filter((s) => s.id !== stepId),
            },
            selectedStepId: state.selectedStepId === stepId ? null : state.selectedStepId,
          };
        case "verification":
          return {
            ...state,
            workflow: {
              ...state.workflow,
              verification_steps: state.workflow.verification_steps.filter((s) => s.id !== stepId),
            },
            selectedStepId: state.selectedStepId === stepId ? null : state.selectedStepId,
          };
        case "agentic":
          return {
            ...state,
            workflow: {
              ...state.workflow,
              agentic_steps: state.workflow.agentic_steps.filter((s) => s.id !== stepId),
            },
            selectedStepId: state.selectedStepId === stepId ? null : state.selectedStepId,
          };
        case "completion":
          return {
            ...state,
            workflow: {
              ...state.workflow,
              completion_steps: (state.workflow.completion_steps ?? []).filter(
                (s) => s.id !== stepId,
              ),
            },
            selectedStepId: state.selectedStepId === stepId ? null : state.selectedStepId,
          };
        default:
          return state;
      }
    }

    case "UPDATE_STEP": {
      const { step, phase } = action.payload;

      switch (phase) {
        case "setup":
          return {
            ...state,
            workflow: {
              ...state.workflow,
              setup_steps: state.workflow.setup_steps.map((s) =>
                s.id === step.id ? (step as SetupStep) : s,
              ),
            },
          };
        case "verification":
          return {
            ...state,
            workflow: {
              ...state.workflow,
              verification_steps: state.workflow.verification_steps.map((s) =>
                s.id === step.id ? (step as VerificationStep) : s,
              ),
            },
          };
        case "agentic":
          return {
            ...state,
            workflow: {
              ...state.workflow,
              agentic_steps: state.workflow.agentic_steps.map((s) =>
                s.id === step.id ? (step as AgenticStep) : s,
              ),
            },
          };
        case "completion":
          return {
            ...state,
            workflow: {
              ...state.workflow,
              completion_steps: (state.workflow.completion_steps ?? []).map((s) =>
                s.id === step.id ? (step as CompletionStep) : s,
              ),
            },
          };
        default:
          return state;
      }
    }

    case "MOVE_STEP": {
      const { stepId, phase, direction } = action.payload;

      const moveInArray = <T extends { id: string }>(arr: T[]): T[] => {
        const index = arr.findIndex((s) => s.id === stepId);
        if (index === -1) return arr;
        if (direction === "up" && index === 0) return arr;
        if (direction === "down" && index === arr.length - 1) return arr;

        const newArr = [...arr];
        const targetIndex = direction === "up" ? index - 1 : index + 1;
        [newArr[index], newArr[targetIndex]] = [newArr[targetIndex], newArr[index]];
        return newArr;
      };

      // Special handling for completion phase to protect summary step position
      if (phase === "completion") {
        const steps = state.workflow.completion_steps ?? [];
        const stepToMove = steps.find((s) => s.id === stepId);

        // Prevent moving the summary step
        if (
          stepToMove &&
          stepToMove.type === "prompt" &&
          (stepToMove as PromptStep).is_summary_step
        ) {
          return state;
        }

        // Prevent moving any step past (below) the summary step
        if (direction === "down") {
          const index = steps.findIndex((s) => s.id === stepId);
          const nextStep = steps[index + 1];
          if (
            nextStep &&
            nextStep.type === "prompt" &&
            (nextStep as PromptStep).is_summary_step
          ) {
            return state;
          }
        }
      }

      switch (phase) {
        case "setup":
          return {
            ...state,
            workflow: {
              ...state.workflow,
              setup_steps: moveInArray(state.workflow.setup_steps),
            },
          };
        case "verification":
          return {
            ...state,
            workflow: {
              ...state.workflow,
              verification_steps: moveInArray(state.workflow.verification_steps),
            },
          };
        case "agentic":
          return {
            ...state,
            workflow: {
              ...state.workflow,
              agentic_steps: moveInArray(state.workflow.agentic_steps),
            },
          };
        case "completion":
          return {
            ...state,
            workflow: {
              ...state.workflow,
              completion_steps: moveInArray(state.workflow.completion_steps ?? []),
            },
          };
        default:
          return state;
      }
    }

    case "SELECT_STEP":
      return { ...state, selectedStepId: action.payload };

    case "TOGGLE_PHASE":
      return {
        ...state,
        expandedPhases: {
          ...state.expandedPhases,
          [action.payload]: !state.expandedPhases[action.payload],
        },
      };

    case "SET_PHASE_EXPANDED":
      return {
        ...state,
        expandedPhases: {
          ...state.expandedPhases,
          [action.payload.phase]: action.payload.expanded,
        },
      };

    case "SHOW_SAVE_DIALOG":
      return { ...state, showSaveDialog: action.payload };

    case "SHOW_ADD_DROPDOWN":
      return {
        ...state,
        showAddDropdown: action.payload.show,
        addDropdownPhase: action.payload.phase ?? null,
      };

    case "SET_LOADING":
      return { ...state, isLoading: action.payload };

    case "SET_SAVING":
      return { ...state, isSaving: action.payload };

    case "SET_ERROR":
      return { ...state, error: action.payload };

    case "RESET_TO_NEW": {
      const emptyWorkflow: UnifiedWorkflow = {
        ...createDefaultWorkflow(),
        id: generateStepId(),
        created_at: new Date().toISOString(),
        modified_at: new Date().toISOString(),
      };
      return {
        ...state,
        workflow: emptyWorkflow,
        originalWorkflow: null,
        selectedStepId: null,
        error: null,
      };
    }

    case "MARK_SAVED":
      return {
        ...state,
        originalWorkflow: state.workflow,
      };

    default:
      return state;
  }
}

// =============================================================================
// Context Value Type
// =============================================================================

interface WorkflowBuilderContextValue {
  // State
  state: WorkflowBuilderState;

  // Computed values
  features: WorkflowFeatures;
  hasUnsavedChanges: boolean;
  isEmpty: boolean;

  // Workflow actions
  setWorkflow: (workflow: UnifiedWorkflow) => void;
  updateWorkflow: (updates: Partial<UnifiedWorkflow>) => void;
  resetToNew: () => void;

  // Step actions
  addStep: (step: UnifiedStep, phase: WorkflowPhase) => void;
  removeStep: (stepId: string, phase: WorkflowPhase) => void;
  updateStep: (step: UnifiedStep, phase: WorkflowPhase) => void;
  moveStep: (stepId: string, phase: WorkflowPhase, direction: "up" | "down") => void;

  // Selection
  selectStep: (stepId: string | null) => void;
  getSelectedStep: () => UnifiedStep | null;

  // Phase UI
  togglePhase: (phase: WorkflowPhase) => void;
  setPhaseExpanded: (phase: WorkflowPhase, expanded: boolean) => void;

  // Dialogs
  showSaveDialog: (show: boolean) => void;
  showAddDropdown: (show: boolean, phase?: WorkflowPhase | null) => void;

  // Loading/saving
  setLoading: (loading: boolean) => void;
  setSaving: (saving: boolean) => void;
  setError: (error: string | null) => void;
  markSaved: () => void;

  // API operations
  saveWorkflow: () => Promise<UnifiedWorkflow | null>;
  loadWorkflow: (id: string) => Promise<boolean>;

  // Export/Import operations
  exportWorkflow: (id: string) => Promise<WorkflowExport | null>;
  importWorkflow: (
    workflow: UnifiedWorkflow,
    conflictStrategy?: "keep" | "generate" | "overwrite",
  ) => Promise<WorkflowImportResult | null>;
}

// =============================================================================
// Context
// =============================================================================

const WorkflowBuilderContext = createContext<WorkflowBuilderContextValue | null>(null);

// =============================================================================
// Provider
// =============================================================================

interface WorkflowBuilderProviderProps {
  children: React.ReactNode;
  initialWorkflow?: UnifiedWorkflow;
}

// Helper to load workflow from localStorage
function loadFromStorage(): UnifiedWorkflow | null {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored) {
      const parsed = JSON.parse(stored);
      // Validate it has the required structure
      if (parsed && typeof parsed === "object" && "setup_steps" in parsed) {
        return parsed as UnifiedWorkflow;
      }
    }
  } catch (e) {
    console.warn("Failed to load workflow from localStorage:", e);
  }
  return null;
}

// Helper to load original workflow from localStorage (tracks if workflow was saved)
function loadOriginalFromStorage(): UnifiedWorkflow | null {
  try {
    const stored = localStorage.getItem(STORAGE_KEY_ORIGINAL);
    if (stored) {
      const parsed = JSON.parse(stored);
      if (parsed && typeof parsed === "object" && "setup_steps" in parsed) {
        return parsed as UnifiedWorkflow;
      }
    }
  } catch (e) {
    console.warn("Failed to load original workflow from localStorage:", e);
  }
  return null;
}

// Helper to save workflow to localStorage
function saveToStorage(workflow: UnifiedWorkflow): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(workflow));
  } catch (e) {
    console.warn("Failed to save workflow to localStorage:", e);
  }
}

// Helper to save original workflow to localStorage (called after successful save)
function saveOriginalToStorage(workflow: UnifiedWorkflow | null): void {
  try {
    if (workflow) {
      localStorage.setItem(STORAGE_KEY_ORIGINAL, JSON.stringify(workflow));
    } else {
      localStorage.removeItem(STORAGE_KEY_ORIGINAL);
    }
  } catch (e) {
    console.warn("Failed to save original workflow to localStorage:", e);
  }
}

export function WorkflowBuilderProvider({
  children,
  initialWorkflow,
}: WorkflowBuilderProviderProps) {
  // Try to load from localStorage if no initial workflow provided
  const storedWorkflow = !initialWorkflow ? loadFromStorage() : null;
  // Also load the original workflow to preserve update vs create state
  const storedOriginalWorkflow = !initialWorkflow ? loadOriginalFromStorage() : null;

  const emptyWorkflow: UnifiedWorkflow = {
    ...createDefaultWorkflow(),
    id: generateStepId(),
    created_at: new Date().toISOString(),
    modified_at: new Date().toISOString(),
  };

  const initialState: WorkflowBuilderState = {
    workflow: initialWorkflow ?? storedWorkflow ?? emptyWorkflow,
    // Restore originalWorkflow from storage to preserve update vs create state after app reload
    originalWorkflow: initialWorkflow ?? storedOriginalWorkflow ?? null,
    selectedStepId: null,
    expandedPhases: {
      setup: true,
      verification: true,
      agentic: true,
      completion: true,
    },
    showSaveDialog: false,
    showAddDropdown: false,
    addDropdownPhase: null,
    isLoading: false,
    isSaving: false,
    error: null,
  };

  const [state, dispatch] = useReducer(workflowBuilderReducer, initialState);

  // Persist workflow to localStorage whenever it changes
  useEffect(() => {
    saveToStorage(state.workflow);
  }, [state.workflow]);

  // Persist originalWorkflow to localStorage to track whether workflow was saved
  // This ensures updates work correctly after app reload
  useEffect(() => {
    saveOriginalToStorage(state.originalWorkflow);
  }, [state.originalWorkflow]);

  // Computed values
  const features = useMemo(() => detectWorkflowFeatures(state.workflow), [state.workflow]);

  const hasUnsavedChanges = useMemo(() => {
    if (!state.originalWorkflow) {
      return !isWorkflowEmpty(state.workflow) || state.workflow.name !== "";
    }
    return JSON.stringify(state.workflow) !== JSON.stringify(state.originalWorkflow);
  }, [state.workflow, state.originalWorkflow]);

  const isEmpty = useMemo(() => isWorkflowEmpty(state.workflow), [state.workflow]);

  // Actions
  const setWorkflow = useCallback((workflow: UnifiedWorkflow) => {
    dispatch({ type: "SET_WORKFLOW", payload: workflow });
  }, []);

  const updateWorkflow = useCallback((updates: Partial<UnifiedWorkflow>) => {
    dispatch({ type: "UPDATE_WORKFLOW", payload: updates });
  }, []);

  const resetToNew = useCallback(() => {
    dispatch({ type: "RESET_TO_NEW" });
  }, []);

  const addStep = useCallback((step: UnifiedStep, phase: WorkflowPhase) => {
    dispatch({ type: "ADD_STEP", payload: { step, phase } });
    // Emit Tauri event for tutorial system to detect step additions
    emit("workflow-step-added", { phase, stepType: step.type, stepId: step.id }).catch((err) => {
      console.warn("[WorkflowBuilder] Failed to emit workflow-step-added event:", err);
    });
  }, []);

  const removeStep = useCallback((stepId: string, phase: WorkflowPhase) => {
    dispatch({ type: "REMOVE_STEP", payload: { stepId, phase } });
  }, []);

  const updateStep = useCallback((step: UnifiedStep, phase: WorkflowPhase) => {
    dispatch({ type: "UPDATE_STEP", payload: { step, phase } });
  }, []);

  const moveStep = useCallback((stepId: string, phase: WorkflowPhase, direction: "up" | "down") => {
    dispatch({ type: "MOVE_STEP", payload: { stepId, phase, direction } });
  }, []);

  const selectStep = useCallback((stepId: string | null) => {
    dispatch({ type: "SELECT_STEP", payload: stepId });
  }, []);

  const getSelectedStep = useCallback((): UnifiedStep | null => {
    if (!state.selectedStepId) return null;

    for (const step of state.workflow.setup_steps) {
      if (step.id === state.selectedStepId) return step;
    }
    for (const step of state.workflow.verification_steps) {
      if (step.id === state.selectedStepId) return step;
    }
    for (const step of state.workflow.agentic_steps) {
      if (step.id === state.selectedStepId) return step;
    }
    for (const step of state.workflow.completion_steps ?? []) {
      if (step.id === state.selectedStepId) return step;
    }
    return null;
  }, [state.selectedStepId, state.workflow]);

  const togglePhase = useCallback((phase: WorkflowPhase) => {
    dispatch({ type: "TOGGLE_PHASE", payload: phase });
  }, []);

  const setPhaseExpanded = useCallback((phase: WorkflowPhase, expanded: boolean) => {
    dispatch({ type: "SET_PHASE_EXPANDED", payload: { phase, expanded } });
  }, []);

  const showSaveDialogAction = useCallback((show: boolean) => {
    dispatch({ type: "SHOW_SAVE_DIALOG", payload: show });
  }, []);

  const showAddDropdownAction = useCallback((show: boolean, phase?: WorkflowPhase | null) => {
    dispatch({ type: "SHOW_ADD_DROPDOWN", payload: { show, phase } });
  }, []);

  const setLoading = useCallback((loading: boolean) => {
    dispatch({ type: "SET_LOADING", payload: loading });
  }, []);

  const setSaving = useCallback((saving: boolean) => {
    dispatch({ type: "SET_SAVING", payload: saving });
  }, []);

  const setError = useCallback((error: string | null) => {
    dispatch({ type: "SET_ERROR", payload: error });
  }, []);

  const markSaved = useCallback(() => {
    dispatch({ type: "MARK_SAVED" });
  }, []);

  // Save workflow to API
  const saveWorkflow = useCallback(async (): Promise<UnifiedWorkflow | null> => {
    dispatch({ type: "SET_SAVING", payload: true });
    dispatch({ type: "SET_ERROR", payload: null });

    try {
      const workflow = state.workflow;
      const isNew = !state.originalWorkflow || state.originalWorkflow.id !== workflow.id;

      // Prepare the request body
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
        skip_ai_summary: workflow.skip_ai_summary,
        log_source_selection: workflow.log_source_selection,
        context_ids: workflow.context_ids,
        disabled_context_ids: workflow.disabled_context_ids,
        auto_include_contexts: workflow.auto_include_contexts,
        prompt_template: workflow.prompt_template,
      };

      const url = isNew
        ? `${API_BASE}/unified-workflows`
        : `${API_BASE}/unified-workflows/${workflow.id}`;
      const method = isNew ? "POST" : "PUT";

      const response = await fetch(url, {
        method,
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });

      const data = await response.json();

      if (data.success && data.data) {
        dispatch({ type: "SET_WORKFLOW", payload: data.data });
        dispatch({ type: "SET_SAVING", payload: false });
        return data.data as UnifiedWorkflow;
      } else {
        dispatch({ type: "SET_ERROR", payload: data.error || "Failed to save workflow" });
        dispatch({ type: "SET_SAVING", payload: false });
        return null;
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : "Failed to save workflow";
      dispatch({ type: "SET_ERROR", payload: message });
      dispatch({ type: "SET_SAVING", payload: false });
      return null;
    }
  }, [state.workflow, state.originalWorkflow]);

  // Load workflow from API
  const loadWorkflow = useCallback(async (id: string): Promise<boolean> => {
    dispatch({ type: "SET_LOADING", payload: true });
    dispatch({ type: "SET_ERROR", payload: null });

    try {
      const response = await fetch(`${API_BASE}/unified-workflows/${id}`);
      const data = await response.json();

      if (data.success && data.data) {
        dispatch({ type: "SET_WORKFLOW", payload: data.data });
        dispatch({ type: "SET_LOADING", payload: false });
        return true;
      } else {
        dispatch({ type: "SET_ERROR", payload: data.error || "Failed to load workflow" });
        dispatch({ type: "SET_LOADING", payload: false });
        return false;
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : "Failed to load workflow";
      dispatch({ type: "SET_ERROR", payload: message });
      dispatch({ type: "SET_LOADING", payload: false });
      return false;
    }
  }, []);

  // Export a workflow by ID
  const exportWorkflow = useCallback(
    async (id: string): Promise<WorkflowExport | null> => {
      dispatch({ type: "SET_LOADING", payload: true });
      dispatch({ type: "SET_ERROR", payload: null });

      try {
        const response = await fetch(`${API_BASE}/unified-workflows/${id}/export`);
        const data = await response.json();

        if (data.success && data.data) {
          dispatch({ type: "SET_LOADING", payload: false });
          return data.data as WorkflowExport;
        } else {
          dispatch({ type: "SET_ERROR", payload: data.error || "Failed to export workflow" });
          dispatch({ type: "SET_LOADING", payload: false });
          return null;
        }
      } catch (error) {
        const message = error instanceof Error ? error.message : "Failed to export workflow";
        dispatch({ type: "SET_ERROR", payload: message });
        dispatch({ type: "SET_LOADING", payload: false });
        return null;
      }
    },
    [],
  );

  // Import a workflow
  const importWorkflow = useCallback(
    async (
      workflow: UnifiedWorkflow,
      conflictStrategy: "keep" | "generate" | "overwrite" = "generate",
    ): Promise<WorkflowImportResult | null> => {
      dispatch({ type: "SET_LOADING", payload: true });
      dispatch({ type: "SET_ERROR", payload: null });

      try {
        const response = await fetch(`${API_BASE}/unified-workflows/import`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            workflow,
            conflict_strategy: conflictStrategy,
          }),
        });

        const data = await response.json();

        if (data.success && data.data) {
          dispatch({ type: "SET_LOADING", payload: false });
          return data.data as WorkflowImportResult;
        } else {
          dispatch({ type: "SET_ERROR", payload: data.error || "Failed to import workflow" });
          dispatch({ type: "SET_LOADING", payload: false });
          return null;
        }
      } catch (error) {
        const message = error instanceof Error ? error.message : "Failed to import workflow";
        dispatch({ type: "SET_ERROR", payload: message });
        dispatch({ type: "SET_LOADING", payload: false });
        return null;
      }
    },
    [],
  );

  const value: WorkflowBuilderContextValue = {
    state,
    features,
    hasUnsavedChanges,
    isEmpty,
    setWorkflow,
    updateWorkflow,
    resetToNew,
    addStep,
    removeStep,
    updateStep,
    moveStep,
    selectStep,
    getSelectedStep,
    togglePhase,
    setPhaseExpanded,
    showSaveDialog: showSaveDialogAction,
    showAddDropdown: showAddDropdownAction,
    setLoading,
    setSaving,
    setError,
    markSaved,
    saveWorkflow,
    loadWorkflow,
    exportWorkflow,
    importWorkflow,
  };

  return (
    <WorkflowBuilderContext.Provider value={value}>{children}</WorkflowBuilderContext.Provider>
  );
}

// =============================================================================
// Hook
// =============================================================================

export function useWorkflowBuilder(): WorkflowBuilderContextValue {
  const context = useContext(WorkflowBuilderContext);
  if (!context) {
    throw new Error("useWorkflowBuilder must be used within a WorkflowBuilderProvider");
  }
  return context;
}
