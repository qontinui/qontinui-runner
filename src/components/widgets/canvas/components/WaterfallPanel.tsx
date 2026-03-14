/**
 * WaterfallPanel Component
 *
 * Gantt/waterfall chart showing time-based execution of steps.
 * Similar to Buildkite's waterfall view.
 */

import { cn } from "@/lib/utils";
import type { CanvasPanelComponentProps } from "./CanvasComponentRegistry";

interface WaterfallEntry {
  name: string;
  start_ms: number;
  duration_ms: number;
  status?: "running" | "success" | "failed" | "skipped" | "pending";
  phase?: string;
}

const STATUS_COLORS: Record<string, string> = {
  success: "#22c55e",
  failed: "#ef4444",
  running: "#3b82f6",
  skipped: "#6b7280",
  pending: "#a3a3a3",
};

const PHASE_COLORS: Record<string, string> = {
  setup: "#60a5fa",
  verification: "#a78bfa",
  agentic: "#4ade80",
  completion: "#22d3ee",
};

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  const seconds = ms / 1000;
  if (seconds < 60) return `${seconds.toFixed(1)}s`;
  const minutes = Math.floor(seconds / 60);
  const remainingSeconds = Math.round(seconds % 60);
  return `${minutes}m ${remainingSeconds}s`;
}

function formatTimeMarker(ms: number): string {
  if (ms === 0) return "0s";
  const seconds = ms / 1000;
  if (seconds < 60) return `${Math.round(seconds)}s`;
  const minutes = Math.floor(seconds / 60);
  const remainingSeconds = Math.round(seconds % 60);
  if (remainingSeconds === 0) return `${minutes}m`;
  return `${minutes}m ${remainingSeconds}s`;
}

function getTimeMarkers(totalMs: number): number[] {
  if (totalMs <= 0) return [0];

  // Choose a nice interval based on total duration
  const intervals = [1000, 5000, 10000, 15000, 30000, 60000, 120000, 300000, 600000];
  let interval = intervals[0];
  for (const candidate of intervals) {
    if (totalMs / candidate <= 8) {
      interval = candidate;
      break;
    }
    interval = candidate;
  }

  const markers: number[] = [0];
  let current = interval;
  while (current < totalMs) {
    markers.push(current);
    current += interval;
  }
  return markers;
}

export function WaterfallPanel({ data, size }: CanvasPanelComponentProps) {
  const entries = (data.entries as WaterfallEntry[]) ?? [];
  const totalDuration = (data.total_duration_ms as number) ?? 0;
  const isCompact = size === "compact";

  if (entries.length === 0 || totalDuration <= 0) {
    return <p className="text-sm text-muted-foreground italic">No data</p>;
  }

  const nameWidth = isCompact ? 110 : 140;
  const barHeight = isCompact ? "h-4" : "h-5";
  const fontSize = isCompact ? "text-[10px]" : "text-xs";
  const timeMarkers = getTimeMarkers(totalDuration);

  return (
    <div className="space-y-1">
      {/* Time axis header */}
      <div className="relative" style={{ paddingLeft: `${nameWidth + 8}px` }}>
        <div className="relative h-4">
          {timeMarkers.map((ms) => {
            const leftPct = (ms / totalDuration) * 100;
            return (
              <span
                key={ms}
                className={cn(
                  "absolute text-muted-foreground font-mono",
                  isCompact ? "text-[9px]" : "text-[10px]",
                )}
                style={{
                  left: `${leftPct}%`,
                  transform: leftPct > 0 ? "translateX(-50%)" : undefined,
                }}
              >
                {formatTimeMarker(ms)}
              </span>
            );
          })}
        </div>
      </div>

      {/* Entries */}
      {entries.map((entry, idx) => {
        const leftPct = (entry.start_ms / totalDuration) * 100;
        const widthPct = Math.max(
          (4 / 600) * 100, // Minimum 4px assuming ~600px chart width
          (entry.duration_ms / totalDuration) * 100,
        );

        const barColor =
          (entry.status && STATUS_COLORS[entry.status]) ||
          (entry.phase && PHASE_COLORS[entry.phase]) ||
          STATUS_COLORS.pending;

        const isRunning = entry.status === "running";
        const durationText = formatDuration(entry.duration_ms);

        // Estimate if text fits inside bar (rough: 1 char ~6px, bar ~widthPct% of container)
        // We use 60px as threshold
        const barWidthApprox = (widthPct / 100) * 600;
        const showTextInside = barWidthApprox > 60;

        return (
          <div key={idx} className="flex items-center gap-2">
            {/* Step name */}
            <div
              className={cn("truncate text-muted-foreground flex-shrink-0", fontSize)}
              style={{ width: `${nameWidth}px` }}
              title={entry.name}
            >
              {entry.name}
            </div>

            {/* Bar area */}
            <div className="flex-1 relative">
              <div
                className={cn("rounded-sm relative", barHeight, isRunning && "animate-pulse")}
                style={{
                  marginLeft: `${leftPct}%`,
                  width: `${Math.min(widthPct, 100 - leftPct)}%`,
                  backgroundColor: barColor,
                }}
                title={`${entry.name}: ${durationText}${entry.status ? ` (${entry.status})` : ""}${entry.phase ? ` [${entry.phase}]` : ""}`}
              >
                {/* Duration text inside bar */}
                {showTextInside && (
                  <span
                    className={cn(
                      "absolute inset-0 flex items-center justify-center font-mono text-white/90",
                      isCompact ? "text-[9px]" : "text-[10px]",
                    )}
                  >
                    {durationText}
                  </span>
                )}
              </div>

              {/* Duration text outside bar (right side) */}
              {!showTextInside && (
                <span
                  className={cn(
                    "absolute top-0 font-mono text-muted-foreground flex items-center",
                    barHeight,
                    isCompact ? "text-[9px]" : "text-[10px]",
                  )}
                  style={{
                    left: `${Math.min(leftPct + widthPct + 0.5, 95)}%`,
                  }}
                >
                  {durationText}
                </span>
              )}
            </div>
          </div>
        );
      })}
    </div>
  );
}
