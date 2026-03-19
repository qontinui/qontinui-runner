/**
 * SummaryStatsPanel Component
 *
 * Renders a compact horizontal stacked bar showing pass/fail/skip ratios
 * with counts, similar to GitHub's test summary bars.
 */

import { cn } from "@/lib/utils";
import type { CanvasPanelComponentProps } from "./types";

export function SummaryStatsPanel({ data, size }: CanvasPanelComponentProps) {
  const total = (data.total as number) ?? 0;
  const passed = (data.passed as number) ?? 0;
  const failed = (data.failed as number) ?? 0;
  const skipped = (data.skipped as number) ?? 0;
  const label = (data.label as string) ?? "";
  const isCompact = size === "compact";

  if (total === 0) {
    return <p className="text-sm text-muted-foreground italic">No data</p>;
  }

  const passedPct = (passed / total) * 100;
  const failedPct = (failed / total) * 100;
  const skippedPct = (skipped / total) * 100;
  const overallPct = Math.round(passedPct);
  const allPassed = passed === total;

  return (
    <div className="space-y-2">
      {/* Label */}
      {label && (
        <p className={cn("font-medium text-foreground", isCompact ? "text-xs" : "text-sm")}>
          {label}
        </p>
      )}

      {/* Stacked bar */}
      <div className="flex items-center gap-2">
        <div
          className={cn(
            "flex-1 rounded-full overflow-hidden flex bg-muted",
            isCompact ? "h-3" : "h-4",
            allPassed && "ring-1 ring-green-500/40 shadow-[0_0_6px_rgba(34,197,94,0.25)]",
          )}
        >
          {passedPct > 0 && (
            <div
              className="h-full bg-green-500 transition-all"
              style={{ width: `${passedPct}%` }}
              title={`Passed: ${passed} (${passedPct.toFixed(1)}%)`}
            />
          )}
          {failedPct > 0 && (
            <div
              className="h-full bg-red-500 transition-all"
              style={{ width: `${failedPct}%` }}
              title={`Failed: ${failed} (${failedPct.toFixed(1)}%)`}
            />
          )}
          {skippedPct > 0 && (
            <div
              className="h-full bg-gray-400 transition-all"
              style={{ width: `${skippedPct}%` }}
              title={`Skipped: ${skipped} (${skippedPct.toFixed(1)}%)`}
            />
          )}
        </div>

        {/* Percentage */}
        <span
          className={cn(
            "font-mono flex-shrink-0",
            isCompact ? "text-[10px]" : "text-xs",
            allPassed ? "text-green-500" : "text-muted-foreground",
          )}
        >
          {overallPct}%
        </span>
      </div>

      {/* Counts with colored dots */}
      <div className={cn("flex flex-wrap gap-x-4 gap-y-1", isCompact ? "text-[10px]" : "text-xs")}>
        <span className="flex items-center gap-1.5">
          <span className="inline-block h-2 w-2 rounded-full bg-green-500 flex-shrink-0" />
          <span className="text-muted-foreground">
            <span className="font-mono text-foreground">{passed}</span> passed
          </span>
        </span>
        <span className="flex items-center gap-1.5">
          <span className="inline-block h-2 w-2 rounded-full bg-red-500 flex-shrink-0" />
          <span className="text-muted-foreground">
            <span className="font-mono text-foreground">{failed}</span> failed
          </span>
        </span>
        {skipped > 0 && (
          <span className="flex items-center gap-1.5">
            <span className="inline-block h-2 w-2 rounded-full bg-gray-400 flex-shrink-0" />
            <span className="text-muted-foreground">
              <span className="font-mono text-foreground">{skipped}</span> skipped
            </span>
          </span>
        )}
      </div>
    </div>
  );
}
