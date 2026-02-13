/**
 * Execution Log Component
 *
 * Displays the results of executing an orchestration plan,
 * including request/response details and extracted variables.
 */

import { useState } from "react";
import { Check, X, ChevronDown, ChevronRight, Clock, Variable } from "lucide-react";
import type {
  OrchestrationExecutionResult,
  StepExecutionResult,
} from "../../types/test-orchestrator";

interface ExecutionLogProps {
  result: OrchestrationExecutionResult;
}

export function ExecutionLog({ result }: ExecutionLogProps) {
  const [expandedSteps, setExpandedSteps] = useState<Set<number>>(
    new Set(result.failed_at_step !== undefined ? [result.failed_at_step] : []),
  );

  const toggleStep = (index: number) => {
    setExpandedSteps((prev) => {
      const next = new Set(prev);
      if (next.has(index)) {
        next.delete(index);
      } else {
        next.add(index);
      }
      return next;
    });
  };

  const expandAll = () => {
    setExpandedSteps(new Set(result.step_results.map((_, i) => i)));
  };

  const collapseAll = () => {
    setExpandedSteps(new Set());
  };

  return (
    <div className="space-y-3">
      {/* Summary header */}
      <div
        className={`flex items-center justify-between p-3 rounded ${
          result.success
            ? "bg-green-900/20 border border-green-700/50"
            : "bg-red-900/20 border border-red-700/50"
        }`}
      >
        <div className="flex items-center gap-2">
          {result.success ? (
            <Check className="w-5 h-5 text-green-400" />
          ) : (
            <X className="w-5 h-5 text-red-400" />
          )}
          <span
            data-content-role="status"
            data-content-label="execution result"
            className={`font-medium ${result.success ? "text-green-400" : "text-red-400"}`}
          >
            {result.success ? "Execution Successful" : "Execution Failed"}
          </span>
        </div>
        <div className="flex items-center gap-4 text-sm text-neutral-400">
          <span
            data-content-role="metric"
            data-content-label="total duration"
            className="flex items-center gap-1"
          >
            <Clock className="w-4 h-4" />
            {result.total_duration_ms}ms
          </span>
          <span data-content-role="metric" data-content-label="steps passed">
            {result.step_results.filter((s) => s.success).length}/{result.step_results.length} steps
            passed
          </span>
        </div>
      </div>

      {/* Expand/Collapse controls */}
      <div className="flex items-center gap-2 text-xs">
        <button
          onClick={expandAll}
          className="px-2 py-1 bg-neutral-800 text-neutral-400 rounded hover:bg-neutral-700"
          data-ui-id="test-builder-execution-expand-all-btn"
        >
          Expand All
        </button>
        <button
          onClick={collapseAll}
          className="px-2 py-1 bg-neutral-800 text-neutral-400 rounded hover:bg-neutral-700"
          data-ui-id="test-builder-execution-collapse-all-btn"
        >
          Collapse All
        </button>
      </div>

      {/* All extracted variables */}
      {Object.keys(result.all_variables).length > 0 && (
        <div className="p-3 bg-purple-900/20 border border-purple-700/30 rounded">
          <div
            data-content-role="heading"
            className="flex items-center gap-2 text-xs font-medium text-purple-400 uppercase mb-2"
          >
            <Variable className="w-4 h-4" />
            All Extracted Variables
          </div>
          <div className="flex flex-wrap gap-2">
            {Object.entries(result.all_variables).map(([name, value]) => (
              <div key={name} className="px-2 py-1 bg-neutral-800 rounded text-xs font-mono">
                <span className="text-purple-400">{name}</span>
                <span className="text-neutral-500"> = </span>
                <span className="text-neutral-300">{formatValue(value, 50)}</span>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Step results */}
      <div className="space-y-2">
        {result.step_results.map((step, idx) => (
          <StepResult
            key={idx}
            step={step}
            isExpanded={expandedSteps.has(idx)}
            onToggle={() => toggleStep(idx)}
          />
        ))}
      </div>
    </div>
  );
}

interface StepResultProps {
  step: StepExecutionResult;
  isExpanded: boolean;
  onToggle: () => void;
}

function StepResult({ step, isExpanded, onToggle }: StepResultProps) {
  return (
    <div
      className={`border rounded overflow-hidden ${
        step.success ? "border-neutral-700" : "border-red-700/50"
      }`}
    >
      {/* Header */}
      <button
        onClick={onToggle}
        className={`w-full flex items-center gap-3 p-3 text-left hover:bg-neutral-700/30 ${
          step.success ? "bg-neutral-800" : "bg-red-900/20"
        }`}
      >
        {isExpanded ? (
          <ChevronDown className="w-4 h-4 text-neutral-500" />
        ) : (
          <ChevronRight className="w-4 h-4 text-neutral-500" />
        )}

        <div
          className={`w-5 h-5 rounded-full flex items-center justify-center ${
            step.success ? "bg-green-600" : "bg-red-600"
          }`}
        >
          {step.success ? (
            <Check className="w-3 h-3 text-white" />
          ) : (
            <X className="w-3 h-3 text-white" />
          )}
        </div>

        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span
              data-content-role="label"
              data-content-label="step name"
              className="font-medium text-neutral-200"
            >
              {step.step_name}
            </span>
            <StatusBadge code={step.response.status_code} />
            <span
              data-content-role="metric"
              data-content-label="step duration"
              className="text-xs text-neutral-500"
            >
              {step.duration_ms}ms
            </span>
          </div>
          <div className="text-xs text-neutral-500 truncate">
            {step.request.method} {step.request.url}
          </div>
        </div>

        {Object.keys(step.extracted_variables).length > 0 && (
          <div className="flex items-center gap-1 px-2 py-0.5 bg-purple-900/30 rounded text-xs text-purple-400">
            <Variable className="w-3 h-3" />
            {Object.keys(step.extracted_variables).length} var
          </div>
        )}
      </button>

      {/* Expanded content */}
      {isExpanded && (
        <div className="border-t border-neutral-700 p-3 space-y-3 bg-neutral-900">
          {/* Error message */}
          {step.error && (
            <div className="p-2 bg-red-900/30 border border-red-700/50 rounded text-sm text-red-400">
              {step.error}
            </div>
          )}

          {/* Request details */}
          <div className="space-y-2">
            <div
              data-content-role="heading"
              className="text-xs font-medium text-neutral-400 uppercase"
            >
              Request
            </div>
            <div className="p-2 bg-neutral-950 rounded">
              <div className="flex items-center gap-2 text-sm">
                <MethodBadge method={step.request.method} />
                <code className="text-neutral-300 break-all">{step.request.url}</code>
              </div>
              {step.request.body && (
                <div className="mt-2">
                  <div className="text-xs text-neutral-500 mb-1">Body:</div>
                  <pre className="text-xs text-neutral-400 overflow-auto max-h-32">
                    {formatJson(step.request.body)}
                  </pre>
                </div>
              )}
            </div>
          </div>

          {/* Response details */}
          <div className="space-y-2">
            <div
              data-content-role="heading"
              className="text-xs font-medium text-neutral-400 uppercase flex items-center gap-2"
            >
              Response
              <span
                data-content-role="metric"
                data-content-label="response size"
                className="text-neutral-500"
              >
                ({step.response.size_bytes} bytes)
              </span>
            </div>
            <div className="p-2 bg-neutral-950 rounded">
              <pre className="text-xs text-neutral-400 overflow-auto max-h-48">
                {formatJson(step.response.body)}
              </pre>
            </div>
          </div>

          {/* Extracted variables */}
          {Object.keys(step.extracted_variables).length > 0 && (
            <div className="space-y-2">
              <div
                data-content-role="heading"
                className="text-xs font-medium text-purple-400 uppercase flex items-center gap-2"
              >
                <Variable className="w-3 h-3" />
                Extracted Variables
              </div>
              <div className="space-y-1">
                {Object.entries(step.extracted_variables).map(([name, value]) => (
                  <div
                    key={name}
                    className="flex items-start gap-2 p-2 bg-purple-900/20 rounded text-sm font-mono"
                  >
                    <span className="text-purple-400 shrink-0">{name}</span>
                    <span className="text-neutral-500">=</span>
                    <span className="text-neutral-300 break-all">{formatValue(value, 200)}</span>
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/**
 * HTTP status badge
 */
function StatusBadge({ code }: { code: number }) {
  let color = "bg-neutral-700 text-neutral-400";
  if (code >= 200 && code < 300) {
    color = "bg-green-900/50 text-green-400";
  } else if (code >= 400 && code < 500) {
    color = "bg-yellow-900/50 text-yellow-400";
  } else if (code >= 500) {
    color = "bg-red-900/50 text-red-400";
  }

  return <span className={`px-1.5 py-0.5 text-xs font-mono rounded ${color}`}>{code}</span>;
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
  };

  return (
    <span
      className={`px-1.5 py-0.5 text-xs font-mono rounded ${colors[method] || "bg-neutral-700 text-neutral-400"}`}
    >
      {method}
    </span>
  );
}

/**
 * Format a value for display, truncating if needed
 */
function formatValue(value: unknown, maxLength: number): string {
  if (value === null) return "null";
  if (value === undefined) return "undefined";

  let str: string;
  if (typeof value === "string") {
    str = `"${value}"`;
  } else if (typeof value === "object") {
    str = JSON.stringify(value);
  } else {
    str = String(value);
  }

  if (str.length > maxLength) {
    return str.slice(0, maxLength) + "...";
  }
  return str;
}

/**
 * Format JSON for display
 */
function formatJson(value: unknown): string {
  if (typeof value === "string") {
    // Try to parse and pretty-print
    try {
      const parsed = JSON.parse(value);
      return JSON.stringify(parsed, null, 2);
    } catch {
      return value;
    }
  }
  return JSON.stringify(value, null, 2);
}
