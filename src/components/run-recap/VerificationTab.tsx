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
  Globe,
} from "lucide-react";
import { useTaskRunVerificationResults } from "../../hooks/useAiData";
import type {
  ConsoleError,
  VerificationPhaseResult,
  VerificationStepResult,
  SnapshotAssertOutputData,
  SnapshotAssertionResult,
} from "../../types/aiData";

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
    case "command":
    case "shell_command": // Legacy
      return <FileText className="w-4 h-4" />;
    case "ui_bridge":
      return <Globe className="w-4 h-4" />;
    case "prompt":
      return <AlertCircle className="w-4 h-4" />;
    default:
      return <AlertCircle className="w-4 h-4" />;
  }
}

/**
 * Check if output_data is a snapshot_assert result.
 */
function isSnapshotAssertOutput(data: Record<string, unknown> | null | undefined): boolean {
  return data != null && data.action === "snapshot_assert" && Array.isArray(data.results);
}

/**
 * Displays per-assertion results from a snapshot_assert UI Bridge step.
 */
function SnapshotAssertResults({
  data,
  dataUiPrefix: _dataUiPrefix,
}: {
  data: SnapshotAssertOutputData;
  dataUiPrefix: string;
}) {
  return (
    <div>
      <h4 className="text-sm font-medium mb-2 flex items-center gap-1.5">
        <Globe className="w-3.5 h-3.5" />
        Snapshot Assertions ({data.passed}/{data.total} passed)
      </h4>
      <div className="space-y-1">
        {data.results.map((assertion: SnapshotAssertionResult) => (
          <div
            key={assertion.id}
            className={`text-xs p-2 rounded flex items-start gap-2 ${
              assertion.passed ? "bg-green-500/10 text-green-400" : "bg-red-500/10 text-red-400"
            }`}
          >
            <span className="shrink-0 mt-0.5">
              {assertion.passed ? (
                <CheckCircle2 className="w-3.5 h-3.5" />
              ) : (
                <XCircle className="w-3.5 h-3.5" />
              )}
            </span>
            <div className="flex-1 min-w-0">
              <div className="flex items-center gap-2 mb-0.5">
                <span className="font-medium">{assertion.description}</span>
                <span className="text-[10px] px-1.5 py-0.5 rounded bg-muted/50 text-muted-foreground font-mono">
                  {assertion.assertionType}
                </span>
                <span
                  className={`text-[10px] px-1.5 py-0.5 rounded ${
                    assertion.severity === "critical"
                      ? "bg-red-500/20 text-red-400"
                      : assertion.severity === "warning"
                        ? "bg-amber-500/20 text-amber-400"
                        : "bg-muted/50 text-muted-foreground"
                  }`}
                >
                  {assertion.severity}
                </span>
              </div>
              <span className="text-muted-foreground">{assertion.detail}</span>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

function ConsoleErrorsList({
  errors,
  dataUiPrefix: _dataUiPrefix,
}: {
  errors: ConsoleError[];
  dataUiPrefix: string;
}) {
  return (
    <div>
      <h4 className="text-sm font-medium mb-1 text-red-400 flex items-center gap-1.5">
        <AlertTriangle className="w-3.5 h-3.5" />
        Console Errors ({errors.length})
      </h4>
      <div className="space-y-1">
        {errors.slice(0, 15).map((err, i) => (
          <div
            key={`err-${err.type}-${i}`}
            className={`text-xs p-2 rounded flex items-start gap-2 ${
              err.type === "warn" ? "bg-amber-500/10 text-amber-400" : "bg-red-500/10 text-red-400"
            }`}
          >
            <span className="font-mono opacity-70 shrink-0">[{err.type}]</span>
            <span className="whitespace-pre-wrap break-all">{err.message}</span>
          </div>
        ))}
        {errors.length > 15 && (
          <p className="text-xs text-muted-foreground">
            ... and {errors.length - 15} more console errors
          </p>
        )}
      </div>
    </div>
  );
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
  const consoleErrors = step.verification_details?.console_errors;
  const hasConsoleErrors = consoleErrors && consoleErrors.length > 0;
  const snapshotAssertData = isSnapshotAssertOutput(step.output_data)
    ? (step.output_data as unknown as SnapshotAssertOutputData)
    : null;
  const hasDetails =
    step.error ||
    step.verification_details?.stdout ||
    step.verification_details?.stderr ||
    step.verification_details?.console_output ||
    hasConsoleErrors ||
    snapshotAssertData;

  // Resolve assertion counts: prefer verification_details, fall back to snapshot_assert output_data
  const assertionsPassed =
    step.verification_details?.assertions_passed ?? snapshotAssertData?.passed ?? undefined;
  const assertionsTotal =
    step.verification_details?.assertions_total ?? snapshotAssertData?.total ?? undefined;

  return (
    <div className="border border-border rounded-lg overflow-hidden">
      <button
        onClick={onToggle}
        disabled={!hasDetails}
        className={`w-full px-4 py-3 flex items-center justify-between transition-colors ${
          hasDetails ? "hover:bg-muted/50 cursor-pointer" : "cursor-default"
        }`}
      >
        <div className="flex items-center gap-3">
          {/* Status indicator */}
          <div>
            {step.success ? (
              <CheckCircle2 className="w-5 h-5 text-green-500" />
            ) : (
              <XCircle className="w-5 h-5 text-red-500" />
            )}
          </div>

          {/* Type indicator */}
          <div
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
          {assertionsTotal !== undefined && assertionsTotal !== null && (
            <span className="text-xs text-muted-foreground">
              {assertionsPassed ?? 0}/{assertionsTotal} assertions
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
          {/* Snapshot Assert per-assertion results */}
          {snapshotAssertData && (
            <SnapshotAssertResults
              data={snapshotAssertData}
              dataUiPrefix={`verification-step-${step.step_index}`}
            />
          )}

          {/* Error message */}
          {step.error && (
            <div>
              <h4 className="text-sm font-medium mb-1 text-red-400">Error</h4>
              <pre className="text-xs text-red-400/80 bg-red-500/10 p-2 rounded overflow-x-auto whitespace-pre-wrap">
                {step.error}
              </pre>
            </div>
          )}

          {/* Stdout (skip for snapshot_assert since we show per-assertion results) */}
          {step.verification_details?.stdout && !snapshotAssertData && (
            <div>
              <h4 className="text-sm font-medium mb-1">Output</h4>
              <pre className="text-xs text-muted-foreground bg-muted p-2 rounded overflow-x-auto whitespace-pre-wrap max-h-48 overflow-y-auto">
                {step.verification_details.stdout}
              </pre>
            </div>
          )}

          {/* Stderr */}
          {step.verification_details?.stderr && (
            <div>
              <h4 className="text-sm font-medium mb-1 text-amber-400">Stderr</h4>
              <pre className="text-xs text-amber-400/80 bg-amber-500/10 p-2 rounded overflow-x-auto whitespace-pre-wrap max-h-48 overflow-y-auto">
                {step.verification_details.stderr}
              </pre>
            </div>
          )}

          {/* Console output (for browser tests) */}
          {step.verification_details?.console_output && (
            <div>
              <h4 className="text-sm font-medium mb-1">Console Output</h4>
              <pre className="text-xs text-muted-foreground bg-muted p-2 rounded overflow-x-auto whitespace-pre-wrap max-h-48 overflow-y-auto">
                {step.verification_details.console_output}
              </pre>
            </div>
          )}

          {/* Console errors (from UI Bridge ConsoleCapture) */}
          {hasConsoleErrors && (
            <ConsoleErrorsList
              errors={consoleErrors}
              dataUiPrefix={`verification-step-${step.step_index}`}
            />
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
    <div className="card overflow-hidden">
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
            {result.console_errors && result.console_errors.length > 0 && (
              <span className="text-xs px-2 py-0.5 rounded bg-amber-500/10 text-amber-500 flex items-center gap-1">
                <AlertTriangle className="w-3 h-3" />
                {result.console_errors.length} console error
                {result.console_errors.length !== 1 ? "s" : ""}
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

          {/* Phase-level console errors */}
          {result.console_errors && result.console_errors.length > 0 && (
            <div className="mt-3 border border-red-500/20 rounded-lg p-3">
              <ConsoleErrorsList
                errors={result.console_errors}
                dataUiPrefix={`verification-iteration-${result.iteration}`}
              />
            </div>
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
        <div className="card p-6 text-center">
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
            <div
              data-content-role="metric"
              data-content-label="verification pass rate"
              className="text-2xl font-bold"
            >
              {data.passed_iterations}/{data.count}
            </div>
            <div data-content-role="label" className="text-xs opacity-80">
              iterations passed
            </div>
          </div>
        </div>
      </div>

      {/* Iteration Results */}
      <div className="space-y-3">
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
