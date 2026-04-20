/**
 * WorkflowPreview
 *
 * Shows a preview of the generated UnifiedWorkflow structure
 * with phase breakdown (Setup, Verification, Agentic, Completion).
 */

import { Play, Check, Bot, Flag, ChevronDown, ChevronRight } from "lucide-react";
import { useState } from "react";
import type { UnifiedWorkflow, UnifiedStep, WorkflowPhase } from "../../types/unified-workflow";

interface WorkflowPreviewProps {
  workflow: UnifiedWorkflow;
  onApply?: () => void;
}

const PHASE_ICONS = {
  setup: <Play className="w-3 h-3 text-blue-400" />,
  verification: <Check className="w-3 h-3 text-emerald-400" />,
  agentic: <Bot className="w-3 h-3 text-purple-400" />,
  completion: <Flag className="w-3 h-3 text-yellow-400" />,
};

const PHASE_COLORS = {
  setup: "border-blue-500/30",
  verification: "border-emerald-500/30",
  agentic: "border-purple-500/30",
  completion: "border-yellow-500/30",
};

export function WorkflowPreview({ workflow, onApply }: WorkflowPreviewProps) {
  const [expandedPhases, setExpandedPhases] = useState<Set<string>>(
    new Set(["setup", "verification", "agentic", "completion"]),
  );

  // Wire-side UnifiedStep[] has an open `Other` variant; runner-produced
  // workflows only contain canonical steps, so narrow to the strict view.
  const phases: { key: WorkflowPhase; steps: UnifiedStep[] }[] = [
    { key: "setup", steps: workflow.setup_steps as UnifiedStep[] },
    { key: "verification", steps: workflow.verification_steps as UnifiedStep[] },
    { key: "agentic", steps: workflow.agentic_steps as UnifiedStep[] },
    { key: "completion", steps: workflow.completion_steps as UnifiedStep[] },
  ];

  const totalSteps = phases.reduce((sum, p) => sum + p.steps.length, 0);
  const activePhases = phases.filter((p) => p.steps.length > 0).length;

  const togglePhase = (phase: string) => {
    setExpandedPhases((prev) => {
      const next = new Set(prev);
      if (next.has(phase)) next.delete(phase);
      else next.add(phase);
      return next;
    });
  };

  return (
    <div className="p-4 space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h3 className="text-sm font-medium text-foreground">Workflow Preview: {workflow.name}</h3>
          <p className="text-xs text-muted-foreground mt-0.5">{workflow.description}</p>
        </div>
        {onApply && (
          <button
            onClick={onApply}
            className="flex items-center gap-2 px-3 py-1.5 text-sm font-medium bg-emerald-600 text-white rounded-md hover:bg-emerald-700 transition-colors"
          >
            <Play className="w-4 h-4" />
            Save Workflow
          </button>
        )}
      </div>

      {/* Settings summary */}
      <div className="flex gap-4 text-xs text-muted-foreground">
        {workflow.maxIterations && <span>Max iterations: {workflow.maxIterations}</span>}
        {workflow.tags.length > 0 && <span>Tags: {workflow.tags.join(", ")}</span>}
      </div>

      <div className="space-y-2">
        {phases.map(({ key, steps }) => {
          const isExpanded = expandedPhases.has(key);
          const icon = PHASE_ICONS[key];
          const borderColor = PHASE_COLORS[key];

          return (
            <div key={key} className={`border-l-2 ${borderColor} pl-3`}>
              <button
                onClick={() => togglePhase(key)}
                className="flex items-center gap-2 py-1 w-full text-left"
              >
                {isExpanded ? (
                  <ChevronDown className="w-3 h-3 text-muted-foreground" />
                ) : (
                  <ChevronRight className="w-3 h-3 text-muted-foreground" />
                )}
                {icon}
                <span className="text-xs font-medium text-foreground capitalize">{key}</span>
                <span className="text-xs text-muted-foreground">({steps.length} steps)</span>
              </button>

              {isExpanded && steps.length > 0 && (
                <div className="ml-5 space-y-1 pb-2">
                  {steps.map((step, i) => (
                    <div
                      key={step.id}
                      className="flex items-center gap-2 px-2 py-1.5 bg-muted/30 rounded text-xs"
                    >
                      <span className="text-muted-foreground">{i + 1}.</span>
                      <span className="text-foreground">{step.name}</span>
                      <span className="text-muted-foreground ml-auto">{step.type}</span>
                    </div>
                  ))}
                </div>
              )}

              {isExpanded && steps.length === 0 && (
                <p className="ml-5 py-1 text-xs text-muted-foreground italic">No steps</p>
              )}
            </div>
          );
        })}
      </div>

      {/* Summary */}
      <div className="pt-3 border-t border-border text-xs text-muted-foreground">
        Total: {totalSteps} steps across {activePhases} phases
      </div>
    </div>
  );
}
