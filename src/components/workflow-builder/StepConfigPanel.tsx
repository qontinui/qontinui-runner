import { X, AlertCircle } from "lucide-react";
import type { UnifiedStep, WorkflowPhase } from "../../types";
import type { BaseStep, CommandStep } from "../../types/unified-workflow";
import { useWorkflowBuilder } from "./WorkflowBuilderContext";
import {
  CommandConfig,
  PromptConfig,
  UiBridgeConfig,
  WorkflowConfig,
  DataFlowSection,
} from "./step-config";
import { WrapperActionStepConfig } from "./WrapperActionStepConfig";

function findStepPhase(
  stepId: string,
  // Wire-side step arrays (UnifiedStep) carry an open `Other` variant whose
  // `id` is typed `unknown`; widen the predicate shape here.
  workflow: {
    setupSteps: ReadonlyArray<{ id?: unknown }>;
    verificationSteps: ReadonlyArray<{ id?: unknown }>;
    agenticSteps: ReadonlyArray<{ id?: unknown }>;
    completionSteps?: ReadonlyArray<{ id?: unknown }>;
  },
): WorkflowPhase | null {
  if (workflow.setupSteps.some((s) => s.id === stepId)) return "setup";
  if (workflow.verificationSteps.some((s) => s.id === stepId)) return "verification";
  if (workflow.agenticSteps.some((s) => s.id === stepId)) return "agentic";
  if (workflow.completionSteps?.some((s) => s.id === stepId)) return "completion";
  return null;
}

interface StepConfigPanelProps {
  onClose?: () => void;
  onOpenWorkflowPicker?: () => void;
}

export function StepConfigPanel({ onClose, onOpenWorkflowPicker }: StepConfigPanelProps) {
  const { state, getSelectedStep, updateStep } = useWorkflowBuilder();
  const selectedStep = getSelectedStep();

  if (!selectedStep) {
    return (
      <div className="h-full flex items-center justify-center p-4">
        <div className="text-center text-zinc-500">
          <AlertCircle className="w-8 h-8 mx-auto mb-2 opacity-50" />
          <p>Select a step to configure</p>
        </div>
      </div>
    );
  }

  const phase = findStepPhase(selectedStep.id, state.workflow);

  if (!phase) {
    return (
      <div className="h-full flex items-center justify-center p-4">
        <div className="text-center text-zinc-500">
          <AlertCircle className="w-8 h-8 mx-auto mb-2 opacity-50" />
          <p>Step not found in workflow</p>
        </div>
      </div>
    );
  }

  const handleUpdate = (updates: Partial<UnifiedStep>) => {
    updateStep({ ...selectedStep, ...updates } as UnifiedStep, phase);
  };

  const stepTypeLabel = (() => {
    switch (selectedStep.type) {
      case "command": {
        const cmd = selectedStep as CommandStep;
        return cmd.testType || cmd.testId ? "Test" : "Command";
      }
      case "prompt":
        return "AI Prompt";
      case "ui_bridge":
        return "UI Bridge";
      case "workflow":
        return "Workflow";
      case "wrapper_action":
        return "Wrapper Action";
      default:
        return "Step";
    }
  })();

  // Wire-side UnifiedStep arrays include an open `Other` variant; narrow to
  // the runner-strict form so DataFlowSection can read `id`/`name`.
  const allSteps = [
    ...state.workflow.setupSteps,
    ...state.workflow.verificationSteps,
    ...state.workflow.agenticSteps,
    ...(state.workflow.completionSteps || []),
  ] as UnifiedStep[];

  return (
    <div className="h-full flex flex-col bg-zinc-850 border-l border-zinc-700">
      <div className="flex items-center justify-between p-4 border-b border-zinc-700">
        <div>
          <h3 className="text-sm font-medium text-zinc-200">Configure Step</h3>
          <p className="text-xs text-zinc-500">{stepTypeLabel}</p>
        </div>
        {onClose && (
          <button
            onClick={onClose}
            className="p-1 hover:bg-zinc-700 rounded transition-colors text-zinc-400 hover:text-zinc-200"
          >
            <X className="w-4 h-4" />
          </button>
        )}
      </div>

      <div className="flex-1 overflow-y-auto p-4">
        {selectedStep.skillOrigin && (
          <div className="mb-3 flex items-center justify-between gap-2 px-2.5 py-1.5 bg-zinc-800/50 border border-zinc-700/50 rounded-md">
            <span className="text-xs text-zinc-400">
              From skill:{" "}
              <span className="text-zinc-300 font-medium">
                {/* skill_origin is typed as an opaque map on the wire, but
                    all runner-produced origins include `skill_slug`. */}
                {String(
                  (selectedStep.skillOrigin as { skill_slug?: string } | undefined)?.skill_slug ??
                    "",
                )}
              </span>
            </span>
            <button
              className="text-[10px] text-zinc-500 hover:text-zinc-300 transition-colors"
              onClick={() =>
                handleUpdate({ skillOrigin: undefined } as unknown as Partial<UnifiedStep>)
              }
              title="Detach from skill — converts to raw step"
            >
              Detach
            </button>
          </div>
        )}

        <div className="mb-4">
          <label htmlFor="step-name-input" className="block text-sm font-medium text-zinc-400 mb-1">
            Step Name
          </label>
          <input
            id="step-name-input"
            type="text"
            value={selectedStep.name}
            onChange={(e) => handleUpdate({ name: e.target.value })}
            placeholder="Step name"
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
          />
        </div>

        {selectedStep.type === "command" && (
          <CommandConfig step={selectedStep} onUpdate={handleUpdate} />
        )}
        {selectedStep.type === "prompt" && (
          <PromptConfig step={selectedStep} onUpdate={handleUpdate} />
        )}
        {selectedStep.type === "ui_bridge" && (
          <UiBridgeConfig step={selectedStep} onUpdate={handleUpdate} />
        )}
        {selectedStep.type === "workflow" && (
          <WorkflowConfig
            step={selectedStep as UnifiedStep & { type: "workflow" }}
            onChangeWorkflow={onOpenWorkflowPicker}
          />
        )}
        {selectedStep.type === "wrapper_action" && (
          <WrapperActionStepConfig
            step={selectedStep as UnifiedStep & { type: "wrapper_action" }}
            onUpdate={handleUpdate}
          />
        )}

        {phase === "verification" && (
          <div className="mt-4 pt-4 border-t border-zinc-700">
            <h4 className="text-xs font-medium text-zinc-500 uppercase tracking-wider mb-3">
              Console Errors
            </h4>
            <div className="flex items-center gap-2">
              <input
                type="checkbox"
                id="failOnConsoleErrors"
                checked={(selectedStep as BaseStep).failOnConsoleErrors ?? false}
                onChange={(e) =>
                  handleUpdate({ failOnConsoleErrors: e.target.checked } as Partial<UnifiedStep>)
                }
                className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
              />
              <label htmlFor="failOnConsoleErrors" className="text-sm text-zinc-300">
                Fail on console errors
              </label>
            </div>
            <p className="text-xs text-zinc-500 mt-1 ml-6">
              Step will fail if browser console errors are detected during execution, even if the
              step itself passes.
            </p>
          </div>
        )}

        <DataFlowSection step={selectedStep} onUpdate={handleUpdate} allSteps={allSteps} />
      </div>
    </div>
  );
}
