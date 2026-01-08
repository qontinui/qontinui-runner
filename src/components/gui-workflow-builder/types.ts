/**
 * Local types for the GUI Workflow Builder
 */

import type { GuiActionType, GuiWorkflowStep, SavedGuiWorkflow } from "../../types/gui-workflow";

/** State information parsed from loaded config */
export interface StateInfo {
  id: string;
  name: string;
  description?: string;
}

/** Image information parsed from loaded config */
export interface ImageInfo {
  id: string;
  name: string;
  stateName: string;
}

/** Props for the main GuiWorkflowBuilderTab component */
export interface GuiWorkflowBuilderTabProps {
  editWorkflowId?: string | null;
  onNavigateToLibrary?: () => void;
}

/** Result message for user feedback */
export interface ResultMessage {
  success: boolean;
  message: string;
}

/** Form state for creating/editing a workflow */
export interface WorkflowFormState {
  name: string;
  description: string;
  category: string;
  tags: string[];
}

/** Context value for the GUI Builder */
export interface GuiBuilderContextValue {
  // Workflow Steps
  steps: GuiWorkflowStep[];
  setSteps: React.Dispatch<React.SetStateAction<GuiWorkflowStep[]>>;
  addStep: (actionType: GuiActionType) => void;
  updateStep: (stepId: string, updates: Partial<GuiWorkflowStep>) => void;
  removeStep: (stepId: string) => void;
  moveStepUp: (index: number) => void;
  moveStepDown: (index: number) => void;
  reorderSteps: (startIndex: number, endIndex: number) => void;

  // Workflow Management
  currentWorkflowId: string | null;
  currentWorkflowName: string;
  hasUnsavedChanges: boolean;
  savedWorkflows: SavedGuiWorkflow[];
  loadWorkflow: (workflow: SavedGuiWorkflow) => void;
  deleteWorkflow: (workflowId: string) => Promise<void>;
  refreshWorkflows: () => Promise<void>;

  // Form State
  formState: WorkflowFormState;
  setFormState: React.Dispatch<React.SetStateAction<WorkflowFormState>>;

  // Editing State
  editingStepId: string | null;
  setEditingStepId: (stepId: string | null) => void;

  // Save Dialog
  showSaveDialog: boolean;
  setShowSaveDialog: (show: boolean) => void;
  isSaving: boolean;
  handleSaveWorkflow: () => Promise<void>;
  handleSaveAsNew: () => Promise<void>;

  // Workflows Panel
  showWorkflowsPanel: boolean;
  setShowWorkflowsPanel: (show: boolean) => void;

  // Config Data (from loaded configuration)
  configLoaded: boolean;
  states: StateInfo[];
  images: ImageInfo[];

  // Execution
  isRunning: boolean;
  runWorkflow: () => Promise<void>;
  lastResult: ResultMessage | null;

  // Actions
  handleNewWorkflow: () => void;
}
