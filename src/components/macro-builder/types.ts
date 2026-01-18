/**
 * Local types for the Macro Builder
 */

import type { MacroActionType, MacroStep, SavedMacro } from "../../types/macro";

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

/** Props for the main MacroBuilderTab component */
export interface MacroBuilderTabProps {
  editMacroId?: string | null;
  onNavigateToLibrary?: () => void;
}

/** Result message for user feedback */
export interface ResultMessage {
  success: boolean;
  message: string;
}

/** Form state for creating/editing a macro */
export interface MacroFormState {
  name: string;
  description: string;
  category: string;
  tags: string[];
}

/** Context value for the Macro Builder */
export interface MacroBuilderContextValue {
  // Macro Steps
  steps: MacroStep[];
  setSteps: React.Dispatch<React.SetStateAction<MacroStep[]>>;
  addStep: (actionType: MacroActionType) => void;
  updateStep: (stepId: string, updates: Partial<MacroStep>) => void;
  removeStep: (stepId: string) => void;
  moveStepUp: (index: number) => void;
  moveStepDown: (index: number) => void;
  reorderSteps: (startIndex: number, endIndex: number) => void;

  // Macro Management
  currentMacroId: string | null;
  currentMacroName: string;
  hasUnsavedChanges: boolean;
  savedMacros: SavedMacro[];
  loadMacro: (macro: SavedMacro) => void;
  deleteMacro: (macroId: string) => Promise<void>;
  refreshMacros: () => Promise<void>;

  // Form State
  formState: MacroFormState;
  setFormState: React.Dispatch<React.SetStateAction<MacroFormState>>;

  // Editing State
  editingStepId: string | null;
  setEditingStepId: (stepId: string | null) => void;

  // Save Dialog
  showSaveDialog: boolean;
  setShowSaveDialog: (show: boolean) => void;
  isSaving: boolean;
  handleSaveMacro: () => Promise<void>;
  handleSaveAsNew: () => Promise<void>;

  // Macros Panel
  showMacrosPanel: boolean;
  setShowMacrosPanel: (show: boolean) => void;

  // Config Data (from loaded configuration)
  configLoaded: boolean;
  states: StateInfo[];
  images: ImageInfo[];

  // Execution
  isRunning: boolean;
  runMacro: () => Promise<void>;
  lastResult: ResultMessage | null;

  // Actions
  handleNewMacro: () => void;
}
