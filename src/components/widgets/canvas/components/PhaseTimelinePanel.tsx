/**
 * PhaseTimelinePanel Component
 *
 * Horizontal segmented bar showing workflow phase progression
 * with proportional time sizing and step counts.
 */

import { cn } from "@/lib/utils";
import type { CanvasPanelComponentProps } from "./CanvasComponentRegistry";

interface Phase {
  name: string;
  duration_ms: number;
  status: "completed" | "running" | "pending" | "failed";
  step_count: number;
}

const STATUS_COLORS: Record<string, string> = {
  completed: "bg-green-500",
  running: "bg-blue-500 animate-pulse",
  pending: "bg-muted",
  failed: "bg-red-500",
};

const STATUS_TEXT_COLORS: Record<string, string> = {
  completed: "text-green-500",
  running: "text-blue-400",
  pending: "text-muted-foreground",
  failed: "text-red-500",
};

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  const seconds = ms / 1000;
  if (seconds < 60) return `${seconds.toFixed(1)}s`;
  const minutes = Math.floor(seconds / 60);
  const remainingSeconds = Math.round(seconds % 60);
  return `${minutes}m ${remainingSeconds}s`;
}

export function PhaseTimelinePanel({ data, size }: CanvasPanelComponentProps) {
  const phases = (data.phases as Phase[]) ?? [];
  const totalDuration = (data.total_duration_ms as number) ?? 0;
  const isCompact = size === "compact";

  if (phases.length === 0) {
    return <p className="text-sm text-muted-foreground italic">No data</p>;
  }

  return (
    <div className="space-y-3">
      {/* Segmented bar */}
      <div className={cn("flex rounded-md overflow-hidden", isCompact ? "h-6" : "h-8")}>
        {phases.map((phase, idx) => {
          const widthPct =
            totalDuration > 0
              ? Math.max(2, (phase.duration_ms / totalDuration) * 100)
              : 100 / phases.length;
          const statusColor = STATUS_COLORS[phase.status] ?? STATUS_COLORS.pending;
          const showLabel = widthPct > 12;

          return (
            <div
              key={idx}
              className={cn(
                "relative flex items-center justify-center transition-all",
                statusColor,
                idx > 0 && "border-l border-background/50",
              )}
              style={{ width: `${widthPct}%` }}
              title={`${phase.name}: ${formatDuration(phase.duration_ms)} (${phase.step_count} steps)`}
            >
              {showLabel && (
                <span
                  className={cn(
                    "font-medium text-white/90 truncate px-1",
                    isCompact ? "text-[9px]" : "text-[10px]",
                  )}
                >
                  {phase.name}
                </span>
              )}
            </div>
          );
        })}
      </div>

      {/* Phase details */}
      <div
        className={cn("flex flex-wrap gap-x-4 gap-y-1.5", isCompact ? "text-[10px]" : "text-xs")}
      >
        {phases.map((phase, idx) => (
          <div key={idx} className="flex items-center gap-1.5">
            <span
              className={cn(
                "inline-block h-2 w-2 rounded-full flex-shrink-0",
                STATUS_COLORS[phase.status]?.split(" ")[0] ?? "bg-muted",
              )}
            />
            <span className={cn("font-medium", STATUS_TEXT_COLORS[phase.status])}>
              {phase.name}
            </span>
            <span className="text-muted-foreground font-mono">
              {formatDuration(phase.duration_ms)}
            </span>
            <span className="text-muted-foreground">
              ({phase.step_count} {phase.step_count === 1 ? "step" : "steps"})
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}
