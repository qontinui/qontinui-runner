/**
 * Plan Visualization Component
 *
 * Displays the orchestration plan as a visual flow diagram showing
 * steps, variable dependencies, and execution progress.
 */

import React, { useMemo } from "react";
import { ArrowRight, Check, Play, Link2, Variable } from "lucide-react";
import type { TestOrchestrationPlan } from "../../types/test-orchestrator";

interface PlanVisualizationProps {
  plan: TestOrchestrationPlan;
  currentStepIndex?: number;
  isExecuting?: boolean;
}

export function PlanVisualization({
  plan,
  currentStepIndex = -1,
  isExecuting = false,
}: PlanVisualizationProps) {
  // Group variable mappings by source step for easier lookup
  const variablesByStep = useMemo(() => {
    const map = new Map<number, typeof plan.variable_mappings>();
    for (const mapping of plan.variable_mappings) {
      const existing = map.get(mapping.source_step) || [];
      existing.push(mapping);
      map.set(mapping.source_step, existing);
    }
    return map;
  }, [plan]);

  return (
    <div className="space-y-3">
      {plan.steps.map((step, idx) => {
        const isComplete = idx < currentStepIndex;
        const isCurrent = idx === currentStepIndex && isExecuting;
        const variables = variablesByStep.get(idx) || [];

        return (
          <div key={step.step_index} className="relative">
            {/* Connection line to next step */}
            {idx < plan.steps.length - 1 && (
              <div className="absolute left-5 top-full w-0.5 h-3 bg-neutral-700" />
            )}

            {/* Step card */}
            <div
              className={`flex items-start gap-3 p-3 rounded border transition-all ${
                isComplete
                  ? "bg-green-900/20 border-green-700/50"
                  : isCurrent
                    ? "bg-blue-900/30 border-blue-500 shadow-lg shadow-blue-500/10"
                    : "bg-neutral-800 border-neutral-700"
              }`}
            >
              {/* Step indicator */}
              <div
                className={`w-10 h-10 rounded-full flex items-center justify-center shrink-0 ${
                  isComplete
                    ? "bg-green-600"
                    : isCurrent
                      ? "bg-blue-500 animate-pulse"
                      : "bg-neutral-700"
                }`}
              >
                {isComplete ? (
                  <Check className="w-5 h-5 text-white" />
                ) : isCurrent ? (
                  <Play className="w-5 h-5 text-white" />
                ) : (
                  <span className="text-sm font-medium text-neutral-300">{idx + 1}</span>
                )}
              </div>

              {/* Step content */}
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <span
                    data-content-role="label"
                    data-content-label="step name"
                    className="font-medium text-neutral-200"
                  >
                    {step.name}
                  </span>
                  {step.depends_on.length > 0 && (
                    <span className="text-xs text-neutral-500">
                      (needs step {step.depends_on.map((d) => d + 1).join(", ")})
                    </span>
                  )}
                </div>

                <div
                  data-content-role="description"
                  data-content-label="step purpose"
                  className="text-sm text-neutral-400 mt-1"
                >
                  {step.purpose}
                </div>

                {/* URL preview */}
                <div className="mt-2 flex items-center gap-2">
                  <MethodBadge method={extractMethod(step.url_template)} />
                  <code className="text-xs text-neutral-500 truncate flex-1">
                    {highlightVariables(step.url_template)}
                  </code>
                </div>

                {/* Variables extracted */}
                {step.extractions.length > 0 && (
                  <div className="mt-2 flex flex-wrap gap-1">
                    {step.extractions.map((ext, extIdx) => (
                      <div
                        key={extIdx}
                        className="flex items-center gap-1 px-2 py-0.5 bg-purple-900/30 border border-purple-700/50 rounded text-xs"
                      >
                        <Variable className="w-3 h-3 text-purple-400" />
                        <span
                          data-content-role="label"
                          data-content-label="variable name"
                          className="text-purple-300"
                        >
                          {ext.variable_name}
                        </span>
                        <span className="text-purple-500">←</span>
                        <span
                          data-content-role="code"
                          data-content-label="json path"
                          className="text-purple-400 font-mono"
                        >
                          {ext.json_path}
                        </span>
                      </div>
                    ))}
                  </div>
                )}

                {/* Variables used in next steps */}
                {variables.length > 0 && (
                  <div className="mt-2 flex items-center gap-2 text-xs text-neutral-500">
                    <Link2 className="w-3 h-3" />
                    <span>
                      Passes{" "}
                      <span className="text-purple-400">
                        {variables.map((v) => v.variable_name).join(", ")}
                      </span>{" "}
                      to step(s){" "}
                      {variables
                        .flatMap((v) => v.used_in_steps)
                        .map((s) => s + 1)
                        .filter((v, i, a) => a.indexOf(v) === i)
                        .join(", ")}
                    </span>
                  </div>
                )}
              </div>
            </div>

            {/* Variable flow arrow (if this step has variable outputs used later) */}
            {variables.length > 0 && idx < plan.steps.length - 1 && (
              <div className="flex items-center justify-center py-1">
                <div className="flex items-center gap-1 px-2 py-0.5 bg-purple-900/20 rounded text-xs text-purple-400">
                  <Variable className="w-3 h-3" />
                  {variables.map((v) => v.variable_name).join(", ")}
                  <ArrowRight className="w-3 h-3" />
                </div>
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

/**
 * Extract HTTP method from URL (assumes saved request has method in some form)
 */
function extractMethod(_url: string): string {
  // This is a placeholder - the actual method comes from the saved request
  return "API";
}

/**
 * Highlight {{variables}} in a string
 */
function highlightVariables(text: string): React.ReactElement {
  const parts = text.split(/(\{\{[^}]+\}\})/g);
  return (
    <>
      {parts.map((part, idx) => {
        if (part.match(/^\{\{[^}]+\}\}$/)) {
          return (
            <span key={idx} className="text-purple-400 font-semibold">
              {part}
            </span>
          );
        }
        return <span key={idx}>{part}</span>;
      })}
    </>
  );
}

/**
 * HTTP method badge
 */
function MethodBadge({ method }: { method: string }) {
  const colors: Record<string, string> = {
    GET: "bg-green-900/50 text-green-400",
    POST: "bg-blue-900/50 text-blue-400",
    PUT: "bg-yellow-900/50 text-yellow-400",
    PATCH: "bg-orange-900/50 text-orange-400",
    DELETE: "bg-red-900/50 text-red-400",
    API: "bg-neutral-700 text-neutral-400",
  };

  return (
    <span className={`px-1.5 py-0.5 text-xs font-mono rounded ${colors[method] || colors.API}`}>
      {method}
    </span>
  );
}
