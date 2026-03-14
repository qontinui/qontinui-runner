/**
 * WorkflowBuilderContext.tsx
 *
 * State management for the unified Workflow Builder.
 * Handles workflow state, step management, and feature detection.
 *
 * Internally uses @qontinui/workflow-ui's shared WorkflowBuilderProvider
 * and WorkflowDataProvider to make the shared context available for
 * headless components, while preserving the runner's own rich API layer
 * that includes Tauri-specific features, loading/saving state,
 * instanceStorage persistence, and export/import operations.
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
import {
  WorkflowBuilderProvider as SharedBuilderProvider,
  WorkflowDataProvider,
} from "@qontinui/workflow-ui";
import type {
  WorkflowBuilderState as SharedBuilderState,
  WorkflowBuilderAction as SharedBuilderAction,
  WorkflowBuilderContextValue as SharedBuilderContextValue,
} from "@qontinui/workflow-ui";
import { createRunnerDataAdapter } from "../../lib/workflow-builder/runner-data-adapter";
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
  WorkflowStage,
} from "../../types";
import {
  detectWorkflowFeatures,
  generateStepId,
  createDefaultWorkflow,
  isWorkflowEmpty,
} from "../../types";
import { registerUserSkills } from "@qontinui/workflow-utils";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { instanceStorage } from "@/lib/instance-storage";

// Re-export shared types for consumers that may need them
export type { SharedBuilderState, SharedBuilderAction, SharedBuilderContextValue };

// =============================================================================
// Constants
// =============================================================================

const STORAGE_KEY = "qontinui-workflow-builder-draft";
const STORAGE_KEY_ORIGINAL = "qontinui-workflow-builder-original";
// Singleton data adapter instance
const runnerDataAdapter = createRunnerDataAdapter();

// =============================================================================
// State Types
// =============================================================================

interface WorkflowBuilderState {
  // Workflow data
  workflow: UnifiedWorkflow;
  originalWorkflow: UnifiedWorkflow | null; // For tracking unsaved changes

  // UI state
  selectedStepId: string | null;
  currentStageIndex: number | null; // null = top-level (single-stage mode)
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
  | { type: "MARK_SAVED" }
  | { type: "ADD_STAGE"; payload: { name: string } }
  | { type: "REMOVE_STAGE"; payload: { stageIndex: number } }
  | { type: "SELECT_STAGE"; payload: number | null }
  | {
      type: "UPDATE_STAGE";
      payload: { stageIndex: number; updates: Partial<WorkflowStage> };
    }
  | {
      type: "MOVE_STAGE";
      payload: { stageIndex: number; direction: "up" | "down" };
    };

// =============================================================================
// Stage-Aware Helpers
// =============================================================================

/** Get the step array for a given phase, respecting stage context. */
function getPhaseSteps(
  workflow: UnifiedWorkflow,
  stageIndex: number | null,
  phase: WorkflowPhase,
): UnifiedStep[] {
  const source =
    stageIndex !== null && workflow.stages?.[stageIndex] ? workflow.stages[stageIndex] : workflow;
  switch (phase) {
    case "setup":
      return source.setup_steps ?? [];
    case "verification":
      return source.verification_steps ?? [];
    case "agentic":
      return source.agentic_steps ?? [];
    case "completion":
      return source.completion_steps ?? [];
    default:
      return [];
  }
}

/** Return a new workflow with the given steps set for the phase, respecting stage context. */
function setPhaseSteps(
  workflow: UnifiedWorkflow,
  stageIndex: number | null,
  phase: WorkflowPhase,
  steps: UnifiedStep[],
): UnifiedWorkflow {
  if (stageIndex !== null && workflow.stages) {
    const stages = workflow.stages.map((s, i) => {
      if (i !== stageIndex) return s;
      const updated = { ...s };
      switch (phase) {
        case "setup":
          updated.setup_steps = steps as SetupStep[];
          break;
        case "verification":
          updated.verification_steps = steps as VerificationStep[];
          break;
        case "agentic":
          updated.agentic_steps = steps as AgenticStep[];
          break;
        case "completion":
          updated.completion_steps = steps as CompletionStep[];
          break;
      }
      return updated;
    });
    return { ...workflow, stages };
  }
  switch (phase) {
    case "setup":
      return { ...workflow, setup_steps: steps as SetupStep[] };
    case "verification":
      return { ...workflow, verification_steps: steps as VerificationStep[] };
    case "agentic":
      return { ...workflow, agentic_steps: steps as AgenticStep[] };
    case "completion":
      return { ...workflow, completion_steps: steps as CompletionStep[] };
    default:
      return workflow;
  }
}

// =============================================================================
// Reducer
// =============================================================================

function workflowBuilderReducer(
  state: WorkflowBuilderState,
  action: WorkflowBuilderAction,
): WorkflowBuilderState {
  switch (action.type) {
    case "SET_WORKFLOW": {
      const wf = action.payload;
      const si = wf.stages && wf.stages.length > 0 ? 0 : null;
      const phaseHasSteps = (phase: WorkflowPhase) => getPhaseSteps(wf, si, phase).length > 0;
      return {
        ...state,
        workflow: wf,
        originalWorkflow: wf,
        selectedStepId: null,
        currentStageIndex: si,
        expandedPhases: {
          setup: phaseHasSteps("setup"),
          verification: phaseHasSteps("verification"),
          agentic: phaseHasSteps("agentic"),
          completion: phaseHasSteps("completion"),
        },
      };
    }

    case "UPDATE_WORKFLOW":
      return {
        ...state,
        workflow: { ...state.workflow, ...action.payload },
      };

    case "ADD_STEP": {
      const { step, phase } = action.payload;
      const stepWithId = { ...step, id: step.id || generateStepId() };

      // Check for duplicate IDs across all phases of the active context
      const allSteps = (
        ["setup", "verification", "agentic", "completion"] as WorkflowPhase[]
      ).flatMap((p) => getPhaseSteps(state.workflow, state.currentStageIndex, p));
      if (allSteps.some((s) => s.id === stepWithId.id)) {
        console.log("[WorkflowBuilder] Skipping duplicate step ID:", stepWithId.id);
        return state;
      }

      const existing = getPhaseSteps(state.workflow, state.currentStageIndex, phase);
      let newSteps: UnifiedStep[];

      if (phase === "completion") {
        const summaryIndex = existing.findIndex(
          (s) => s.type === "prompt" && (s as PromptStep).is_summary_step === true,
        );
        if (summaryIndex >= 0) {
          newSteps = [
            ...existing.slice(0, summaryIndex),
            stepWithId,
            ...existing.slice(summaryIndex),
          ];
        } else {
          newSteps = [...existing, stepWithId];
        }
      } else {
        newSteps = [...existing, stepWithId];
      }

      return {
        ...state,
        workflow: setPhaseSteps(state.workflow, state.currentStageIndex, phase, newSteps),
        selectedStepId: stepWithId.id,
        expandedPhases: { ...state.expandedPhases, [phase]: true },
      };
    }

    case "REMOVE_STEP": {
      const { stepId, phase } = action.payload;
      const steps = getPhaseSteps(state.workflow, state.currentStageIndex, phase);
      const filtered = steps.filter((s) => s.id !== stepId);
      return {
        ...state,
        workflow: setPhaseSteps(state.workflow, state.currentStageIndex, phase, filtered),
        selectedStepId: state.selectedStepId === stepId ? null : state.selectedStepId,
      };
    }

    case "UPDATE_STEP": {
      const { step, phase } = action.payload;
      const steps = getPhaseSteps(state.workflow, state.currentStageIndex, phase);
      const updated = steps.map((s) => (s.id === step.id ? step : s));
      return {
        ...state,
        workflow: setPhaseSteps(state.workflow, state.currentStageIndex, phase, updated),
      };
    }

    case "MOVE_STEP": {
      const { stepId, phase, direction } = action.payload;
      const steps = getPhaseSteps(state.workflow, state.currentStageIndex, phase);

      // Special handling for completion phase to protect summary step position
      if (phase === "completion") {
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
          const idx = steps.findIndex((s) => s.id === stepId);
          const nextStep = steps[idx + 1];
          if (nextStep && nextStep.type === "prompt" && (nextStep as PromptStep).is_summary_step) {
            return state;
          }
        }
      }

      const index = steps.findIndex((s) => s.id === stepId);
      if (index === -1) return state;
      if (direction === "up" && index === 0) return state;
      if (direction === "down" && index === steps.length - 1) return state;

      const newSteps = [...steps];
      const targetIndex = direction === "up" ? index - 1 : index + 1;
      [newSteps[index], newSteps[targetIndex]] = [newSteps[targetIndex], newSteps[index]];

      return {
        ...state,
        workflow: setPhaseSteps(state.workflow, state.currentStageIndex, phase, newSteps),
      };
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
        expandedPhases: {
          setup: false,
          verification: false,
          agentic: false,
          completion: false,
        },
      };
    }

    case "MARK_SAVED":
      return {
        ...state,
        originalWorkflow: state.workflow,
      };

    case "ADD_STAGE": {
      const existingStages = state.workflow.stages ?? [];
      if (existingStages.length === 0) {
        // Moving from single-phase to multi-phase: wrap top-level steps as Phase 1
        const phase1: WorkflowStage = {
          id: generateStepId(),
          name: state.workflow.name || "Phase 1",
          description: "",
          setup_steps: state.workflow.setup_steps,
          verification_steps: state.workflow.verification_steps,
          agentic_steps: state.workflow.agentic_steps,
          completion_steps: state.workflow.completion_steps ?? [],
          max_iterations: state.workflow.max_iterations ?? 10,
        };
        const newStage: WorkflowStage = {
          id: generateStepId(),
          name: action.payload.name,
          description: "",
          setup_steps: [],
          verification_steps: [],
          agentic_steps: [],
          completion_steps: [],
          max_iterations: state.workflow.max_iterations ?? 10,
        };
        return {
          ...state,
          workflow: {
            ...state.workflow,
            stages: [phase1, newStage],
            setup_steps: [],
            verification_steps: [],
            agentic_steps: [],
            completion_steps: [],
          },
          currentStageIndex: 1, // Select the newly added phase
        };
      }
      const newStage: WorkflowStage = {
        id: generateStepId(),
        name: action.payload.name,
        description: "",
        setup_steps: [],
        verification_steps: [],
        agentic_steps: [],
        completion_steps: [],
        max_iterations: state.workflow.max_iterations ?? 10,
      };
      return {
        ...state,
        workflow: {
          ...state.workflow,
          stages: [...existingStages, newStage],
        },
        currentStageIndex: existingStages.length,
      };
    }

    case "REMOVE_STAGE": {
      const { stageIndex } = action.payload;
      const stages = state.workflow.stages ?? [];
      if (stageIndex < 0 || stageIndex >= stages.length) return state;
      if (stages.length <= 1) {
        // Can't remove the last phase — this shouldn't happen with UI guard
        return state;
      }
      const newStages = stages.filter((_, i) => i !== stageIndex);
      let newCurrentStage = state.currentStageIndex;
      if (newStages.length === 1) {
        // Down to 1 phase — move steps back to top-level
        const sole = newStages[0];
        return {
          ...state,
          workflow: {
            ...state.workflow,
            stages: undefined,
            setup_steps: sole.setup_steps ?? [],
            verification_steps: sole.verification_steps ?? [],
            agentic_steps: sole.agentic_steps ?? [],
            completion_steps: sole.completion_steps ?? [],
          },
          currentStageIndex: null,
          selectedStepId: null,
        };
      }
      if (newCurrentStage !== null && newCurrentStage >= newStages.length) {
        newCurrentStage = newStages.length - 1;
      }
      return {
        ...state,
        workflow: {
          ...state.workflow,
          stages: newStages,
        },
        currentStageIndex: newCurrentStage,
        selectedStepId: null,
      };
    }

    case "SELECT_STAGE":
      return {
        ...state,
        currentStageIndex: action.payload,
        selectedStepId: null,
      };

    case "UPDATE_STAGE": {
      const { stageIndex, updates } = action.payload;
      const stages = state.workflow.stages ?? [];
      if (stageIndex < 0 || stageIndex >= stages.length) return state;
      const updatedStages = stages.map((s, i) => (i === stageIndex ? { ...s, ...updates } : s));
      return {
        ...state,
        workflow: { ...state.workflow, stages: updatedStages },
      };
    }

    case "MOVE_STAGE": {
      const { stageIndex, direction } = action.payload;
      const stages = state.workflow.stages ?? [];
      if (stageIndex < 0 || stageIndex >= stages.length) return state;
      if (direction === "up" && stageIndex === 0) return state;
      if (direction === "down" && stageIndex === stages.length - 1) return state;
      const targetIndex = direction === "up" ? stageIndex - 1 : stageIndex + 1;
      const newStages = [...stages];
      [newStages[stageIndex], newStages[targetIndex]] = [
        newStages[targetIndex],
        newStages[stageIndex],
      ];
      return {
        ...state,
        workflow: { ...state.workflow, stages: newStages },
        currentStageIndex: targetIndex,
      };
    }

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

  // Stage management
  currentStageIndex: number | null;
  currentStage: WorkflowStage | null;
  addStage: (name: string) => void;
  removeStage: (stageIndex: number) => void;
  selectStage: (stageIndex: number | null) => void;
  updateStage: (stageIndex: number, updates: Partial<WorkflowStage>) => void;
  moveStage: (stageIndex: number, direction: "up" | "down") => void;
  getActiveSteps: (phase: WorkflowPhase) => UnifiedStep[];
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
  /** When true, skip restoring draft from storage and start with an empty workflow */
  startEmpty?: boolean;
}

// Helper to load workflow from storage
function loadFromStorage(): UnifiedWorkflow | null {
  try {
    const parsed = instanceStorage.getJSON<UnifiedWorkflow | null>(STORAGE_KEY, null);
    // Validate it has the required structure
    if (parsed && typeof parsed === "object" && "setup_steps" in parsed) {
      return parsed as UnifiedWorkflow;
    }
  } catch (e) {
    console.warn("Failed to load workflow from storage:", e);
  }
  return null;
}

// Helper to load original workflow from storage (tracks if workflow was saved)
function loadOriginalFromStorage(): UnifiedWorkflow | null {
  try {
    const parsed = instanceStorage.getJSON<UnifiedWorkflow | null>(STORAGE_KEY_ORIGINAL, null);
    if (parsed && typeof parsed === "object" && "setup_steps" in parsed) {
      return parsed as UnifiedWorkflow;
    }
  } catch (e) {
    console.warn("Failed to load original workflow from storage:", e);
  }
  return null;
}

// Helper to save workflow to storage
function saveToStorage(workflow: UnifiedWorkflow): void {
  try {
    instanceStorage.setJSON(STORAGE_KEY, workflow);
  } catch (e) {
    console.warn("Failed to save workflow to storage:", e);
  }
}

// Helper to save original workflow to storage (called after successful save)
function saveOriginalToStorage(workflow: UnifiedWorkflow | null): void {
  try {
    if (workflow) {
      instanceStorage.setJSON(STORAGE_KEY_ORIGINAL, workflow);
    } else {
      instanceStorage.removeItem(STORAGE_KEY_ORIGINAL);
    }
  } catch (e) {
    console.warn("Failed to save original workflow to storage:", e);
  }
}

/**
 * Inner provider that manages the runner-specific state.
 * Wrapped by SharedBuilderProvider and WorkflowDataProvider.
 */
function RunnerWorkflowBuilderInner({
  children,
  initialWorkflow,
  startEmpty,
}: WorkflowBuilderProviderProps) {
  // Try to load from storage if no initial workflow provided (and not starting empty)
  const storedWorkflow = !initialWorkflow && !startEmpty ? loadFromStorage() : null;
  // Also load the original workflow to preserve update vs create state
  const storedOriginalWorkflow = !initialWorkflow && !startEmpty ? loadOriginalFromStorage() : null;

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
    currentStageIndex: (() => {
      const wf = initialWorkflow ?? storedWorkflow ?? emptyWorkflow;
      return wf.stages && wf.stages.length > 0 ? 0 : null;
    })(),
    expandedPhases: (() => {
      const wf = initialWorkflow ?? storedWorkflow ?? emptyWorkflow;
      const si = wf.stages && wf.stages.length > 0 ? 0 : null;
      const hasSteps = (phase: WorkflowPhase) => getPhaseSteps(wf, si, phase).length > 0;
      return {
        setup: hasSteps("setup"),
        verification: hasSteps("verification"),
        agentic: hasSteps("agentic"),
        completion: hasSteps("completion"),
      };
    })(),
    showSaveDialog: false,
    showAddDropdown: false,
    addDropdownPhase: null,
    isLoading: false,
    isSaving: false,
    error: null,
  };

  const [state, dispatch] = useReducer(workflowBuilderReducer, initialState);

  // Persist workflow to storage whenever it changes
  useEffect(() => {
    saveToStorage(state.workflow);
  }, [state.workflow]);

  // Persist originalWorkflow to storage to track whether workflow was saved
  // This ensures updates work correctly after app reload
  useEffect(() => {
    saveOriginalToStorage(state.originalWorkflow);
  }, [state.originalWorkflow]);

  // Load user-created skills into the registry on mount
  const refreshSkills = useCallback(async () => {
    try {
      const skills = (await runnerDataAdapter.fetchSkills?.()) ?? [];
      registerUserSkills(skills);
    } catch {
      // Skills loading is non-critical
    }
  }, []);

  useEffect(() => {
    refreshSkills();
  }, [refreshSkills]);

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
    const phases: WorkflowPhase[] = ["setup", "verification", "agentic", "completion"];
    for (const phase of phases) {
      const steps = getPhaseSteps(state.workflow, state.currentStageIndex, phase);
      const found = steps.find((s) => s.id === state.selectedStepId);
      if (found) return found;
    }
    return null;
  }, [state.selectedStepId, state.workflow, state.currentStageIndex]);

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

  // Stage management
  const currentStage = useMemo((): WorkflowStage | null => {
    if (state.currentStageIndex === null) return null;
    return state.workflow.stages?.[state.currentStageIndex] ?? null;
  }, [state.currentStageIndex, state.workflow.stages]);

  const addStage = useCallback((name: string) => {
    dispatch({ type: "ADD_STAGE", payload: { name } });
  }, []);

  const removeStage = useCallback((stageIndex: number) => {
    dispatch({ type: "REMOVE_STAGE", payload: { stageIndex } });
  }, []);

  const selectStage = useCallback((stageIndex: number | null) => {
    dispatch({ type: "SELECT_STAGE", payload: stageIndex });
  }, []);

  const updateStage = useCallback((stageIndex: number, updates: Partial<WorkflowStage>) => {
    dispatch({ type: "UPDATE_STAGE", payload: { stageIndex, updates } });
  }, []);

  const moveStage = useCallback((stageIndex: number, direction: "up" | "down") => {
    dispatch({ type: "MOVE_STAGE", payload: { stageIndex, direction } });
  }, []);

  const getActiveSteps = useCallback(
    (phase: WorkflowPhase): UnifiedStep[] => {
      return getPhaseSteps(state.workflow, state.currentStageIndex, phase);
    },
    [state.workflow, state.currentStageIndex],
  );

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
        log_watch_enabled: workflow.log_watch_enabled,
        health_check_enabled: workflow.health_check_enabled,
        health_check_urls: workflow.health_check_urls,
        stages: workflow.stages,
        stop_on_failure: workflow.stop_on_failure,
        reflection_mode: workflow.reflection_mode,
        model_overrides: workflow.model_overrides,
      };

      const url = isNew
        ? `${getApiBase()}/unified-workflows`
        : `${getApiBase()}/unified-workflows/${workflow.id}`;
      const method = isNew ? "POST" : "PUT";

      const response = await tracedFetch(url, {
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
      const response = await tracedFetch(`${getApiBase()}/unified-workflows/${id}`);
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
  const exportWorkflow = useCallback(async (id: string): Promise<WorkflowExport | null> => {
    dispatch({ type: "SET_LOADING", payload: true });
    dispatch({ type: "SET_ERROR", payload: null });

    try {
      const response = await tracedFetch(`${getApiBase()}/unified-workflows/${id}/export`);
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
  }, []);

  // Import a workflow
  const importWorkflow = useCallback(
    async (
      workflow: UnifiedWorkflow,
      conflictStrategy: "keep" | "generate" | "overwrite" = "generate",
    ): Promise<WorkflowImportResult | null> => {
      dispatch({ type: "SET_LOADING", payload: true });
      dispatch({ type: "SET_ERROR", payload: null });

      try {
        const response = await tracedFetch(`${getApiBase()}/unified-workflows/import`, {
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
    currentStageIndex: state.currentStageIndex,
    currentStage,
    addStage,
    removeStage,
    selectStage,
    updateStage,
    moveStage,
    getActiveSteps,
  };

  return (
    <WorkflowBuilderContext.Provider value={value}>{children}</WorkflowBuilderContext.Provider>
  );
}

/**
 * WorkflowBuilderProvider wraps the runner-specific context
 * with the shared @qontinui/workflow-ui providers, making both
 * the runner's rich API and the shared headless context available.
 */
export function WorkflowBuilderProvider({
  children,
  initialWorkflow,
  startEmpty,
}: WorkflowBuilderProviderProps) {
  return (
    <WorkflowDataProvider adapter={runnerDataAdapter}>
      <SharedBuilderProvider initialWorkflow={initialWorkflow}>
        <RunnerWorkflowBuilderInner initialWorkflow={initialWorkflow} startEmpty={startEmpty}>
          {children}
        </RunnerWorkflowBuilderInner>
      </SharedBuilderProvider>
    </WorkflowDataProvider>
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
