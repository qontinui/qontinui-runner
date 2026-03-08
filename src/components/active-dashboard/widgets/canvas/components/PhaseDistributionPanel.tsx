/**
 * PhaseDistributionPanel Component
 *
 * Donut/ring chart showing time distribution across workflow phases.
 * Uses SVG for a clean, lightweight rendering.
 */

import { cn } from "../../../../../lib/utils";
import type { CanvasPanelComponentProps } from "./CanvasComponentRegistry";

interface Segment {
  phase: string;
  duration_ms: number;
  percentage: number;
  color?: string;
}

const PHASE_COLORS: Record<string, string> = {
  setup: "#60a5fa",
  verification: "#a78bfa",
  agentic: "#4ade80",
  completion: "#22d3ee",
};

const FALLBACK_COLORS = ["#f59e0b", "#ec4899", "#8b5cf6", "#14b8a6", "#f97316", "#6366f1"];

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  const seconds = ms / 1000;
  if (seconds < 60) return `${seconds.toFixed(1)}s`;
  const minutes = Math.floor(seconds / 60);
  const remainingSeconds = Math.round(seconds % 60);
  return `${minutes}m ${remainingSeconds}s`;
}

function getColor(segment: Segment, idx: number): string {
  if (segment.color) return segment.color;
  const phaseLower = segment.phase.toLowerCase();
  return PHASE_COLORS[phaseLower] ?? FALLBACK_COLORS[idx % FALLBACK_COLORS.length];
}

export function PhaseDistributionPanel({ data, size }: CanvasPanelComponentProps) {
  const segments = (data.segments as Segment[]) ?? [];
  const totalDuration = (data.total_duration_ms as number) ?? 0;
  const isCompact = size === "compact";

  if (segments.length === 0 || totalDuration <= 0) {
    return <p className="text-sm text-muted-foreground italic">No data</p>;
  }

  const ringSize = isCompact ? 80 : 100;
  const strokeWidth = isCompact ? 14 : 18;
  const radius = (ringSize - strokeWidth) / 2;
  const circumference = 2 * Math.PI * radius;
  const center = ringSize / 2;
  const fontSize = isCompact ? "text-[10px]" : "text-xs";

  // Build SVG arc segments
  let cumulativeOffset = 0;
  const arcs = segments.map((seg, idx) => {
    const pct = seg.percentage / 100;
    const dashLength = circumference * pct;
    const dashGap = circumference - dashLength;
    const offset = -circumference * cumulativeOffset + circumference * 0.25; // start at top
    cumulativeOffset += pct;

    return {
      dashArray: `${dashLength} ${dashGap}`,
      offset,
      color: getColor(seg, idx),
    };
  });

  return (
    <div className="flex items-start gap-4">
      {/* Donut chart */}
      <div className="flex-shrink-0" style={{ width: ringSize, height: ringSize }}>
        <svg viewBox={`0 0 ${ringSize} ${ringSize}`} width={ringSize} height={ringSize}>
          {/* Background ring */}
          <circle
            cx={center}
            cy={center}
            r={radius}
            fill="none"
            stroke="currentColor"
            className="text-muted/30"
            strokeWidth={strokeWidth}
          />
          {/* Segments */}
          {arcs.map((arc, idx) => (
            <circle
              key={idx}
              cx={center}
              cy={center}
              r={radius}
              fill="none"
              stroke={arc.color}
              strokeWidth={strokeWidth}
              strokeDasharray={arc.dashArray}
              strokeDashoffset={arc.offset}
              strokeLinecap="butt"
              className="transition-all"
            />
          ))}
          {/* Center label */}
          <text
            x={center}
            y={center}
            textAnchor="middle"
            dominantBaseline="central"
            className="fill-foreground"
            fontSize={isCompact ? 11 : 13}
            fontWeight={600}
          >
            {formatDuration(totalDuration)}
          </text>
        </svg>
      </div>

      {/* Legend */}
      <div className={cn("flex flex-col gap-1.5 min-w-0", fontSize)}>
        {segments.map((seg, idx) => (
          <div key={idx} className="flex items-center gap-1.5">
            <span
              className="inline-block h-2.5 w-2.5 rounded-sm flex-shrink-0"
              style={{ backgroundColor: getColor(seg, idx) }}
            />
            <span className="font-medium text-foreground truncate">{seg.phase}</span>
            <span className="text-muted-foreground font-mono">{seg.percentage.toFixed(0)}%</span>
            <span className="text-muted-foreground font-mono">
              ({formatDuration(seg.duration_ms)})
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}
