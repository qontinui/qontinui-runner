/**
 * IterationComparisonPanel Component
 *
 * Grouped bar chart showing verification pass/fail counts per iteration.
 * Each iteration gets a stacked horizontal bar.
 */

import { cn } from "@/lib/utils";
import type { CanvasPanelComponentProps } from "./CanvasComponentRegistry";

interface Iteration {
  iteration: number;
  passed: number;
  failed: number;
  total: number;
}

const PASS_COLOR = "#22c55e";
const FAIL_COLOR = "#ef4444";

export function IterationComparisonPanel({ data, size }: CanvasPanelComponentProps) {
  const iterations = (data.iterations as Iteration[]) ?? [];
  const isCompact = size === "compact";

  if (iterations.length === 0) {
    return <p className="text-sm text-muted-foreground italic">No data</p>;
  }

  const maxTotal = Math.max(...iterations.map((i) => i.total), 1);
  const barHeight = isCompact ? "h-4" : "h-5";
  const fontSize = isCompact ? "text-[10px]" : "text-xs";
  const labelWidth = isCompact ? 50 : 60;

  return (
    <div className="space-y-1.5">
      {iterations.map((iter) => {
        const passedPct = (iter.passed / maxTotal) * 100;
        const failedPct = (iter.failed / maxTotal) * 100;
        const allPassed = iter.failed === 0 && iter.passed > 0;

        return (
          <div key={iter.iteration} className="flex items-center gap-2">
            {/* Iteration label */}
            <div
              className={cn("flex-shrink-0 text-muted-foreground font-mono", fontSize)}
              style={{ width: `${labelWidth}px` }}
            >
              Iter {iter.iteration}
            </div>

            {/* Stacked bar */}
            <div className="flex-1 flex items-center gap-0">
              <div
                className={cn(
                  "flex rounded-sm overflow-hidden bg-muted",
                  barHeight,
                  allPassed && "ring-1 ring-green-500/30",
                )}
                style={{ width: "100%" }}
              >
                {iter.passed > 0 && (
                  <div
                    className={cn(barHeight, "transition-all")}
                    style={{
                      width: `${passedPct}%`,
                      backgroundColor: PASS_COLOR,
                    }}
                    title={`Passed: ${iter.passed}`}
                  />
                )}
                {iter.failed > 0 && (
                  <div
                    className={cn(barHeight, "transition-all")}
                    style={{
                      width: `${failedPct}%`,
                      backgroundColor: FAIL_COLOR,
                    }}
                    title={`Failed: ${iter.failed}`}
                  />
                )}
              </div>
            </div>

            {/* Count */}
            <div className={cn("flex-shrink-0 font-mono text-muted-foreground", fontSize)}>
              <span className="text-green-500">{iter.passed}</span>
              <span className="mx-0.5">/</span>
              <span className={iter.failed > 0 ? "text-red-500" : "text-muted-foreground"}>
                {iter.failed}
              </span>
            </div>
          </div>
        );
      })}

      {/* Improvement indicator */}
      {iterations.length >= 2 && (
        <div className={cn("pt-1 border-t border-border/50", fontSize)}>
          {(() => {
            const first = iterations[0];
            const last = iterations[iterations.length - 1];
            const delta = last.passed - first.passed;
            if (delta > 0) {
              return (
                <span className="text-green-500">+{delta} checks fixed since iteration 1</span>
              );
            } else if (delta < 0) {
              return <span className="text-red-500">{delta} regression since iteration 1</span>;
            }
            return <span className="text-muted-foreground">No change across iterations</span>;
          })()}
        </div>
      )}
    </div>
  );
}
