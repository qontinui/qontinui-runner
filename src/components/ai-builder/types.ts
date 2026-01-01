/**
 * types.ts
 *
 * Shared type definitions for the AI Builder components.
 */

import type { UseProjectLogsReturn } from "../../hooks/useProjectLogs";

/** State information parsed from config */
export interface StateInfo {
  name: string;
  description?: string;
  images: string[];
}

/** Image information parsed from config */
export interface ImageInfo {
  id: string;
  name: string;
  stateName: string;
}

/** Workflow information parsed from config */
export interface WorkflowInfo {
  id: string;
  name: string;
  category?: string;
}

/** Playwright script info for the dropdown */
export interface PlaywrightScriptInfo {
  id: string;
  name: string;
  target_url: string;
  description: string;
  script_content: string;
}

/** Saved prompt info for the dropdown */
export interface SavedPromptInfo {
  id: string;
  name: string;
  description: string;
  content: string;
  category: string;
}

/** A single step in the execution sequence */
export interface ExecutionStep {
  id: string;
  type: "workflow" | "state" | "playwright" | "prompt" | "action" | "screenshot";
  name: string;
  takeScreenshot: boolean;
  screenshotDelay?: number;
  playwrightScriptId?: string;
  playwrightScriptContent?: string;
  playwrightTargetUrl?: string;
  promptId?: string;
  promptContent?: string;
  actionType?: "click" | "double_click" | "right_click";
  targetImageId?: string;
  targetImageName?: string;
  screenshotMonitor?: number | "all";
}

/** Saved AI Workflow from the AI Workflows library */
export interface SavedAiWorkflow {
  id: string;
  name: string;
  description: string;
  steps: ExecutionStep[];
  goal: string;
  max_iterations: number;
  persistent_session: boolean;
  capture_input_validation: boolean;
  category: string;
  tags: string[];
  created_at: string;
  modified_at: string;
}

/** Prompt history entry */
export interface PromptHistoryEntry {
  id: string;
  timestamp: number;
  steps: ExecutionStep[];
  goal: string;
  success?: boolean;
}

/** Workspace paths from backend */
export interface WorkspacePaths {
  workspace_root: string;
  dev_logs_path: string;
  scripts_path: string;
  workspace_root_escaped: string;
  dev_logs_path_escaped: string;
}

/** AI Developer session state */
export interface AiDeveloperState {
  session_id: string;
  iteration: number;
  max_iterations: number;
  status: string;
  started_at: string;
  stop_requested: boolean;
  current_action: string;
  errors_fixed: Array<{ file: string; line?: number; description: string; fixed_at: string }>;
  errors_remaining: Array<{ file: string; error_type: string; context: string }>;
}

/** Session checkpoint from unified sessions API */
export interface SessionCheckpoint {
  session_id: string;
  current_phase: number;
  total_phases: number;
  completed: boolean;
  status: string;
  started_at: string;
  last_activity: string;
  sessions_spawned: number;
  restart_permitted: boolean;
  error_message: string | null;
  custom_data: Record<string, unknown>;
  activity_log: string[];
}

/** Session config from unified sessions API */
export interface SessionConfig {
  prompt: string;
  continuation_prompt: string | null;
  total_phases: number;
  uses_gui: boolean;
  timeout_seconds: number;
  stall_threshold_seconds: number;
  name: string;
  description: string;
  custom_config: Record<string, unknown>;
}

/** Session from unified sessions API */
export interface Session {
  id: string;
  config: SessionConfig;
  status:
    | "starting"
    | "running"
    | "completed"
    | "failed"
    | "stopped"
    | "waiting_for_continuation"
    | "stalled";
  checkpoint: SessionCheckpoint;
  active_subprocess_id: string | null;
  event_log: { timestamp: number; event_type: string; message: string }[];
}

/** Resumable workflow state */
export interface ResumableWorkflow {
  name: string;
  currentPhase: number;
  totalPhases: number;
  status: string;
}

/** Result message display */
export interface ResultMessage {
  success: boolean;
  message: string;
}

/** Props for the main AiBuilderTab component */
export interface AiBuilderTabProps {
  projectLogs: UseProjectLogsReturn;
  onNavigateToLogLocations: () => void;
}

/** Shared context for AI Builder state */
export interface AiBuilderContextValue {
  // Execution Steps
  executionSteps: ExecutionStep[];
  setExecutionSteps: React.Dispatch<React.SetStateAction<ExecutionStep[]>>;
  addStep: (
    type: "workflow" | "state" | "playwright" | "prompt",
    name: string,
    scriptId?: string,
    scriptContent?: string,
    scriptTargetUrl?: string,
    promptId?: string,
    promptContent?: string,
  ) => void;
  addActionStep: (
    actionType: "click" | "double_click" | "right_click",
    imageId: string,
    imageName: string,
  ) => void;
  addScreenshotStep: () => void;
  removeStep: (stepId: string) => void;
  toggleStepScreenshot: (stepId: string) => void;
  updateScreenshotDelay: (stepId: string, delay: number) => void;
  updateScreenshotMonitor: (stepId: string, monitor: number | "all") => void;
  moveStepUp: (index: number) => void;
  moveStepDown: (index: number) => void;

  // Goal
  goal: string;
  setGoal: (goal: string) => void;

  // Settings
  maxIterations: number;
  setMaxIterations: (iterations: number) => void;
  captureInputValidation: boolean;
  setCaptureInputValidation: (enabled: boolean) => void;

  // Workflow Management
  currentWorkflowId: string | null;
  currentWorkflowName: string;
  hasUnsavedChanges: boolean;
  savedAiWorkflows: SavedAiWorkflow[];
  loadAiWorkflow: (workflow: SavedAiWorkflow) => void;
  deleteAiWorkflow: (workflowId: string) => Promise<void>;
  handleNewWorkflow: () => void;
  handleSaveWorkflow: () => Promise<void>;

  // Session State
  currentSessionId: string | null;
  sessionState: AiDeveloperState | null;
  isRunning: boolean;
  lastResult: ResultMessage | null;

  // Resumable Workflow
  resumableWorkflow: ResumableWorkflow | null;
  isResuming: boolean;
  handleResumeWorkflow: () => Promise<void>;

  // Actions
  runAutomation: () => Promise<void>;
  stopSession: () => Promise<void>;
  copyPrompt: () => Promise<void>;
  generatePrompt: (sessionId: string) => string;

  // History
  history: PromptHistoryEntry[];
  loadFromHistory: (entry: PromptHistoryEntry) => void;
  getHistorySummary: (entry: PromptHistoryEntry) => string;

  // Dropdowns/Dialogs
  showAddDropdown: boolean;
  setShowAddDropdown: (show: boolean) => void;
  showWorkflowsPanel: boolean;
  setShowWorkflowsPanel: (show: boolean) => void;
  showSaveDialog: boolean;
  setShowSaveDialog: (show: boolean) => void;
  showNewConfirmDialog: boolean;
  setShowNewConfirmDialog: (show: boolean) => void;

  // Save Dialog State
  saveName: string;
  setSaveName: (name: string) => void;
  saveDescription: string;
  setSaveDescription: (desc: string) => void;
  isSaving: boolean;
  saveAsNewWorkflow: () => Promise<void>;
  resetToNewWorkflow: () => void;

  // Prompt Template
  isEditingPromptTemplate: boolean;
  setIsEditingPromptTemplate: (editing: boolean) => void;
  editedPromptTemplate: string;
  setEditedPromptTemplate: (template: string) => void;
  usingCustomTemplate: boolean;
  handleSavePromptTemplate: () => void;
  handleResetPromptTemplate: () => void;

  // Config Data
  states: StateInfo[];
  images: ImageInfo[];
  workflows: WorkflowInfo[];
  playwrightScripts: PlaywrightScriptInfo[];
  savedPrompts: SavedPromptInfo[];
  workspacePaths: WorkspacePaths | null;

  // External Props
  projectLogs: UseProjectLogsReturn;
  onNavigateToLogLocations: () => void;

  // Config Loading
  configLoaded: boolean;
  loadConfiguration: () => void;

  // Set Result
  setLastResult: (result: ResultMessage | null) => void;
}

/** Convert Session to AiDeveloperState for UI compatibility */
export function sessionToAiDeveloperState(session: Session): AiDeveloperState {
  return {
    session_id: session.id,
    iteration: session.checkpoint.current_phase,
    max_iterations: session.config.total_phases,
    status: session.status === "waiting_for_continuation" ? "idle" : session.status,
    started_at: session.checkpoint.started_at,
    stop_requested: false,
    current_action:
      session.event_log.length > 0 ? session.event_log[session.event_log.length - 1].message : "",
    errors_fixed: [],
    errors_remaining: [],
  };
}
