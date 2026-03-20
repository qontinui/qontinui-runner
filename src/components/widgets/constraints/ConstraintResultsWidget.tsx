/**
 * ConstraintResultsWidget Component
 *
 * Standalone dashboard widget that visualizes real-time constraint evaluation
 * results during workflow execution. Listens for "constraint-results" events
 * from the Rust backend and displays per-iteration pass/fail results.
 *
 * Auto-shows when constraint results events arrive (same pattern as ConvergenceWidget).
 */

import { useState } from "react";
import {
  ShieldCheck,
  ShieldAlert,
  ShieldX,
  ChevronDown,
  ChevronRight,
  FileCode,
  AlertTriangle,
  Info,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { useConstraintResults } from "./useConstraintResults";
import type {
  ConstraintIterationResults,
  ConstraintResult,
  ConstraintSeverity,
  ConstraintViolation,
} from "./types";
import { calculateCounts } from "./types";

/**
 * Get color classes for a constraint severity level.
 */
function getSeverityColors(severity: ConstraintSeverity): {
  text: string;
  bg: string;
  border: string;
} {
  switch (severity) {
    case "block":
      return {
        text: "text-red-400",
        bg: "bg-red-500/10",
        border: "border-red-500/30",
      };
    case "warn":
      return {
        text: "text-amber-400",
        bg: "bg-amber-500/10",
        border: "border-amber-500/30",
      };
    case "log":
      return {
        text: "text-zinc-400",
        bg: "bg-zinc-500/10",
        border: "border-zinc-500/30",
      };
  }
}

/**
 * Get severity label for display.
 */
function getSeverityLabel(severity: ConstraintSeverity): string {
  switch (severity) {
    case "block":
      return "Block";
    case "warn":
      return "Warn";
    case "log":
      return "Log";
  }
}

/**
 * Icon for a severity level.
 */
function SeverityIcon({
  severity,
  className,
}: {
  severity: ConstraintSeverity;
  className?: string;
}) {
  const iconClass = cn("w-3.5 h-3.5", className);
  switch (severity) {
    case "block":
      return <ShieldX className={iconClass} />;
    case "warn":
      return <AlertTriangle className={iconClass} />;
    case "log":
      return <Info className={iconClass} />;
  }
}

/**
 * Single violation row.
 */
function ViolationRow({ violation }: { violation: ConstraintViolation }) {
  return (
    <div className="flex items-start gap-2 py-1 px-3 text-xs">
      {violation.file && (
        <div className="flex items-center gap-1 flex-shrink-0 text-zinc-400">
          <FileCode className="w-3 h-3" />
          <span className="font-mono">
            {violation.file}
            {violation.line != null && `:${violation.line}`}
          </span>
        </div>
      )}
      <span className="text-zinc-300">{violation.detail}</span>
    </div>
  );
}

/**
 * Single constraint result row with expandable violations.
 */
function ConstraintRow({ result }: { result: ConstraintResult }) {
  const [expanded, setExpanded] = useState(false);
  const hasViolations = result.violations.length > 0;
  const severityColors = getSeverityColors(result.severity);

  return (
    <div className="flex flex-col">
      <button
        className={cn(
          "flex items-center gap-2 px-3 py-2 text-left transition-colors w-full",
          hasViolations && "cursor-pointer hover:bg-zinc-700/30",
          !hasViolations && "cursor-default",
        )}
        onClick={() => hasViolations && setExpanded(!expanded)}
        disabled={!hasViolations}
      >
        {/* Expand indicator */}
        <div className="flex-shrink-0 w-4">
          {hasViolations &&
            (expanded ? (
              <ChevronDown className="w-3.5 h-3.5 text-zinc-500" />
            ) : (
              <ChevronRight className="w-3.5 h-3.5 text-zinc-500" />
            ))}
        </div>

        {/* Pass/fail icon */}
        {result.passed ? (
          <ShieldCheck className="w-4 h-4 text-emerald-400 flex-shrink-0" />
        ) : (
          <SeverityIcon severity={result.severity} className={severityColors.text} />
        )}

        {/* Constraint name */}
        <span
          className={cn(
            "flex-1 text-sm truncate",
            result.passed ? "text-zinc-300" : "text-zinc-200",
          )}
        >
          {result.constraint_name}
        </span>

        {/* Severity badge (only for failures) */}
        {!result.passed && (
          <span
            className={cn(
              "text-[10px] font-medium px-1.5 py-0.5 rounded border",
              severityColors.bg,
              severityColors.text,
              severityColors.border,
            )}
          >
            {getSeverityLabel(result.severity)}
          </span>
        )}

        {/* Violation count */}
        {hasViolations && (
          <span className="text-[10px] text-zinc-500">
            {result.violations.length} violation{result.violations.length !== 1 ? "s" : ""}
          </span>
        )}
      </button>

      {/* Expanded violations */}
      {expanded && hasViolations && (
        <div
          className={cn("ml-8 mr-3 mb-1 rounded border", severityColors.bg, severityColors.border)}
        >
          {result.violations.map((v, idx) => (
            <ViolationRow key={`${v.file ?? ""}-${idx}`} violation={v} />
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * Header for a single iteration's results.
 */
function IterationHeader({
  iteration,
  expanded,
  onToggle,
}: {
  iteration: ConstraintIterationResults;
  expanded: boolean;
  onToggle: () => void;
}) {
  const counts = calculateCounts(iteration.results);
  const allPassed = counts.failed === 0;

  return (
    <button
      className={cn(
        "flex items-center gap-2 w-full px-3 py-2 text-left transition-colors",
        "hover:bg-zinc-700/30 border-b border-zinc-700/50",
      )}
      onClick={onToggle}
    >
      {expanded ? (
        <ChevronDown className="w-3.5 h-3.5 text-zinc-500" />
      ) : (
        <ChevronRight className="w-3.5 h-3.5 text-zinc-500" />
      )}

      <span className="text-xs font-medium text-zinc-400">Iteration {iteration.iteration}</span>

      {/* Pass/fail summary badge */}
      {allPassed ? (
        <span className="text-[10px] font-medium px-1.5 py-0.5 rounded bg-emerald-500/10 text-emerald-400 border border-emerald-500/30">
          {counts.passed}/{counts.total} passed
        </span>
      ) : (
        <span className="text-[10px] font-medium px-1.5 py-0.5 rounded bg-red-500/10 text-red-400 border border-red-500/30">
          {counts.failed} failed
        </span>
      )}

      {/* Blocking indicator */}
      {iteration.hasBlocking && <ShieldX className="w-3.5 h-3.5 text-red-400" />}
    </button>
  );
}

/**
 * ConstraintResultsWidget component.
 *
 * Listens for constraint-results events and displays a live view of
 * constraint evaluation results per iteration.
 */
export function ConstraintResultsWidget() {
  const data = useConstraintResults();
  const [expandedIteration, setExpandedIteration] = useState<number | null>(null);

  // Auto-expand the latest iteration when new data arrives
  const latestIteration = data.latest?.iteration ?? null;

  // Don't render if no data
  if (!data.hasData) {
    return null;
  }

  const { latestCounts } = data;
  const allPassed = latestCounts.failed === 0;

  // For single iteration, show results directly. For multiple, show collapsible list.
  const singleIteration = data.iterations.length === 1;

  return (
    <div className="flex flex-col gap-0 bg-zinc-800/50 rounded-lg border border-zinc-700/50 overflow-hidden">
      {/* Header */}
      <div className="flex items-center justify-between px-3 py-2 border-b border-zinc-700/50">
        <div className="flex items-center gap-2">
          {allPassed ? (
            <ShieldCheck className="w-4 h-4 text-emerald-400" />
          ) : (
            <ShieldAlert className="w-4 h-4 text-red-400" />
          )}
          <h3 className="text-xs font-semibold text-zinc-300 uppercase tracking-wider">
            Constraints
          </h3>
        </div>

        {/* Summary counts */}
        <div className="flex items-center gap-2">
          {latestCounts.blocking > 0 && (
            <span className="text-[10px] text-red-400 font-medium">
              {latestCounts.blocking} blocking
            </span>
          )}
          {latestCounts.warnings > 0 && (
            <span className="text-[10px] text-amber-400 font-medium">
              {latestCounts.warnings} warn
            </span>
          )}
          <span
            className={cn(
              "text-[10px] font-medium",
              allPassed ? "text-emerald-400" : "text-zinc-400",
            )}
          >
            {latestCounts.passed}/{latestCounts.total} passed
          </span>
        </div>
      </div>

      {/* Mini progress bar */}
      <div className="h-1 bg-zinc-700/50">
        <div
          className={cn(
            "h-full transition-all duration-300",
            allPassed
              ? "bg-emerald-500"
              : latestCounts.blocking > 0
                ? "bg-red-500"
                : "bg-amber-500",
          )}
          style={{
            width: `${latestCounts.total > 0 ? (latestCounts.passed / latestCounts.total) * 100 : 0}%`,
          }}
        />
      </div>

      {/* Content */}
      {singleIteration ? (
        /* Single iteration: show results directly */
        <div className="flex flex-col max-h-64 overflow-y-auto">
          {data.latest!.results.map((result) => (
            <ConstraintRow key={result.constraint_id} result={result} />
          ))}
        </div>
      ) : (
        /* Multiple iterations: collapsible per-iteration sections */
        <div className="flex flex-col max-h-72 overflow-y-auto">
          {data.iterations.map((iteration) => {
            const isExpanded =
              expandedIteration === iteration.iteration ||
              (expandedIteration === null && iteration.iteration === latestIteration);

            return (
              <div key={iteration.iteration}>
                <IterationHeader
                  iteration={iteration}
                  expanded={isExpanded}
                  onToggle={() =>
                    setExpandedIteration((prev) =>
                      prev === iteration.iteration ? null : iteration.iteration,
                    )
                  }
                />
                {isExpanded && (
                  <div className="flex flex-col">
                    {iteration.results.map((result) => (
                      <ConstraintRow key={result.constraint_id} result={result} />
                    ))}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}

      {/* Summary text */}
      {data.latest && (
        <div className="px-3 py-1.5 border-t border-zinc-700/50 text-[10px] text-zinc-500">
          {data.latest.summary}
        </div>
      )}
    </div>
  );
}

export default ConstraintResultsWidget;
