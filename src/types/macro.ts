/**
 * Macro Types
 *
 * Type definitions for Macros - deterministic sequences of
 * GUI automation actions that can be composed, saved, and executed.
 */

/**
 * Action types supported by the Macro Builder
 */
export type MacroActionType =
  | "click"
  | "double_click"
  | "right_click"
  | "type"
  | "hotkey"
  | "go_to_state";

/**
 * A single step in a Macro
 */
export interface MacroStep {
  /** Unique identifier (UUID v4) */
  id: string;
  /** Action type */
  action_type: MacroActionType;
  /** Display name for the step */
  name: string;

  // Click action configuration
  /** Target StateImage IDs (one or more) - first match will be clicked */
  target_image_ids?: string[];
  /** Target StateImage names (for display) */
  target_image_names?: string[];

  // Type action configuration
  /** Text to type (for "type" action) */
  text_input?: string;

  // Hotkey action configuration
  /** Key combination string (for "hotkey" action), e.g., "Ctrl+C", "Alt+Tab" */
  hotkey?: string;

  // Go to state configuration
  /** Target state IDs (one or more) - navigates to any of these */
  target_state_ids?: string[];
  /** Target state names (for display) */
  target_state_names?: string[];

  // Timing configuration
  /** Pause after this step in milliseconds */
  pause_after_ms?: number;

  // Execution settings
  /** Monitor index (0 = primary, undefined = all monitors) */
  monitor_index?: number;
  /** Timeout for this step in seconds */
  timeout_seconds?: number;
}

/**
 * A saved Macro
 */
export interface SavedMacro {
  /** Unique identifier (UUID v4) */
  id: string;
  /** Display name */
  name: string;
  /** Description of what this macro does */
  description: string;
  /** Ordered list of steps */
  steps: MacroStep[];
  /** Category for organization */
  category: string;
  /** Tags for filtering */
  tags: string[];
  /** Creation timestamp (ISO 8601) */
  created_at: string;
  /** Last modification timestamp (ISO 8601) */
  modified_at: string;
  /** Number of times this macro has been run */
  run_count: number;
}

/**
 * Request to create a new macro
 */
export interface CreateMacroRequest {
  name: string;
  description?: string;
  steps: MacroStep[];
  category?: string;
  tags?: string[];
}

/**
 * Request to update an existing macro
 */
export interface UpdateMacroRequest {
  name?: string;
  description?: string;
  steps?: MacroStep[];
  category?: string;
  tags?: string[];
}

/**
 * Result of running a macro step
 */
export interface MacroStepResult {
  step_index: number;
  step_name: string;
  action_type: string;
  success: boolean;
  error?: string;
  duration_ms: number;
}

/**
 * Result of running a macro
 */
export interface MacroRunResult {
  macro_id: string;
  macro_name: string;
  total_steps: number;
  successful_steps: number;
  failed_steps: number;
  duration_ms: number;
  step_results: MacroStepResult[];
}

/**
 * Get default values for a new macro step
 */
export function getDefaultMacroStep(
  actionType: MacroActionType = "click",
): Omit<MacroStep, "id"> {
  return {
    action_type: actionType,
    name: getDefaultStepName(actionType),
    pause_after_ms: undefined,
    monitor_index: undefined,
    timeout_seconds: undefined,
  };
}

/**
 * Get default step name based on action type
 */
export function getDefaultStepName(actionType: MacroActionType): string {
  switch (actionType) {
    case "click":
      return "Click";
    case "double_click":
      return "Double Click";
    case "right_click":
      return "Right Click";
    case "type":
      return "Type Text";
    case "hotkey":
      return "Press Hotkey";
    case "go_to_state":
      return "Go to State";
    default:
      return "Action";
  }
}

/**
 * Get default values for a new macro
 */
export function getDefaultMacro(): Omit<
  SavedMacro,
  "id" | "created_at" | "modified_at"
> {
  return {
    name: "",
    description: "",
    steps: [],
    category: "general",
    tags: [],
    run_count: 0,
  };
}

/**
 * Get icon name for an action type (Lucide icon names)
 */
export function getActionTypeIcon(actionType: MacroActionType): string {
  switch (actionType) {
    case "click":
      return "MousePointer2";
    case "double_click":
      return "MousePointerClick";
    case "right_click":
      return "MousePointer";
    case "type":
      return "Keyboard";
    case "hotkey":
      return "Command";
    case "go_to_state":
      return "Navigation";
    default:
      return "CircleDot";
  }
}

/**
 * Get display label for an action type
 */
export function getActionTypeLabel(actionType: MacroActionType): string {
  switch (actionType) {
    case "click":
      return "Click";
    case "double_click":
      return "Double Click";
    case "right_click":
      return "Right Click";
    case "type":
      return "Type";
    case "hotkey":
      return "Hotkey";
    case "go_to_state":
      return "Go to State";
    default:
      return actionType;
  }
}

/**
 * Validate a macro step
 */
export function validateMacroStep(step: MacroStep): {
  valid: boolean;
  errors: string[];
} {
  const errors: string[] = [];

  if (!step.name || step.name.trim() === "") {
    errors.push("Step name is required");
  }

  switch (step.action_type) {
    case "click":
    case "double_click":
    case "right_click":
      if (!step.target_image_ids || step.target_image_ids.length === 0) {
        errors.push("At least one target image is required for click actions");
      }
      break;
    case "type":
      if (!step.text_input || step.text_input.trim() === "") {
        errors.push("Text input is required for type action");
      }
      break;
    case "hotkey":
      if (!step.hotkey || step.hotkey.trim() === "") {
        errors.push("Hotkey combination is required");
      }
      break;
    case "go_to_state":
      if (!step.target_state_ids || step.target_state_ids.length === 0) {
        errors.push(
          "At least one target state is required for go_to_state action",
        );
      }
      break;
  }

  return { valid: errors.length === 0, errors };
}

/**
 * Validate a complete macro
 */
export function validateMacro(macro: Partial<SavedMacro>): {
  valid: boolean;
  errors: string[];
} {
  const errors: string[] = [];

  if (!macro.name || macro.name.trim() === "") {
    errors.push("Macro name is required");
  }

  if (!macro.steps || macro.steps.length === 0) {
    errors.push("At least one step is required");
  } else {
    macro.steps.forEach((step, index) => {
      const stepValidation = validateMacroStep(step);
      if (!stepValidation.valid) {
        stepValidation.errors.forEach((error) => {
          errors.push(`Step ${index + 1}: ${error}`);
        });
      }
    });
  }

  return { valid: errors.length === 0, errors };
}
