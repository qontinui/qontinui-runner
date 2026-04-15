/**
 * PhaseSection.tsx
 *
 * Thin wrapper around PhaseSectionConcrete.
 * Connects to WorkflowBuilderContext for expand/collapse state and batch delete.
 */

import React from "react";
import type { UnifiedStep, WorkflowPhase } from "../../types";
import { useWorkflowBuilder } from "./WorkflowBuilderContext";
import { PhaseSectionConcrete } from "@qontinui/workflow-ui/components";

// =============================================================================
// Props
// =============================================================================

interface PhaseSectionProps {
  phase: WorkflowPhase;
  steps: UnifiedStep[];
  onAddStep: (phase: WorkflowPhase) => void;
  /** Optional actions rendered in the phase header (e.g. a "Live" button) */
  headerActions?: React.ReactNode;
  renderStep: (
    step: UnifiedStep,
    index: number,
    isSelectionMode: boolean,
    isSelectedForDelete: boolean,
    onToggleSelect: () => void,
  ) => React.ReactNode;
}

// =============================================================================
// Component — Wrapper around PhaseSectionConcrete
// =============================================================================

export function PhaseSection({
  phase,
  steps,
  onAddStep,
  headerActions,
  renderStep,
}: PhaseSectionProps) {
  const { state, togglePhase, removeStep } = useWorkflowBuilder();
  const isExpanded = state.expandedPhases[phase];

  const hasSelectedStep = state.selectedStepId
    ? steps.some((s) => s.id === state.selectedStepId)
    : false;

  return (
    <PhaseSectionConcrete
      phase={phase}
      steps={steps}
      isExpanded={isExpanded}
      onToggle={() => togglePhase(phase)}
      onAddStep={onAddStep}
      hasSelectedStep={hasSelectedStep}
      headerActions={headerActions}
      onBatchDelete={(ids) => ids.forEach((id) => removeStep(id, phase))}
      renderStepList={(stepsToRender, isSelectionMode, selectedIds, onToggleSelect) => (
        <div className="space-y-2">
          {/* workflow-ui's wire `UnifiedStep` includes an open `Other` variant;
              runner-produced steps are always canonical, so narrow here. */}
          {(stepsToRender as UnifiedStep[]).map((step, index) => (
            <div key={step.id}>
              {renderStep(step, index, isSelectionMode, selectedIds.has(step.id), () =>
                onToggleSelect(step.id),
              )}
            </div>
          ))}
        </div>
      )}
    />
  );
}
