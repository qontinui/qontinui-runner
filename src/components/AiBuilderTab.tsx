/**
 * AiBuilderTab.tsx
 *
 * AI Automation Builder panel that allows users to:
 * 1. Build an ordered list of workflows and states to execute
 * 2. Configure screenshot capture for each step
 * 3. Enter natural language goals
 * 4. Generate and run AI-powered recursive automation
 *
 * This file re-exports the refactored modular implementation
 * from ./ai-builder/ for backward compatibility.
 */

// Re-export everything from the refactored module
export {
  // Main component
  AiBuilderTab,
  AiBuilderTab as default,
  // Types
  type ExecutionStep,
  type SavedAiWorkflow,
  type PromptHistoryEntry,
  type StateInfo,
  type ImageInfo,
  type WorkflowInfo,
  type PlaywrightScriptInfo,
  type SavedPromptInfo,
  type AiDeveloperState,
  type Session,
  type SessionCheckpoint,
  type SessionConfig,
  type WorkspacePaths,
  type ResumableWorkflow,
  type ResultMessage,
  type AiBuilderTabProps,
  // Hooks and Context
  useAiBuilder,
  AiBuilderProvider,
  useAiBuilderState,
  // Constants
  DEFAULT_DEVELOPER_PROMPT_TEMPLATE,
  getDeveloperPromptTemplate,
  saveCustomPromptTemplate,
  resetPromptTemplateToDefault,
  isUsingCustomTemplate,
  // Individual Components
  Header,
  ExecutionStepsList,
  ExecutionStepItem,
  AddStepDropdown,
  SavedWorkflowsPanel,
  GoalInput,
  SettingsPanel,
  AdvancedSettingsPanel,
  ResumeWorkflowBanner,
  ActionButtons,
  SessionStatus,
  ResultMessageComponent,
  RunningIndicator,
  PromptPreview,
  HistoryPanel,
  AvailableImagesPanel,
  SaveWorkflowDialog,
  NewWorkflowConfirmDialog,
  AiBuilderContent,
} from "./ai-builder";
