/**
 * Test Steps Preview Component
 *
 * Displays a structured preview of what the generated test will do,
 * showing each step with its action, expected outcome, and type.
 */

import { Settings, Send, CheckCircle, Trash2, ChevronRight } from "lucide-react";
import type { TestStep } from "../../types/test-orchestrator";

interface TestStepsPreviewProps {
  steps: TestStep[];
}

export function TestStepsPreview({ steps }: TestStepsPreviewProps) {
  if (!steps || steps.length === 0) {
    return null;
  }

  return (
    <div className="space-y-2">
      <h4 className="text-xs font-medium text-neutral-400 uppercase flex items-center gap-2">
        <ChevronRight className="w-4 h-4" />
        Test Steps Preview
      </h4>

      <div className="border border-neutral-700 rounded overflow-hidden">
        {steps.map((step, idx) => (
          <div
            key={idx}
            className={`flex items-start gap-3 p-3 ${
              idx > 0 ? "border-t border-neutral-700" : ""
            } ${getStepBackground(step.step_type)}`}
          >
            {/* Step number and icon */}
            <div className="flex items-center gap-2 shrink-0">
              <span className="w-6 h-6 rounded-full bg-neutral-700 text-white text-xs flex items-center justify-center">
                {step.step_number}
              </span>
              <StepTypeIcon type={step.step_type} />
            </div>

            {/* Step content */}
            <div className="flex-1 min-w-0">
              <div className="flex items-center gap-2">
                <StepTypeBadge type={step.step_type} />
                <span
                  data-content-role="body-text"
                  data-content-label="step action"
                  className="text-sm text-neutral-200"
                >
                  {step.action}
                </span>
              </div>
              <div className="text-xs text-neutral-500 mt-1 flex items-center gap-1">
                <span data-content-role="label" className="text-neutral-600">
                  Expected:
                </span>
                <span data-content-role="body-text" data-content-label="step expected outcome">
                  {step.expected}
                </span>
              </div>
            </div>
          </div>
        ))}
      </div>

      {/* Summary */}
      <div className="flex items-center gap-4 text-xs text-neutral-500 px-1">
        <span data-content-role="metric" data-content-label="setup count">
          {countByType(steps, "setup")} setup
        </span>
        <span data-content-role="metric" data-content-label="request count">
          {countByType(steps, "request")} request(s)
        </span>
        <span data-content-role="metric" data-content-label="assertion count">
          {countByType(steps, "assertion")} assertion(s)
        </span>
        <span data-content-role="metric" data-content-label="cleanup count">
          {countByType(steps, "cleanup")} cleanup
        </span>
      </div>
    </div>
  );
}

function getStepBackground(type: string): string {
  switch (type) {
    case "setup":
      return "bg-neutral-800/50";
    case "request":
      return "bg-blue-900/10";
    case "assertion":
      return "bg-green-900/10";
    case "cleanup":
      return "bg-orange-900/10";
    default:
      return "bg-neutral-800/50";
  }
}

function StepTypeIcon({ type }: { type: string }) {
  const iconClass = "w-4 h-4";

  switch (type) {
    case "setup":
      return <Settings className={`${iconClass} text-neutral-400`} />;
    case "request":
      return <Send className={`${iconClass} text-blue-400`} />;
    case "assertion":
      return <CheckCircle className={`${iconClass} text-green-400`} />;
    case "cleanup":
      return <Trash2 className={`${iconClass} text-orange-400`} />;
    default:
      return <ChevronRight className={`${iconClass} text-neutral-400`} />;
  }
}

function StepTypeBadge({ type }: { type: string }) {
  const colors: Record<string, string> = {
    setup: "bg-neutral-700 text-neutral-300",
    request: "bg-blue-900/50 text-blue-400",
    assertion: "bg-green-900/50 text-green-400",
    cleanup: "bg-orange-900/50 text-orange-400",
  };

  return (
    <span className={`px-1.5 py-0.5 text-xs rounded ${colors[type] || colors.setup}`}>{type}</span>
  );
}

function countByType(steps: TestStep[], type: string): number {
  return steps.filter((s) => s.step_type === type).length;
}
