/**
 * AiBuilderContent.tsx
 *
 * Main content layout for the AI Builder tab.
 * Composes all the focused UI components into the full layout.
 */

import { Header } from "./Header";
import { ExecutionStepsList } from "./ExecutionStepsList";
import { SavedWorkflowsPanel } from "./SavedWorkflowsPanel";
import { SettingsPanel } from "./SettingsPanel";
import { AdvancedSettingsPanel } from "./AdvancedSettingsPanel";
import { ResumeWorkflowBanner } from "./ResumeWorkflowBanner";
import { ActionButtons } from "./ActionButtons";
import { SessionStatus } from "./SessionStatus";
import { ResultMessage } from "./ResultMessage";
import { RunningIndicator } from "./RunningIndicator";
import { PromptComponentsView } from "./PromptComponentsView";
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

          {/* Settings */}
          <SettingsPanel />

          {/* Advanced Settings */}
          <AdvancedSettingsPanel />

          {/* Resume Workflow Banner */}
          <ResumeWorkflowBanner />

          {/* Action Buttons (Start AI Analysis) */}
          <ActionButtons />

          {/* Saved Workflows */}
          <SavedWorkflowsPanel />

          {/* Recent Runs (History) */}
          <HistoryPanel />

          {/* Session Status */}
          <SessionStatus />

          {/* Result Message */}
          <ResultMessage />
        </div>

        {/* Right Panel - Preview & Status */}
        <div className="space-y-4">
          {/* Running Indicator */}
          <RunningIndicator />

          {/* Prompt Components View */}
          <PromptComponentsView />

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
