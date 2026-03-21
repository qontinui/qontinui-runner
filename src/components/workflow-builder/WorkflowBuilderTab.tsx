import React, { useState, useCallback, useEffect, useRef } from "react";
import {
  Loader2,
  Terminal,
  Puzzle,
  Layers,
  Zap,
  Bot,
  TestTube2,
  Monitor,
  Rocket,
  Globe,
  ShieldCheck,
  CheckCircle2,
  Activity,
  ScanSearch,
  HeartPulse,
  Sparkles,
} from "lucide-react";
import type { WorkflowPhase, UnifiedStep, UnifiedWorkflow } from "../../types";
import { WorkflowBuilderProvider, useWorkflowBuilder } from "./WorkflowBuilderContext";
import { PhaseSection } from "./PhaseSection";
import { StepItem } from "./StepItem";
import { AddStepDropdown, AddStepButton } from "./AddStepDropdown";
import { StepConfigPanel } from "./StepConfigPanel";
import { PromptLibraryPicker } from "./PromptLibraryPicker";
import { ShellCommandLibraryPicker } from "./ShellCommandLibraryPicker";
import { CheckLibraryPicker } from "./CheckLibraryPicker";
import { CheckGroupLibraryPicker } from "./CheckGroupLibraryPicker";
import { TestLibraryPicker } from "./TestLibraryPicker";
import { McpServerToolPicker } from "./McpServerToolPicker";
import { WorkflowLibraryPicker } from "./WorkflowLibraryPicker";
import { AiGenerateWorkflowModal } from "./AiGenerateWorkflowModal";
import { AiGeneratePanel } from "./AiGeneratePanel";
import { StageSelector } from "./StageSelector";
import { ConstraintsPanel } from "./constraints";
import { BatchDeleteDialog, ConfirmDialog } from "../ui";
import { registerUserSkills } from "@qontinui/workflow-utils";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { CompositionBuilderDialog } from "./CompositionBuilderDialog";
import { SkillExportDialog } from "./SkillExportDialog";
import { SkillImportDialog } from "./SkillImportDialog";
import { RunOptionsDialog } from "./RunOptionsDialog";
import { createLogger } from "@/lib/logger";
import { SettingsPanel } from "./SettingsPanel";
import { WorkflowLibraryPanel } from "./WorkflowLibraryPanel";
import { WorkflowBuilderToolbar } from "./WorkflowBuilderToolbar";
import { useLibraryPickers } from "./useLibraryPickers";
import { useWorkflowExecution } from "./useWorkflowExecution";
import { useWorkflowLibrary } from "./useWorkflowLibrary";

const log = createLogger("WorkflowBuilder");

const SKILL_ICON_MAP: Record<string, React.ComponentType<{ className?: string }>> = {
  terminal: Terminal,
  puzzle: Puzzle,
  layers: Layers,
  zap: Zap,
  bot: Bot,
  "test-tube-2": TestTube2,
  monitor: Monitor,
  rocket: Rocket,
  globe: Globe,
  "shield-check": ShieldCheck,
  "check-circle-2": CheckCircle2,
  activity: Activity,
  "scan-search": ScanSearch,
  "heart-pulse": HeartPulse,
  sparkles: Sparkles,
};
function resolveSkillIcon(iconId: string): React.ComponentType<{ className?: string }> {
  return SKILL_ICON_MAP[iconId] || Puzzle;
}

function WorkflowBuilderContent({
  onOpenLibrary: _onOpenLibrary,
  onNavigateToActive,
}: {
  onOpenLibrary?: () => void;
  onNavigateToActive?: () => void;
}) {
  const {
    state,
    isEmpty,
    hasUnsavedChanges,
    addStep,
    removeStep,
    moveStep,
    selectStep,
    getSelectedStep,
    updateStep,
    showAddDropdown: _showAddDropdown,
    resetToNew,
    setWorkflow,
    saveWorkflow,
    loadWorkflow,
    updateWorkflow,
    getActiveSteps,
  } = useWorkflowBuilder();

  const selectedStep = getSelectedStep();

  const [showSettings, setShowSettings] = useState(false);
  const [showConstraints, setShowConstraints] = useState(false);
  const nameInputRef = useRef<HTMLInputElement | null>(null);

  const {
    pickerState,
    pickerDispatch,
    handleAddStep,
    handleStepAdded,
    handlePromptSelected,
    handleShellCommandSelected,
    handleCheckSelected,
    handleTestSelected,
    handleCheckGroupSelected,
    handleMcpToolSelected,
    handleOpenWorkflowPicker,
    handleWorkflowSelected,
  } = useLibraryPickers({ addStep, updateStep, getSelectedStep });

  const {
    execState,
    execDispatch,
    handleRun,
    handleStop,
    handleSaveAndRun,
    handleSaveAndRunSpecWorkflow,
  } = useWorkflowExecution({
    workflowId: state.workflow.id,
    workflowName: state.workflow.name,
    isEmpty,
    hasUnsavedChanges,
    saveWorkflow,
    setWorkflow,
    onNavigateToActive,
  });

  const wfLib = useWorkflowLibrary(state.workflow.id, state.isSaving, resetToNew, loadWorkflow);

  const handleNewWorkflow = useCallback(() => {
    resetToNew();
  }, [resetToNew]);

  const handleSave = useCallback(async () => {
    const success = await saveWorkflow();
    if (success) {
      log.debug("Workflow saved successfully");
    }
  }, [saveWorkflow]);

  const handleLoadSpecWorkflow = useCallback(
    (workflow: UnifiedWorkflow) => {
      setWorkflow(workflow);
      log.debug("Loaded spec workflow into builder:", workflow.name);
    },
    [setWorkflow],
  );

  const handleAiWorkflowGenerated = useCallback(
    (workflow: UnifiedWorkflow) => {
      resetToNew();
      updateWorkflow({
        name: workflow.name,
        description: workflow.description,
        category: workflow.category,
        tags: workflow.tags,
        setup_steps: workflow.setup_steps,
        verification_steps: workflow.verification_steps,
        agentic_steps: workflow.agentic_steps,
        completion_steps: workflow.completion_steps,
        max_iterations: workflow.max_iterations,
        provider: workflow.provider ?? undefined,
        model: workflow.model ?? undefined,
        skip_ai_summary: workflow.skip_ai_summary,
        log_source_selection: workflow.log_source_selection,
      });
      setShowSettings(true);
      setShowConstraints(false);
      log.debug("Loaded AI-generated workflow:", workflow.name);
    },
    [resetToNew, updateWorkflow],
  );

  const renderStep = useCallback(
    (
      step: UnifiedStep,
      index: number,
      phase: WorkflowPhase,
      steps: UnifiedStep[],
      isSelectionMode: boolean = false,
      isSelectedForDelete: boolean = false,
      onToggleSelect?: () => void,
    ) => (
      <StepItem
        key={step.id}
        step={step}
        phase={phase}
        index={index}
        isFirst={index === 0}
        isLast={index === steps.length - 1}
        isSelected={state.selectedStepId === step.id}
        onMoveUp={() => moveStep(step.id, phase, "up")}
        onMoveDown={() => moveStep(step.id, phase, "down")}
        onDelete={() => removeStep(step.id, phase)}
        onClick={() => selectStep(step.id)}
        isSelectionMode={isSelectionMode}
        isSelectedForDelete={isSelectedForDelete}
        onToggleSelect={onToggleSelect}
      />
    ),
    [state.selectedStepId, moveStep, removeStep, selectStep],
  );

  const _handleForkSkill = useCallback(async (skillId: string, newName?: string) => {
    try {
      const response = await tracedFetch(`${getApiBase()}/skills/${skillId}/fork`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ new_name: newName || undefined }),
      });
      if (!response.ok) {
        const err = await response.json();
        console.error("Fork failed:", err);
        return;
      }
      const allResponse = await tracedFetch(`${getApiBase()}/skills`);
      if (allResponse.ok) {
        const allSkills = await allResponse.json();
        const nonBuiltin = allSkills.filter((s: { source: string }) => s.source !== "builtin");
        registerUserSkills(nonBuiltin);
      }
    } catch (err) {
      console.error("Fork error:", err);
    }
  }, []);

  const _handleApproveSkill = useCallback(async (skillId: string, status: string) => {
    try {
      const response = await tracedFetch(`${getApiBase()}/skills/${skillId}/approve`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ status }),
      });
      if (!response.ok) {
        const err = await response.json();
        console.error("Approval failed:", err);
        return;
      }
      const allResponse = await tracedFetch(`${getApiBase()}/skills`);
      if (allResponse.ok) {
        const allSkills = await allResponse.json();
        const nonBuiltin = allSkills.filter((s: { source: string }) => s.source !== "builtin");
        registerUserSkills(nonBuiltin);
      }
    } catch (err) {
      console.error("Approval error:", err);
    }
  }, []);

  return (
    <div className="h-full flex">
      {wfLib.showLibrary && (
        <WorkflowLibraryPanel
          filteredWorkflows={wfLib.filteredWorkflows}
          workflowsLoading={wfLib.workflowsLoading}
          currentWorkflowId={state.workflow.id}
          searchQuery={wfLib.searchQuery}
          onSearchQueryChange={wfLib.setSearchQuery}
          showFavoritesOnly={wfLib.showFavoritesOnly}
          onToggleFavoritesOnly={() => wfLib.setShowFavoritesOnly((prev) => !prev)}
          hasFavorites={wfLib.hasFavorites}
          isWorkflowSelectionMode={wfLib.isWorkflowSelectionMode}
          onToggleSelectionMode={() =>
            wfLib.setIsWorkflowSelectionMode(!wfLib.isWorkflowSelectionMode)
          }
          selectedWorkflowIds={wfLib.selectedWorkflowIds}
          onToggleWorkflowSelection={wfLib.toggleWorkflowSelection}
          onExitSelectionMode={wfLib.exitWorkflowSelectionMode}
          onShowBatchDeleteDialog={() => wfLib.setShowBatchDeleteDialog(true)}
          onNewWorkflow={handleNewWorkflow}
          onSelectWorkflow={wfLib.selectWorkflow}
          onToggleFavorite={wfLib.toggleFavorite}
          onImportWorkflow={() => wfLib.workflowFileInputRef.current?.click()}
          workflowImportError={wfLib.workflowImportError}
          onDismissImportError={() => wfLib.setWorkflowImportError(null)}
          workflowFileInputRef={wfLib.workflowFileInputRef}
          onImportFileChange={wfLib.handleImportWorkflow}
        />
      )}

      <div className="flex-1 flex flex-col bg-zinc-900">
        <WorkflowBuilderToolbar
          workflowName={state.workflow.name}
          hasUnsavedChanges={hasUnsavedChanges}
          isSaving={state.isSaving}
          isExecuting={execState.isExecuting}
          isEmpty={isEmpty}
          showLibrary={wfLib.showLibrary}
          showSettings={showSettings}
          showConstraints={showConstraints}
          workflowId={state.workflow.id}
          isExportingWorkflow={wfLib.isExportingWorkflow}
          isExportingSkills={wfLib.isExportingSkills}
          isImportingSkills={wfLib.isImportingSkills}
          originalWorkflow={state.originalWorkflow}
          onToggleLibrary={wfLib.toggleLibrary}
          onToggleSettings={() => {
            setShowSettings(!showSettings);
            if (!showSettings) setShowConstraints(false);
          }}
          onToggleConstraints={() => {
            setShowConstraints(!showConstraints);
            if (!showConstraints) setShowSettings(false);
          }}
          onNameChange={(name) => updateWorkflow({ name })}
          onSave={handleSave}
          onRun={() => handleRun()}
          onShowRunOptions={() => execDispatch({ type: "SHOW_RUN_OPTIONS", show: true })}
          onStop={handleStop}
          onExportWorkflow={wfLib.handleExportWorkflow}
          onOpenExportDialog={wfLib.handleOpenExportDialog}
          onImportSkillsClick={() => wfLib.skillFileInputRef.current?.click()}
          onShowCompositionBuilder={() => wfLib.setShowCompositionBuilder(true)}
          skillFileInputRef={wfLib.skillFileInputRef}
          onImportFileSelected={wfLib.handleImportFileSelected}
        />

        <CompositionBuilderDialog
          isOpen={wfLib.showCompositionBuilder}
          onClose={() => wfLib.setShowCompositionBuilder(false)}
          resolveIcon={resolveSkillIcon}
        />
        <SkillExportDialog
          isOpen={wfLib.showSkillExportDialog}
          onClose={() => wfLib.setShowSkillExportDialog(false)}
          availableSkills={wfLib.availableSkillsForExport}
          selectedIds={wfLib.selectedSkillIdsForExport}
          onToggleId={(id) => {
            wfLib.setSelectedSkillIdsForExport((prev) => {
              const next = new Set(prev);
              if (next.has(id)) next.delete(id);
              else next.add(id);
              return next;
            });
          }}
          onSelectAll={(ids) => wfLib.setSelectedSkillIdsForExport(ids)}
          onExport={wfLib.handleExportSkills}
        />
        <SkillImportDialog
          isOpen={wfLib.showSkillImportDialog && !!wfLib.pendingImportSkills}
          onClose={() => {
            wfLib.setShowSkillImportDialog(false);
            wfLib.setPendingImportSkills(null);
          }}
          skillCount={
            wfLib.pendingImportSkills ? (wfLib.pendingImportSkills as unknown[]).length : 0
          }
          conflictMode={wfLib.importConflictMode}
          onSetConflictMode={wfLib.setImportConflictMode}
          onConfirm={wfLib.handleConfirmImport}
        />

        {execState.isExecuting && (
          <div className="px-4 py-2 bg-blue-500/20 border-b border-blue-500/30 text-blue-400 text-sm flex items-center gap-2">
            <Loader2 className="w-4 h-4 animate-spin" />
            Running workflow: {state.workflow.name}...
          </div>
        )}
        {execState.executionSuccess && !execState.isExecuting && (
          <div className="px-4 py-2 bg-green-500/20 border-b border-green-500/30 text-green-400 text-sm">
            {execState.executionSuccess}
          </div>
        )}
        {(state.error || execState.executionError) && !execState.isExecuting && (
          <div className="px-4 py-2 bg-red-500/20 border-b border-red-500/30 text-red-400 text-sm flex items-center justify-between gap-2">
            <span
              data-content-role="status"
              data-content-label="execution error"
              className="flex-1"
            >
              {state.error || execState.executionError}
            </span>
            <button
              onClick={() => execDispatch({ type: "SET_ERROR", error: null })}
              className="text-red-400 hover:text-red-300 p-1"
              title="Dismiss"
            >
              &#10005;
            </button>
          </div>
        )}

        {showSettings && <SettingsPanel nameInputRef={nameInputRef} />}

        {showConstraints && <ConstraintsPanel onClose={() => setShowConstraints(false)} />}

        <div className="flex-1 flex overflow-hidden">
          {isEmpty ? (
            <div className="flex-1 overflow-hidden">
              <AiGeneratePanel
                onCreateManually={() => handleAddStep("setup")}
                onNavigateToActiveRuns={() => onNavigateToActive?.()}
                onLoadSpecWorkflow={handleLoadSpecWorkflow}
                onSaveAndRunSpecWorkflow={handleSaveAndRunSpecWorkflow}
              />
            </div>
          ) : (
            <div
              className={`flex-1 overflow-y-auto p-4 ${selectedStep ? "border-r border-zinc-700" : ""}`}
            >
              <StageSelector />
              <div className="space-y-4 max-w-3xl mx-auto">
                <PhaseSection
                  phase="setup"
                  steps={getActiveSteps("setup")}
                  onAddStep={handleAddStep}
                  renderStep={(step, index, isSelectionMode, isSelectedForDelete, onToggleSelect) =>
                    renderStep(
                      step,
                      index,
                      "setup",
                      getActiveSteps("setup"),
                      isSelectionMode,
                      isSelectedForDelete,
                      onToggleSelect,
                    )
                  }
                />

                <PhaseSection
                  phase="verification"
                  steps={getActiveSteps("verification")}
                  onAddStep={handleAddStep}
                  headerActions={null}
                  renderStep={(step, index, isSelectionMode, isSelectedForDelete, onToggleSelect) =>
                    renderStep(
                      step,
                      index,
                      "verification",
                      getActiveSteps("verification"),
                      isSelectionMode,
                      isSelectedForDelete,
                      onToggleSelect,
                    )
                  }
                />

                <PhaseSection
                  phase="agentic"
                  steps={getActiveSteps("agentic")}
                  onAddStep={handleAddStep}
                  renderStep={(step, index, isSelectionMode, isSelectedForDelete, onToggleSelect) =>
                    renderStep(
                      step,
                      index,
                      "agentic",
                      getActiveSteps("agentic"),
                      isSelectionMode,
                      isSelectedForDelete,
                      onToggleSelect,
                    )
                  }
                />

                <PhaseSection
                  phase="completion"
                  steps={getActiveSteps("completion")}
                  onAddStep={handleAddStep}
                  renderStep={(step, index, isSelectionMode, isSelectedForDelete, onToggleSelect) =>
                    renderStep(
                      step,
                      index,
                      "completion",
                      getActiveSteps("completion"),
                      isSelectionMode,
                      isSelectedForDelete,
                      onToggleSelect,
                    )
                  }
                />

                <div className="relative flex justify-center pt-4">
                  <AddStepButton onClick={() => handleAddStep("setup")} />
                </div>
              </div>

              <PromptLibraryPicker
                isOpen={pickerState.promptPickerOpen}
                onClose={() => pickerDispatch({ type: "CLOSE_PROMPT_PICKER" })}
                onSelect={handlePromptSelected}
                phase={pickerState.promptPickerPhase}
              />

              <ShellCommandLibraryPicker
                isOpen={pickerState.shellCommandPickerOpen}
                onClose={() => pickerDispatch({ type: "CLOSE_SHELL_COMMAND_PICKER" })}
                onSelect={handleShellCommandSelected}
                phase={pickerState.shellCommandPickerPhase}
              />

              <CheckLibraryPicker
                isOpen={pickerState.checkPickerOpen}
                onClose={() => pickerDispatch({ type: "CLOSE_CHECK_PICKER" })}
                onSelect={handleCheckSelected}
                phase={pickerState.checkPickerPhase}
              />

              <TestLibraryPicker
                isOpen={pickerState.testPickerOpen}
                onClose={() => pickerDispatch({ type: "CLOSE_TEST_PICKER" })}
                onSelect={handleTestSelected}
                phase={pickerState.testPickerPhase}
              />

              <CheckGroupLibraryPicker
                isOpen={pickerState.checkGroupPickerOpen}
                onClose={() => pickerDispatch({ type: "CLOSE_CHECK_GROUP_PICKER" })}
                onSelect={handleCheckGroupSelected}
                phase={pickerState.checkGroupPickerPhase}
              />

              <McpServerToolPicker
                isOpen={pickerState.mcpPickerOpen}
                onClose={() => pickerDispatch({ type: "CLOSE_MCP_PICKER" })}
                onSelect={handleMcpToolSelected}
                phase={pickerState.mcpPickerPhase}
              />

              <WorkflowLibraryPicker
                isOpen={pickerState.workflowPickerOpen}
                onClose={() => pickerDispatch({ type: "CLOSE_WORKFLOW_PICKER" })}
                onSelect={handleWorkflowSelected}
                phase={pickerState.workflowPickerPhase}
                excludeWorkflowId={state.workflow.id}
              />

              <AiGenerateWorkflowModal
                isOpen={pickerState.aiGenerateModalOpen}
                onClose={() => pickerDispatch({ type: "CLOSE_AI_GENERATE_MODAL" })}
                onWorkflowGenerated={handleAiWorkflowGenerated}
              />

              <BatchDeleteDialog
                open={wfLib.showBatchDeleteDialog}
                title="Delete Workflows"
                itemType="workflow"
                itemNames={wfLib.getSelectedWorkflowNames()}
                isDeleting={wfLib.isDeletingWorkflows}
                onClose={() => wfLib.setShowBatchDeleteDialog(false)}
                onConfirm={wfLib.deleteSelectedWorkflows}
              />
            </div>
          )}

          {pickerState.dropdownOpen && (
            <div
              className="fixed inset-0 z-40"
              role="button"
              tabIndex={0}
              aria-label="Close add step dropdown"
              onClick={() => pickerDispatch({ type: "CLOSE_DROPDOWN" })}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") pickerDispatch({ type: "CLOSE_DROPDOWN" });
              }}
            >
              <div
                role="presentation"
                className="absolute top-1/2 left-1/2 transform -translate-x-1/2 -translate-y-1/2"
                onClick={(e) => e.stopPropagation()}
              >
                <AddStepDropdown
                  filterPhase={pickerState.dropdownPhase ?? undefined}
                  onAddStep={handleStepAdded}
                  isOpen={pickerState.dropdownOpen}
                  onClose={() => pickerDispatch({ type: "CLOSE_DROPDOWN" })}
                  onSkillUsed={async (skillId) => {
                    try {
                      await tracedFetch(`${getApiBase()}/skills/${skillId}/increment-usage`, {
                        method: "POST",
                      });
                    } catch {
                      // Non-critical, silently ignore
                    }
                  }}
                />
              </div>
            </div>
          )}

          {selectedStep && (
            <div className="w-96 shrink-0">
              <StepConfigPanel
                onClose={() => selectStep(null)}
                onOpenWorkflowPicker={() => {
                  const step = getSelectedStep();
                  if (step && "phase" in step) {
                    handleOpenWorkflowPicker(step.phase as WorkflowPhase);
                  }
                }}
              />
            </div>
          )}
        </div>

        <RunOptionsDialog
          open={execState.showRunOptions}
          onClose={() => execDispatch({ type: "SHOW_RUN_OPTIONS", show: false })}
          onRun={(overrides) => {
            execDispatch({ type: "SHOW_RUN_OPTIONS", show: false });
            handleRun(overrides);
          }}
          isLoading={execState.isExecuting}
          workflowId={state.workflow.id || null}
        />

        <ConfirmDialog
          open={execState.showSaveBeforeRunDialog}
          title="Save Required"
          message="To run a workflow, you need to save it first."
          description="Would you like to save the workflow now and then run it?"
          variant="info"
          confirmText="Save & Run"
          cancelText="Cancel"
          isLoading={execState.isSavingBeforeRun}
          onClose={() => execDispatch({ type: "SHOW_SAVE_DIALOG", show: false })}
          onConfirm={handleSaveAndRun}
        />
      </div>
    </div>
  );
}

function WorkflowBuilderInner({
  editWorkflowId,
  onOpenLibrary,
  onNavigateToActive,
}: {
  editWorkflowId?: string | null;
  onOpenLibrary?: () => void;
  onNavigateToActive?: () => void;
}) {
  const { loadWorkflow, state } = useWorkflowBuilder();

  useEffect(() => {
    if (editWorkflowId && !state.isLoading) {
      loadWorkflow(editWorkflowId);
    }
  }, [editWorkflowId, loadWorkflow]); // eslint-disable-line react-hooks/exhaustive-deps

  if (state.isLoading) {
    return (
      <div className="flex items-center justify-center h-full bg-zinc-900">
        <div className="flex flex-col items-center gap-3">
          <Loader2 className="w-8 h-8 animate-spin text-zinc-400" />
          <span className="text-zinc-400">Loading workflow...</span>
        </div>
      </div>
    );
  }

  return (
    <WorkflowBuilderContent onOpenLibrary={onOpenLibrary} onNavigateToActive={onNavigateToActive} />
  );
}

interface WorkflowBuilderTabProps {
  editWorkflowId?: string | null;
  onOpenLibrary?: () => void;
  onNavigateToActive?: () => void;
}

export function WorkflowBuilderTab({
  editWorkflowId,
  onOpenLibrary,
  onNavigateToActive,
}: WorkflowBuilderTabProps) {
  return (
    <div data-tutorial-id="workflow-builder-nav" className="h-full">
      <WorkflowBuilderProvider startEmpty={!editWorkflowId}>
        <WorkflowBuilderInner
          editWorkflowId={editWorkflowId}
          onOpenLibrary={onOpenLibrary}
          onNavigateToActive={onNavigateToActive}
        />
      </WorkflowBuilderProvider>
    </div>
  );
}

export default WorkflowBuilderTab;
