/**
 * VerificationTab Component
 *
 * Displays verification phase results for a task run.
 * Shows each iteration's test/check results with pass/fail status and details.
 */

import { useState } from "react";
import {
  CheckCircle2,
  XCircle,
  AlertTriangle,
  ChevronDown,
  ChevronRight,
  Clock,
  Loader2,
  FlaskConical,
  ShieldCheck,
  FileText,
  AlertCircle,
} from "lucide-react";
import { useTaskRunVerificationResults } from "../../hooks/useAiData";
import type { VerificationPhaseResult, VerificationStepResult } from "../../types/aiData";

interface VerificationTabProps {
  taskRunId: string;
}

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  const mins = Math.floor(ms / 60000);
  const secs = Math.floor((ms % 60000) / 1000);
  return `${mins}m ${secs}s`;
}

function StepTypeIcon({ stepType }: { stepType: string }) {
  switch (stepType.toLowerCase()) {
    case "test":
    case "playwright_test":
      return <FlaskConical className="w-4 h-4" />;
    case "check":
    case "code_check":
      return <ShieldCheck className="w-4 h-4" />;
    case "shell_command":
      return <FileText className="w-4 h-4" />;
    default:
      return <AlertCircle className="w-4 h-4" />;
  }
}

function StepResultCard({
  step,
  isExpanded,
  onToggle,
}: {
  step: VerificationStepResult;
  isExpanded: boolean;
  onToggle: () => void;
}) {
  const hasDetails =
    step.error ||
    step.verification_details?.stdout ||
    step.verification_details?.stderr ||
    step.verification_details?.console_output;

  return (
    <div
      data-ui-id={`verification-step-${step.step_index}`}
      className="border border-border rounded-lg overflow-hidden"
    >
      <button
        onClick={onToggle}
        disabled={!hasDetails}
        className={`w-full px-4 py-3 flex items-center justify-between transition-colors ${
          hasDetails ? "hover:bg-muted/50 cursor-pointer" : "cursor-default"
        }`}
      >
        <div className="flex items-center gap-3">
          {/* Status indicator */}
          <div data-ui-id={`verification-step-${step.step_index}-status`}>
            {step.success ? (
              <CheckCircle2 className="w-5 h-5 text-green-500" />
            ) : (
              <XCircle className="w-5 h-5 text-red-500" />
            )}
          </div>

          {/* Type indicator */}
          <div
            data-ui-id={`verification-step-${step.step_index}-type`}
            className={`p-1.5 rounded ${
              step.success ? "bg-green-500/10 text-green-500" : "bg-red-500/10 text-red-500"
            }`}
          >
            <StepTypeIcon stepType={step.step_type} />
          </div>

          <div className="text-left">
            <span className="font-medium">{step.step_name}</span>
            <span className="text-xs text-muted-foreground ml-2">({step.step_type})</span>
          </div>
        </div>

        <div className="flex items-center gap-3">
          {/* Duration */}
          <span className="text-xs text-muted-foreground flex items-center gap-1">
            <Clock className="w-3 h-3" />
            {formatDuration(step.duration_ms)}
          </span>

          {/* Assertions count if available */}
          {step.verification_details?.assertions_total !== undefined &&
            step.verification_details?.assertions_total !== null && (
              <span className="text-xs text-muted-foreground">
                {step.verification_details.assertions_passed ?? 0}/
                {step.verification_details.assertions_total} assertions
              </span>
            )}

          {hasDetails &&
            (isExpanded ? (
              <ChevronDown className="w-4 h-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="w-4 h-4 text-muted-foreground" />
            ))}
        </div>
      </button>

      {isExpanded && hasDetails && (
        <div className="border-t border-border p-4 space-y-3 bg-muted/20">
          {/* Error message */}
          {step.error && (
            <div data-ui-id={`verification-step-${step.step_index}-error`}>
              <h4 className="text-sm font-medium mb-1 text-red-400">Error</h4>
              <pre className="text-xs text-red-400/80 bg-red-500/10 p-2 rounded overflow-x-auto whitespace-pre-wrap">
                {step.error}
              </pre>
            </div>
          )}

          {/* Stdout */}
          {step.verification_details?.stdout && (
            <div data-ui-id={`verification-step-${step.step_index}-stdout`}>
              <h4 className="text-sm font-medium mb-1">Output</h4>
              <pre className="text-xs text-muted-foreground bg-muted p-2 rounded overflow-x-auto whitespace-pre-wrap max-h-48 overflow-y-auto">
                {step.verification_details.stdout}
              </pre>
            </div>
          )}

          {/* Stderr */}
          {step.verification_details?.stderr && (
            <div data-ui-id={`verification-step-${step.step_index}-stderr`}>
              <h4 className="text-sm font-medium mb-1 text-amber-400">Stderr</h4>
              <pre className="text-xs text-amber-400/80 bg-amber-500/10 p-2 rounded overflow-x-auto whitespace-pre-wrap max-h-48 overflow-y-auto">
                {step.verification_details.stderr}
              </pre>
            </div>
          )}

          {/* Console output (for browser tests) */}
          {step.verification_details?.console_output && (
            <div data-ui-id={`verification-step-${step.step_index}-console`}>
              <h4 className="text-sm font-medium mb-1">Console Output</h4>
              <pre className="text-xs text-muted-foreground bg-muted p-2 rounded overflow-x-auto whitespace-pre-wrap max-h-48 overflow-y-auto">
                {step.verification_details.console_output}
              </pre>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function IterationCard({
  result,
  defaultExpanded,
}: {
  result: VerificationPhaseResult;
  defaultExpanded: boolean;
}) {
  const [isExpanded, setIsExpanded] = useState(defaultExpanded);
  const [expandedSteps, setExpandedSteps] = useState<Set<number>>(new Set());

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

  return (
    <div data-ui-id={`verification-iteration-${result.iteration}`} className="card overflow-hidden">
      <button
        onClick={() => setIsExpanded(!isExpanded)}
        className="w-full px-4 py-3 flex items-center justify-between hover:bg-muted/50 transition-colors"
      >
        <div className="flex items-center gap-3">
          {/* Status indicator */}
          {result.all_passed ? (
            <CheckCircle2 className="w-5 h-5 text-green-500" />
          ) : result.critical_failure ? (
            <AlertTriangle className="w-5 h-5 text-red-500" />
          ) : (
            <XCircle className="w-5 h-5 text-amber-500" />
          )}

          <span className="font-medium">Iteration {result.iteration}</span>

          {/* Stats badges */}
          <div className="flex items-center gap-2">
            <span className="text-xs px-2 py-0.5 rounded bg-green-500/10 text-green-500">
              {result.passed_steps} passed
            </span>
            {result.failed_steps > 0 && (
              <span className="text-xs px-2 py-0.5 rounded bg-red-500/10 text-red-500">
                {result.failed_steps} failed
              </span>
            )}
            {result.skipped_steps > 0 && (
              <span className="text-xs px-2 py-0.5 rounded bg-muted text-muted-foreground">
                {result.skipped_steps} skipped
              </span>
            )}
          </div>
        </div>

        <div className="flex items-center gap-3">
          <span className="text-xs text-muted-foreground flex items-center gap-1">
            <Clock className="w-3 h-3" />
            {formatDuration(result.total_duration_ms)}
          </span>
          {isExpanded ? (
            <ChevronDown className="w-4 h-4 text-muted-foreground" />
          ) : (
            <ChevronRight className="w-4 h-4 text-muted-foreground" />
          )}
        </div>
      </button>

      {isExpanded && (
        <div className="border-t border-border p-4 space-y-2">
          {result.step_results.length === 0 ? (
            <p className="text-sm text-muted-foreground">No steps recorded</p>
          ) : (
            result.step_results.map((step) => (
              <StepResultCard
                key={step.step_index}
                step={step}
                isExpanded={expandedSteps.has(step.step_index)}
                onToggle={() => toggleStep(step.step_index)}
              />
            ))
          )}
        </div>
      )}
    </div>
  );
}

export function VerificationTab({ taskRunId }: VerificationTabProps) {
  const { data, isLoading, error } = useTaskRunVerificationResults(taskRunId);

  // Loading state
  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8 text-muted-foreground">
        <Loader2 className="w-6 h-6 animate-spin mr-2" />
        <span>Loading verification results...</span>
      </div>
    );
  }

  // Error state
  if (error) {
    return (
      <div className="flex items-center justify-center py-8 text-red-400">
        <AlertTriangle className="w-5 h-5 mr-2" />
        <span>Failed to load verification results</span>
      </div>
    );
  }

  // No data state
  if (!data || data.results.length === 0) {
    return (
      <div className="space-y-4">
        <div data-ui-id="verification-empty-state" className="card p-6 text-center">
          <FlaskConical className="w-12 h-12 mx-auto mb-4 text-muted-foreground opacity-50" />
          <h3 className="font-medium text-lg mb-2">No Verification Results</h3>
          <p className="text-sm text-muted-foreground max-w-md mx-auto">
            No verification steps have been executed for this run. Verification steps include tests,
            checks, and other validation tasks defined in your workflow.
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {/* Summary Banner */}
      <div
        data-ui-id="verification-summary"
        className={`rounded-lg p-4 ${
          data.failed_iterations === 0
            ? "bg-green-500/10 text-green-500"
            : "bg-amber-500/10 text-amber-500"
        }`}
      >
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            {data.failed_iterations === 0 ? (
              <CheckCircle2 className="w-6 h-6" />
            ) : (
              <AlertTriangle className="w-6 h-6" />
            )}
            <div>
              <h3 className="font-medium">
                {data.failed_iterations === 0
                  ? "All Verification Passed"
                  : `${data.failed_iterations} of ${data.count} Iterations Failed`}
              </h3>
              <p className="text-sm opacity-80">
                {data.count} verification iteration{data.count !== 1 ? "s" : ""} executed
              </p>
            </div>
          </div>
          <div className="text-right">
            <div className="text-2xl font-bold">
              {data.passed_iterations}/{data.count}
            </div>
            <div className="text-xs opacity-80">iterations passed</div>
          </div>
        </div>
      </div>

      {/* Iteration Results */}
      <div data-ui-id="verification-iterations" className="space-y-3">
        {data.results.map((result, index) => (
          <IterationCard
            key={result.iteration}
            result={result}
            defaultExpanded={index === data.results.length - 1} // Expand the latest iteration
          />
        ))}
      </div>
    </div>
  );
}

export default VerificationTab;
