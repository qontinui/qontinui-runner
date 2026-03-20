/**
 * ProgressChartPanel Component
 *
 * Renders a horizontal stacked progress bar with labeled segments.
 */

import { cn } from "@/lib/utils";
import type { CanvasPanelComponentProps } from "./types";

interface Segment {
  label: string;
  value: number;
  color?: string;
}

const DEFAULT_COLORS = [
  "bg-blue-500",
  "bg-green-500",
  "bg-yellow-500",
  "bg-purple-500",
  "bg-cyan-500",
  "bg-rose-500",
  "bg-orange-500",
  "bg-indigo-500",
];

export function ProgressChartPanel({ data, size }: CanvasPanelComponentProps) {
  const segments = (data.segments as Segment[]) ?? [];
  const total = (data.total as number) ?? segments.reduce((sum, s) => sum + s.value, 0);
  const isCompact = size === "compact";

  if (segments.length === 0 || total === 0) {
    return <p className="text-sm text-muted-foreground italic">No data</p>;
  }

  return (
    <div className="space-y-2">
      {/* Progress bar */}
      <div
        className={cn(
          "w-full rounded-full bg-muted overflow-hidden flex",
          isCompact ? "h-3" : "h-5",
        )}
      >
        {segments.map((segment, idx) => {
          const pct = Math.max(0, Math.min(100, (segment.value / total) * 100));
          if (pct === 0) return null;

          const bgClass = segment.color ?? DEFAULT_COLORS[idx % DEFAULT_COLORS.length];
          const isCustomColor = segment.color && !segment.color.startsWith("bg-");

          return (
            <div
              key={`bar-${segment.label}`}
              className={cn("h-full transition-all", !isCustomColor && bgClass)}
              style={{
                width: `${pct}%`,
                ...(isCustomColor ? { backgroundColor: segment.color } : {}),
              }}
              title={`${segment.label}: ${segment.value} (${pct.toFixed(1)}%)`}
            />
          );
        })}
      </div>

      {/* Legend */}
      <div className={cn("flex flex-wrap gap-x-4 gap-y-1", isCompact ? "text-[10px]" : "text-xs")}>
        {segments.map((segment, idx) => {
          const bgClass = segment.color ?? DEFAULT_COLORS[idx % DEFAULT_COLORS.length];
          const isCustomColor = segment.color && !segment.color.startsWith("bg-");

          return (
            <div key={`legend-${segment.label}`} className="flex items-center gap-1.5">
              <div
                className={cn("h-2.5 w-2.5 rounded-sm flex-shrink-0", !isCustomColor && bgClass)}
                style={isCustomColor ? { backgroundColor: segment.color } : undefined}
              />
              <span className="text-muted-foreground">{segment.label}</span>
              <span className="font-mono text-foreground">
                {segment.value}
                <span className="text-muted-foreground/60 ml-0.5">
                  ({((segment.value / total) * 100).toFixed(0)}%)
                </span>
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}
