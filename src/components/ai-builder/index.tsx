/**
 * index.tsx
 *
 * Main entry point for the refactored AI Builder module.
 * Provides the AiBuilderTab component that wraps everything with the context provider.
 */

import type { UseProjectLogsReturn } from "../../hooks/useProjectLogs";
import { AiBuilderProvider } from "./AiBuilderContext";
import { AiBuilderContent } from "./AiBuilderContent";

// Re-export types for external use
export type {
  ExecutionStep,
  SavedAiWorkflow,
  PromptHistoryEntry,
  StateInfo,
  ImageInfo,
  WorkflowInfo,
  PlaywrightScriptInfo,
  SavedPromptInfo,
  AiDeveloperState,
  Session,
  SessionCheckpoint,
  SessionConfig,
  WorkspacePaths,
  ResumableWorkflow,
  ResultMessage,
  AiBuilderTabProps,
} from "./types";

// Re-export hooks and context for advanced use cases
export { useAiBuilder, AiBuilderProvider } from "./AiBuilderContext";
export { useAiBuilderState } from "./useAiBuilderState";

// Re-export constants
export {
  DEFAULT_DEVELOPER_PROMPT_TEMPLATE,
  getDeveloperPromptTemplate,
  saveCustomPromptTemplate,
  resetPromptTemplateToDefault,
  isUsingCustomTemplate,
} from "./constants";

// Re-export individual components for customization
export { Header } from "./Header";
export { ExecutionStepsList } from "./ExecutionStepsList";
export { ExecutionStepItem } from "./ExecutionStepItem";
export { AddStepDropdown } from "./AddStepDropdown";
export { SavedWorkflowsPanel } from "./SavedWorkflowsPanel";
export { GoalInput } from "./GoalInput";
export { SettingsPanel } from "./SettingsPanel";
export { AdvancedSettingsPanel } from "./AdvancedSettingsPanel";
export { ResumeWorkflowBanner } from "./ResumeWorkflowBanner";
export { ActionButtons } from "./ActionButtons";
export { SessionStatus } from "./SessionStatus";
export { ResultMessage as ResultMessageComponent } from "./ResultMessage";
export { RunningIndicator } from "./RunningIndicator";
export { PromptPreview } from "./PromptPreview";
export { HistoryPanel } from "./HistoryPanel";
export { AvailableImagesPanel } from "./AvailableImagesPanel";
export { SaveWorkflowDialog } from "./SaveWorkflowDialog";
export { NewWorkflowConfirmDialog } from "./NewWorkflowConfirmDialog";
export { AiBuilderContent } from "./AiBuilderContent";

interface AiBuilderTabProps {
  projectLogs: UseProjectLogsReturn;
  onNavigateToLogLocations: () => void;
}

/**
 * Main AI Builder Tab component.
 * This is the primary export that wraps the content with the context provider.
 */
export function AiBuilderTab({ projectLogs, onNavigateToLogLocations }: AiBuilderTabProps) {
  return (
    <AiBuilderProvider
      projectLogs={projectLogs}
      onNavigateToLogLocations={onNavigateToLogLocations}
    >
      <AiBuilderContent />
    </AiBuilderProvider>
  );
}

// Default export for convenience
export default AiBuilderTab;
