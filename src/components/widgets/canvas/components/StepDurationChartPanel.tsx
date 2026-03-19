/**
 * StepDurationChartPanel Component
 *
 * Horizontal bar chart of steps ranked by duration.
 * Shows the longest-running steps with status-colored bars.
 */

import { cn } from "@/lib/utils";
import type { CanvasPanelComponentProps } from "./types";

interface StepDuration {
  name: string;
  duration_ms: number;
  status: "success" | "failed" | "running" | "skipped";
  phase?: string;
}

const STATUS_COLORS: Record<string, string> = {
  success: "#22c55e",
  failed: "#ef4444",
  running: "#3b82f6",
  skipped: "#6b7280",
};

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  const seconds = ms / 1000;
  if (seconds < 60) return `${seconds.toFixed(1)}s`;
  const minutes = Math.floor(seconds / 60);
  const remainingSeconds = Math.round(seconds % 60);
  return `${minutes}m ${remainingSeconds}s`;
}

export function StepDurationChartPanel({ data, size }: CanvasPanelComponentProps) {
  const steps = (data.steps as StepDuration[]) ?? [];
  const maxDuration = (data.max_duration_ms as number) ?? 0;
  const isCompact = size === "compact";

  if (steps.length === 0 || maxDuration <= 0) {
    return <p className="text-sm text-muted-foreground italic">No data</p>;
  }

  const nameWidth = isCompact ? 100 : 130;
  const barHeight = isCompact ? "h-3.5" : "h-4";
  const fontSize = isCompact ? "text-[10px]" : "text-xs";

  return (
    <div className="space-y-1">
      {steps.map((step, idx) => {
        const widthPct = Math.max(1, (step.duration_ms / maxDuration) * 100);
        const barColor = STATUS_COLORS[step.status] ?? STATUS_COLORS.skipped;
        const isRunning = step.status === "running";

        return (
          <div key={idx} className="flex items-center gap-2">
            {/* Step name */}
            <div
              className={cn("truncate text-muted-foreground flex-shrink-0", fontSize)}
              style={{ width: `${nameWidth}px` }}
              title={step.name}
            >
              {step.name}
            </div>

            {/* Bar */}
            <div className="flex-1 flex items-center gap-1.5">
              <div
                className={cn("rounded-sm transition-all", barHeight, isRunning && "animate-pulse")}
                style={{
                  width: `${widthPct}%`,
                  backgroundColor: barColor,
                }}
                title={`${step.name}: ${formatDuration(step.duration_ms)} (${step.status})${step.phase ? ` [${step.phase}]` : ""}`}
              />
              <span className={cn("flex-shrink-0 font-mono text-muted-foreground", fontSize)}>
                {formatDuration(step.duration_ms)}
              </span>
            </div>
          </div>
        );
      })}
    </div>
  );
}
