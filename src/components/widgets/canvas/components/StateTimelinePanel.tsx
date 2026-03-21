/**
 * StateTimelinePanel Component
 *
 * One horizontal bar per verification step, with colored segments per iteration
 * showing convergence. Similar to Grafana's state timeline visualization.
 */

import { useState } from "react";
import { cn } from "@/lib/utils";
import type { CanvasPanelComponentProps } from "./types";

interface IterationEntry {
  iteration: number;
  status: "pass" | "fail" | "skip" | "pending";
}

interface StepEntry {
  name: string;
  iterations: IterationEntry[];
}

const STATUS_COLORS: Record<string, string> = {
  pass: "#22c55e",
  fail: "#ef4444",
  skip: "#6b7280",
  pending: "#3b82f6",
};

const STATUS_LABELS: Record<string, string> = {
  pass: "Pass",
  fail: "Fail",
  skip: "Skip",
  pending: "Pending",
};

export function StateTimelinePanel({ data, size }: CanvasPanelComponentProps) {
  const steps = (data.steps as StepEntry[]) ?? [];
  const isCompact = size === "compact";
  const [tooltip, setTooltip] = useState<{
    text: string;
    x: number;
    y: number;
  } | null>(null);

  if (steps.length === 0) {
    return <p className="text-sm text-muted-foreground italic">No data</p>;
  }

  // Determine max iterations across all steps
  const maxIterations = steps.reduce((max, step) => Math.max(max, step.iterations.length), 0);

  if (maxIterations === 0) {
    return <p className="text-sm text-muted-foreground italic">No iterations</p>;
  }

  const iterationNumbers = Array.from({ length: maxIterations }, (_, i) => i + 1);
  const nameWidth = isCompact ? 100 : 120;
  const rowHeight = isCompact ? "h-4" : "h-5";
  const fontSize = isCompact ? "text-[10px]" : "text-xs";

  return (
    <div className="relative space-y-0.5">
      {/* Header row with iteration numbers */}
      <div className="flex gap-0" style={{ paddingLeft: `${nameWidth + 4}px` }}>
        {iterationNumbers.map((num) => (
          <div
            key={num}
            className={cn(
              "flex-1 text-center text-muted-foreground font-mono",
              isCompact ? "text-[9px]" : "text-[10px]",
            )}
          >
            {num}
          </div>
        ))}
      </div>

      {/* Step rows */}
      {steps.map((step, stepIdx) => {
        // Build a lookup from iteration number to status
        const iterationMap = new Map<number, string>();
        for (const iter of step.iterations) {
          iterationMap.set(iter.iteration, iter.status);
        }

        return (
          <div key={stepIdx} className="flex items-center gap-1">
            {/* Step name */}
            <div
              className={cn("truncate text-muted-foreground shrink-0", fontSize)}
              style={{ width: `${nameWidth}px` }}
              title={step.name}
            >
              {step.name}
            </div>

            {/* Iteration segments */}
            <div className={cn("flex flex-1 rounded-xs overflow-hidden", rowHeight)}>
              {iterationNumbers.map((num) => {
                const status = iterationMap.get(num) ?? "pending";
                const color = STATUS_COLORS[status] ?? STATUS_COLORS.pending;
                const label = STATUS_LABELS[status] ?? status;

                return (
                  <div
                    key={num}
                    className="flex-1 border-r border-background last:border-r-0 cursor-default"
                    style={{ backgroundColor: color }}
                    onMouseEnter={(e) => {
                      const rect = e.currentTarget.getBoundingClientRect();
                      setTooltip({
                        text: `${step.name}, Iteration ${num}: ${label}`,
                        x: rect.left + rect.width / 2,
                        y: rect.top,
                      });
                    }}
                    onMouseLeave={() => setTooltip(null)}
                  />
                );
              })}
            </div>
          </div>
        );
      })}

      {/* Tooltip */}
      {tooltip && (
        <div
          className="fixed z-50 px-2 py-1 rounded bg-popover text-popover-foreground text-[10px] shadow-md border pointer-events-none whitespace-nowrap"
          style={{
            left: `${tooltip.x}px`,
            top: `${tooltip.y - 28}px`,
            transform: "translateX(-50%)",
          }}
        >
          {tooltip.text}
        </div>
      )}
    </div>
  );
}
