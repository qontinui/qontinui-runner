/**
 * AiBuilderContent.tsx
 *
 * Main content layout for the AI Builder tab.
 * Composes all the focused UI components into the full layout.
 */

import { Header } from "./Header";
import { ExecutionStepsList } from "./ExecutionStepsList";
import { SavedWorkflowsPanel } from "./SavedWorkflowsPanel";
import { GoalInput } from "./GoalInput";
import { SettingsPanel } from "./SettingsPanel";
import { AdvancedSettingsPanel } from "./AdvancedSettingsPanel";
import { ResumeWorkflowBanner } from "./ResumeWorkflowBanner";
import { ActionButtons } from "./ActionButtons";
import { SessionStatus } from "./SessionStatus";
import { ResultMessage } from "./ResultMessage";
import { RunningIndicator } from "./RunningIndicator";
import { PromptPreview } from "./PromptPreview";
import { HistoryPanel } from "./HistoryPanel";
import { AvailableImagesPanel } from "./AvailableImagesPanel";
import { SaveWorkflowDialog } from "./SaveWorkflowDialog";
import { NewWorkflowConfirmDialog } from "./NewWorkflowConfirmDialog";

export function AiBuilderContent() {
  return (
    <div className="p-6 space-y-6">
      {/* Header */}
      <Header />

      {/* Main Layout */}
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
        {/* Left Panel - Configuration */}
        <div className="space-y-4">
          {/* Execution Steps */}
          <ExecutionStepsList />

          {/* Saved Workflows */}
          <SavedWorkflowsPanel />

          {/* Goal */}
          <GoalInput />

          {/* Settings */}
          <SettingsPanel />

          {/* Advanced Settings */}
          <AdvancedSettingsPanel />

          {/* Resume Workflow Banner */}
          <ResumeWorkflowBanner />

          {/* Action Buttons */}
          <ActionButtons />

          {/* Session Status */}
          <SessionStatus />

          {/* Result Message */}
          <ResultMessage />
        </div>

        {/* Right Panel - Preview & History */}
        <div className="space-y-4">
          {/* Running Indicator */}
          <RunningIndicator />

          {/* Prompt Preview */}
          <PromptPreview />

          {/* History */}
          <HistoryPanel />

          {/* Available Images */}
          <AvailableImagesPanel />
        </div>
      </div>

      {/* Modal Dialogs */}
      <SaveWorkflowDialog />
      <NewWorkflowConfirmDialog />
    </div>
  );
}
