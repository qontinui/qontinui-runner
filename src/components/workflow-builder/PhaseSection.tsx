/**
 * PhaseSection.tsx
 *
 * Collapsible section for each workflow phase (Setup, Verification, Agentic).
 * Shows phase header with expand/collapse, step list, and "+ Add Step" button.
 */

import React from "react";
import { ChevronDown, ChevronRight, Plus } from "lucide-react";
import type { WorkflowPhase, UnifiedStep } from "../../types";
import { PHASE_INFO } from "../../types";
import { useWorkflowBuilder } from "./WorkflowBuilderContext";

interface PhaseSectionProps {
  phase: WorkflowPhase;
  steps: UnifiedStep[];
  onAddStep: (phase: WorkflowPhase) => void;
  renderStep: (step: UnifiedStep, index: number) => React.ReactNode;
}

export function PhaseSection({ phase, steps, onAddStep, renderStep }: PhaseSectionProps) {
  const { state, togglePhase, selectStep } = useWorkflowBuilder();
  const phaseInfo = PHASE_INFO[phase];
  const isExpanded = state.expandedPhases[phase];
  const isEmpty = steps.length === 0;
  const isSelected = state.selectedStepId
    ? steps.some((s) => s.id === state.selectedStepId)
    : false;

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
      <button
        onClick={() => togglePhase(phase)}
        className={`
          w-full flex items-center justify-between p-3 rounded-t-lg
          ${colors.bgHeader}
          transition-colors cursor-pointer
        `}
      >
        <div className="flex items-center gap-2">
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
        </div>
        <span className={`text-xs ${colors.textMuted} hidden sm:block`}>
          {phaseInfo.description}
        </span>
      </button>

      {/* Phase Content */}
      {isExpanded && (
        <div className="p-3 space-y-2">
          {/* Steps List */}
          {steps.length > 0 ? (
            <div className="space-y-2">
              {steps.map((step, index) => (
                <div
                  key={step.id}
                  onClick={() => selectStep(step.id)}
                  className={`
                    cursor-pointer transition-colors rounded-md
                    ${state.selectedStepId === step.id ? "ring-2 ring-white/20" : ""}
                  `}
                >
                  {renderStep(step, index)}
                </div>
              ))}
            </div>
          ) : (
            <div className={`text-center py-4 ${colors.textMuted} text-sm`}>
              No {phase} steps yet
            </div>
          )}

          {/* Add Step Button for this phase */}
          <button
            onClick={() => onAddStep(phase)}
            className={`
              w-full flex items-center justify-center gap-2 p-2 rounded-md
              ${colors.button}
              transition-colors text-sm
            `}
          >
            <Plus className="w-4 h-4" />
            <span>Add {phaseInfo.label} Step</span>
          </button>
        </div>
      )}
    </div>
  );
}
