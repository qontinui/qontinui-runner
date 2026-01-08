/**
 * GUI Workflow Builder
 *
 * Main component for building sequential GUI action workflows.
 * Provides a linear step editor for composing CLICK, TYPE, HOTKEY, and GO_TO_STATE actions.
 */

import { useMemo } from "react";
import {
  Play,
  Save,
  FolderOpen,
  Plus,
  AlertCircle,
  CheckCircle,
  XCircle,
  Loader2,
  FileText,
} from "lucide-react";
import { cn } from "../../lib/utils";

import { GuiBuilderProvider, useGuiBuilder } from "./GuiBuilderContext";
import { useGuiBuilderState } from "./useGuiBuilderState";
import { StepItem } from "./StepItem";
import { AddStepDropdown } from "./AddStepDropdown";
import { StepEditor } from "./StepEditor";
import { WorkflowsPanel } from "./WorkflowsPanel";
import { SaveWorkflowDialog } from "./SaveWorkflowDialog";
import type { GuiWorkflowBuilderTabProps } from "./types";

function GuiBuilderContent() {
  const {
    steps,
    addStep,
    removeStep,
    moveStepUp,
    moveStepDown,
    currentWorkflowId,
    hasUnsavedChanges,
    formState,
    setFormState,
    editingStepId,
    setEditingStepId,
    showWorkflowsPanel,
    setShowWorkflowsPanel,
    setShowSaveDialog,
    handleSaveWorkflow,
    handleNewWorkflow,
    configLoaded,
    isRunning,
    runWorkflow,
    lastResult,
  } = useGuiBuilder();

  const editingStep = useMemo(
    () => steps.find((s) => s.id === editingStepId) || null,
    [steps, editingStepId],
  );

  return (
    <div className="flex h-full">
      {/* Main Content */}
      <div className="flex-1 flex flex-col min-w-0">
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-border">
          <div className="flex items-center gap-4">
            <div className="flex items-center gap-2">
              <FileText className="h-5 w-5 text-primary" />
              <input
                type="text"
                value={formState.name}
                onChange={(e) => setFormState((prev) => ({ ...prev, name: e.target.value }))}
                placeholder="Untitled Workflow"
                className="h-8 w-48 px-2 bg-background border border-border rounded-md text-sm font-medium focus:outline-none focus:ring-2 focus:ring-primary"
              />
              {hasUnsavedChanges && (
                <span className="px-2 py-0.5 text-xs border border-border rounded-md text-muted-foreground">
                  Unsaved
                </span>
              )}
            </div>
          </div>

          <div className="flex items-center gap-2">
            <button
              onClick={handleNewWorkflow}
              disabled={isRunning}
              className="flex items-center gap-1 px-3 py-1.5 text-sm font-medium border border-border rounded-md hover:bg-muted transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            >
              <Plus className="h-4 w-4" />
              New
            </button>
            <button
              onClick={() => setShowWorkflowsPanel(!showWorkflowsPanel)}
              className="flex items-center gap-1 px-3 py-1.5 text-sm font-medium border border-border rounded-md hover:bg-muted transition-colors"
            >
              <FolderOpen className="h-4 w-4" />
              Workflows
            </button>
            <button
              onClick={currentWorkflowId ? handleSaveWorkflow : () => setShowSaveDialog(true)}
              disabled={isRunning}
              className="flex items-center gap-1 px-3 py-1.5 text-sm font-medium border border-border rounded-md hover:bg-muted transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            >
              <Save className="h-4 w-4" />
              Save
            </button>
            <button
              onClick={runWorkflow}
              disabled={isRunning || steps.length === 0}
              className="flex items-center gap-1 px-3 py-1.5 text-sm font-medium bg-primary text-primary-foreground rounded-md hover:bg-primary/90 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            >
              {isRunning ? (
                <Loader2 className="h-4 w-4 animate-spin" />
              ) : (
                <Play className="h-4 w-4" />
              )}
              Run
            </button>
          </div>
        </div>

        {/* Result Banner */}
        {lastResult && (
          <div
            className={cn(
              "flex items-center gap-2 px-4 py-2 text-sm",
              lastResult.success
                ? "bg-green-500/10 text-green-700 dark:text-green-400"
                : "bg-red-500/10 text-red-700 dark:text-red-400",
            )}
          >
            {lastResult.success ? (
              <CheckCircle className="h-4 w-4" />
            ) : (
              <XCircle className="h-4 w-4" />
            )}
            {lastResult.message}
          </div>
        )}

        {/* Config Warning */}
        {!configLoaded && (
          <div className="flex items-center gap-2 px-4 py-2 text-sm bg-yellow-500/10 text-yellow-700 dark:text-yellow-400">
            <AlertCircle className="h-4 w-4" />
            Load a configuration to access images and states for targeting
          </div>
        )}

        {/* Steps Area */}
        <div className="flex-1 overflow-y-auto">
          <div className="p-4 space-y-4">
            {steps.length === 0 ? (
              <div className="border border-border rounded-lg bg-card">
                <div className="flex flex-col items-center justify-center py-12">
                  <div className="text-muted-foreground text-center mb-4">
                    <p className="text-lg font-medium">No steps yet</p>
                    <p className="text-sm">Add steps to build your GUI workflow</p>
                  </div>
                  <AddStepDropdown onAddStep={addStep} />
                </div>
              </div>
            ) : (
              <>
                <div className="space-y-2">
                  {steps.map((step, index) => (
                    <StepItem
                      key={step.id}
                      step={step}
                      index={index}
                      isEditing={editingStepId === step.id}
                      isFirst={index === 0}
                      isLast={index === steps.length - 1}
                      onEdit={() => setEditingStepId(step.id)}
                      onRemove={() => removeStep(step.id)}
                      onMoveUp={() => moveStepUp(index)}
                      onMoveDown={() => moveStepDown(index)}
                    />
                  ))}
                </div>
                <AddStepDropdown onAddStep={addStep} disabled={isRunning} />
              </>
            )}
          </div>
        </div>
      </div>

      {/* Workflows Panel */}
      {showWorkflowsPanel && <WorkflowsPanel className="w-80" />}

      {/* Step Editor Dialog */}
      <StepEditor
        step={editingStep}
        open={editingStepId !== null}
        onClose={() => setEditingStepId(null)}
      />

      {/* Save Dialog */}
      <SaveWorkflowDialog />
    </div>
  );
}

export function GuiWorkflowBuilderTab({ editWorkflowId }: GuiWorkflowBuilderTabProps) {
  const state = useGuiBuilderState({ editWorkflowId });

  return (
    <GuiBuilderProvider value={state}>
      <GuiBuilderContent />
    </GuiBuilderProvider>
  );
}

export default GuiWorkflowBuilderTab;
