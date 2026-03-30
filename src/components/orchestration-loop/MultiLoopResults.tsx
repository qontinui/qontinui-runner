/**
 * MultiLoopResults
 *
 * Displays aggregated results from a completed multi-loop run.
 * Shows per-loop summaries: iterations completed, total fixes, final status.
 */

import { cn } from "../../lib/utils";
import { CheckCircle, XCircle, AlertTriangle, MinusCircle } from "lucide-react";

interface IterationResult {
  iteration: number;
  started_at: string;
  completed_at: string;
  task_run_id: string;
  fix_count: number | null;
  exit_check: { should_exit: boolean; reason: string };
}

interface LoopResult {
  loop_id: string;
  label: string | null;
  running: boolean;
  phase: string;
  current_iteration: number;
  max_iterations: number;
  workflow_id: string;
  target_runner_port: number;
  target_runner_id: string | null;
  error: string | null;
  iteration_results: IterationResult[];
}

interface MultiLoopResultsProps {
  loops: LoopResult[];
}

function StatusIcon({ phase }: { phase: string }) {
  if (phase === "complete") return <CheckCircle className="w-3.5 h-3.5 text-green-400" />;
  if (phase === "error") return <XCircle className="w-3.5 h-3.5 text-red-400" />;
  if (phase === "stopped") return <MinusCircle className="w-3.5 h-3.5 text-muted-foreground" />;
  return <AlertTriangle className="w-3.5 h-3.5 text-yellow-400" />;
}

export function MultiLoopResults({ loops }: MultiLoopResultsProps) {
  if (loops.length === 0) return null;

  const completedLoops = loops.filter((l) => l.phase === "complete");
  const errorLoops = loops.filter((l) => l.phase === "error");
  const totalIterations = loops.reduce((s, l) => s + l.current_iteration, 0);
  const totalFixes = loops.reduce(
    (s, l) =>
      s + l.iteration_results.reduce((fs, r) => fs + (r.fix_count ?? 0), 0),
    0,
  );

  // Calculate duration from first start to last completion
  const allStarts = loops
    .flatMap((l) => l.iteration_results.map((r) => r.started_at))
    .filter(Boolean)
    .sort();
  const allEnds = loops
    .flatMap((l) => l.iteration_results.map((r) => r.completed_at))
    .filter(Boolean)
    .sort();
  const startTime = allStarts[0] ? new Date(allStarts[0]) : null;
  const endTime = allEnds.length > 0 ? new Date(allEnds[allEnds.length - 1]) : null;
  const durationMs = startTime && endTime ? endTime.getTime() - startTime.getTime() : 0;
  const durationStr = durationMs > 0
    ? durationMs > 60000
      ? `${Math.round(durationMs / 60000)}m`
      : `${Math.round(durationMs / 1000)}s`
    : "--";

  return (
    <div className="border border-border rounded overflow-hidden">
      {/* Summary header */}
      <div className="px-3 py-2 border-b border-border bg-muted/30">
        <div className="flex items-center gap-4 text-xs">
          <span className="font-semibold">Results</span>
          <span className="text-green-400 font-mono">{completedLoops.length} passed</span>
          {errorLoops.length > 0 && (
            <span className="text-red-400 font-mono">{errorLoops.length} failed</span>
          )}
          <span className="text-muted-foreground">{totalIterations} iterations</span>
          <span className="text-muted-foreground">{totalFixes} fixes</span>
          <span className="text-muted-foreground ml-auto">{durationStr}</span>
        </div>
      </div>

      {/* Per-loop rows */}
      <div className="divide-y divide-border/50">
        {loops.map((loop) => {
          const fixes = loop.iteration_results.reduce(
            (s, r) => s + (r.fix_count ?? 0),
            0,
          );
          const lastResult = loop.iteration_results[loop.iteration_results.length - 1];
          const exitReason = lastResult?.exit_check.reason ?? "--";

          return (
            <div key={loop.loop_id} className="px-3 py-2 space-y-1">
              <div className="flex items-center gap-2 text-xs">
                <StatusIcon phase={loop.phase} />
                <span className="font-medium">{loop.label || loop.loop_id}</span>
                <span className="text-muted-foreground">:{loop.target_runner_port}</span>
                <span className="ml-auto font-mono text-muted-foreground">
                  {loop.current_iteration}/{loop.max_iterations} iterations
                </span>
                <span className={cn("font-mono", fixes === 0 ? "text-green-400" : "text-yellow-400")}>
                  {fixes} fixes
                </span>
              </div>
              {loop.error && (
                <p className="text-[0.7rem] text-red-400 pl-5 truncate" title={loop.error}>
                  {loop.error}
                </p>
              )}
              <p className="text-[0.7rem] text-muted-foreground pl-5 truncate" title={exitReason}>
                {exitReason}
              </p>
            </div>
          );
        })}
      </div>
    </div>
  );
}
