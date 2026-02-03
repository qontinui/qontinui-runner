/**
 * PhaseSection.tsx
 *
 * Collapsible section for each workflow phase (Setup, Verification, Agentic).
 * Shows phase header with expand/collapse, step list, and "+ Add Step" button.
 */

import React, { useState, useCallback } from "react";
import { ChevronDown, ChevronRight, Plus, Trash2 } from "lucide-react";
import type { WorkflowPhase, UnifiedStep } from "../../types";
import { PHASE_INFO } from "../../types";
import { useWorkflowBuilder } from "./WorkflowBuilderContext";
import { BatchDeleteDialog } from "../ui/BatchDeleteDialog";

interface PhaseSectionProps {
  phase: WorkflowPhase;
  steps: UnifiedStep[];
  onAddStep: (phase: WorkflowPhase) => void;
  renderStep: (
    step: UnifiedStep,
    index: number,
    isSelectionMode: boolean,
    isSelectedForDelete: boolean,
    onToggleSelect: () => void,
  ) => React.ReactNode;
}

export function PhaseSection({ phase, steps, onAddStep, renderStep }: PhaseSectionProps) {
  const { state, togglePhase, selectStep, removeStep } = useWorkflowBuilder();
  const phaseInfo = PHASE_INFO[phase];
  const isExpanded = state.expandedPhases[phase];
  const isEmpty = steps.length === 0;
  const isSelected = state.selectedStepId
    ? steps.some((s) => s.id === state.selectedStepId)
    : false;

  // Step selection mode state (for batch delete)
  const [isStepSelectionMode, setIsStepSelectionMode] = useState(false);
  const [selectedStepIds, setSelectedStepIds] = useState<Set<string>>(new Set());
  const [showBatchDeleteDialog, setShowBatchDeleteDialog] = useState(false);

  // Toggle step selection
  const toggleStepSelection = useCallback((stepId: string) => {
    setSelectedStepIds((prev) => {
      const next = new Set(prev);
      if (next.has(stepId)) {
        next.delete(stepId);
      } else {
        next.add(stepId);
      }
      return next;
    });
  }, []);

  // Exit step selection mode
  const exitStepSelectionMode = useCallback(() => {
    setIsStepSelectionMode(false);
    setSelectedStepIds(new Set());
  }, []);

  // Delete selected steps
  const deleteSelectedSteps = useCallback(() => {
    // Delete in reverse order to avoid index issues
    const stepsToDelete = steps.filter((s) => selectedStepIds.has(s.id));
    stepsToDelete.forEach((step) => {
      removeStep(step.id, phase);
    });

    // Exit selection mode
    exitStepSelectionMode();
    setShowBatchDeleteDialog(false);
  }, [selectedStepIds, steps, removeStep, phase, exitStepSelectionMode]);

  // Get names of selected steps for the delete dialog
  const getSelectedStepNames = useCallback((): string[] => {
    return steps.filter((s) => selectedStepIds.has(s.id)).map((s) => s.name || `${s.type} step`);
  }, [steps, selectedStepIds]);

  // Color classes based on phase
  const colorClasses = {
    setup: {
      border: "border-blue-500/30",
      borderHover: "hover:border-blue-500/50",
      bg: "bg-blue-500/5",
      bgHeader: "bg-blue-500/10",
      text: "text-blue-400",
      textMuted: "text-blue-400/60",
      button: "bg-blue-500/10 hover:bg-blue-500/20 text-blue-400",
    },
    verification: {
      border: "border-green-500/30",
      borderHover: "hover:border-green-500/50",
      bg: "bg-green-500/5",
      bgHeader: "bg-green-500/10",
      text: "text-green-400",
      textMuted: "text-green-400/60",
      button: "bg-green-500/10 hover:bg-green-500/20 text-green-400",
    },
    agentic: {
      border: "border-amber-500/30",
      borderHover: "hover:border-amber-500/50",
      bg: "bg-amber-500/5",
      bgHeader: "bg-amber-500/10",
      text: "text-amber-400",
      textMuted: "text-amber-400/60",
      button: "bg-amber-500/10 hover:bg-amber-500/20 text-amber-400",
    },
    completion: {
      border: "border-purple-500/30",
      borderHover: "hover:border-purple-500/50",
      bg: "bg-purple-500/5",
      bgHeader: "bg-purple-500/10",
      text: "text-purple-400",
      textMuted: "text-purple-400/60",
      button: "bg-purple-500/10 hover:bg-purple-500/20 text-purple-400",
    },
  };

  const colors = colorClasses[phase];

  return (
    <div
      data-tutorial-id={`${phase}-phase`}
      data-phase={phase}
      className={`
        rounded-lg border transition-colors
        ${colors.border} ${colors.borderHover}
        ${isSelected ? colors.bg : ""}
      `}
    >
      {/* Phase Header */}
      <div
        className={`
          w-full flex items-center justify-between p-3 rounded-t-lg
          ${colors.bgHeader}
          transition-colors
        `}
      >
        <button
          onClick={() => togglePhase(phase)}
          className="flex items-center gap-2 flex-1 cursor-pointer"
          data-ui-id={`workflow-builder-phase-${phase}-toggle-btn`}
        >
          {isExpanded ? (
            <ChevronDown className={`w-4 h-4 ${colors.text}`} />
          ) : (
            <ChevronRight className={`w-4 h-4 ${colors.text}`} />
          )}
          <span className={`font-medium ${colors.text}`}>{phaseInfo.label}</span>
          {isEmpty && <span className={`text-xs ${colors.textMuted}`}>(empty)</span>}
          {!isEmpty && (
            <span className={`text-xs ${colors.textMuted}`}>
              ({steps.length} step{steps.length !== 1 ? "s" : ""})
            </span>
          )}
        </button>
        <div className="flex items-center gap-2">
          <span className={`text-xs ${colors.textMuted} hidden sm:block`}>
            {phaseInfo.description}
          </span>
          {/* Trashcan icon for step selection mode - only show if there are steps */}
          {!isEmpty && (
            <button
              onClick={(e) => {
                e.stopPropagation();
                setIsStepSelectionMode(!isStepSelectionMode);
                if (isStepSelectionMode) {
                  setSelectedStepIds(new Set());
                }
              }}
              className={`p-1 rounded transition-colors ${
                isStepSelectionMode
                  ? "bg-red-500/20 text-red-400"
                  : "text-zinc-400 hover:text-red-400 hover:bg-zinc-700"
              }`}
              title={isStepSelectionMode ? "Cancel selection" : "Select steps to delete"}
              data-ui-id={`workflow-builder-phase-${phase}-selection-mode-btn`}
            >
              <Trash2 className="w-4 h-4" />
            </button>
          )}
        </div>
      </div>

      {/* Phase Content */}
      {isExpanded && (
        <div className="p-3 space-y-2">
          {/* Selection Mode Header */}
          {isStepSelectionMode && (
            <div className="flex items-center justify-between px-3 py-2 bg-red-500/10 border border-red-500/30 rounded-md">
              <span className="text-sm text-red-400">{selectedStepIds.size} selected</span>
              <div className="flex items-center gap-2">
                {selectedStepIds.size > 0 && (
                  <button
                    onClick={() => setShowBatchDeleteDialog(true)}
                    className="flex items-center gap-1 px-3 py-1 text-sm font-medium bg-red-600 hover:bg-red-500 text-white rounded-md transition-colors"
                    data-ui-id={`workflow-builder-phase-${phase}-delete-selected-btn`}
                  >
                    <Trash2 className="w-3.5 h-3.5" />
                    Delete
                  </button>
                )}
                <button
                  onClick={exitStepSelectionMode}
                  className="px-3 py-1 text-sm text-zinc-400 hover:text-zinc-200 transition-colors"
                  data-ui-id={`workflow-builder-phase-${phase}-cancel-selection-btn`}
                >
                  Cancel
                </button>
              </div>
            </div>
          )}

          {/* Steps List */}
          {steps.length > 0 ? (
            <div className="space-y-2">
              {steps.map((step, index) => (
                <div
                  key={step.id}
                  onClick={() => {
                    if (isStepSelectionMode) {
                      toggleStepSelection(step.id);
                    } else {
                      selectStep(step.id);
                    }
                  }}
                  className={`
                    cursor-pointer transition-colors rounded-md
                    ${state.selectedStepId === step.id && !isStepSelectionMode ? "ring-2 ring-white/20" : ""}
                    ${isStepSelectionMode && selectedStepIds.has(step.id) ? "ring-2 ring-red-500/50" : ""}
                  `}
                >
                  {renderStep(step, index, isStepSelectionMode, selectedStepIds.has(step.id), () =>
                    toggleStepSelection(step.id),
                  )}
                </div>
              ))}
            </div>
          ) : (
            <div className={`text-center py-4 ${colors.textMuted} text-sm`}>
              No {phase} steps yet
            </div>
          )}

          {/* Add Step Button for this phase */}
          {!isStepSelectionMode && (
            <button
              onClick={() => onAddStep(phase)}
              className={`
                w-full flex items-center justify-center gap-2 p-2 rounded-md
                ${colors.button}
                transition-colors text-sm
              `}
              data-ui-id={`workflow-builder-phase-${phase}-add-step-btn`}
            >
              <Plus className="w-4 h-4" />
              <span>Add {phaseInfo.label} Step</span>
            </button>
          )}

          {/* Batch Delete Steps Dialog */}
          <BatchDeleteDialog
            open={showBatchDeleteDialog}
            title={`Delete ${phaseInfo.label} Steps`}
            itemType="step"
            itemNames={getSelectedStepNames()}
            onClose={() => setShowBatchDeleteDialog(false)}
            onConfirm={deleteSelectedSteps}
          />
        </div>
      )}
    </div>
  );
}
