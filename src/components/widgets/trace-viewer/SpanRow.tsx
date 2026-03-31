import React from "react";
import type { TraceTreeNode, CriticalPathInfo } from "./types";
import { PHASE_COLORS } from "./types";
import { inferPhase, formatDuration } from "./trace-utils";

interface SpanRowProps {
  node: TraceTreeNode;
  traceStartMs: number;
  traceDurationMs: number;
  isSelected: boolean;
  onClick: () => void;
  criticalPath?: CriticalPathInfo | null;
  tokenHeat?: number;
  isHighlighted?: boolean;
}

export const SpanRow: React.FC<SpanRowProps> = React.memo(
  ({ node, traceStartMs, traceDurationMs, isSelected, onClick, criticalPath, tokenHeat, isHighlighted }) => {
    const { span, depth } = node;
    const phase = inferPhase(span.name);
    const colors = PHASE_COLORS[phase];
    const durationMs = span.duration_ms ?? 0;

    // Calculate bar position
    const leftPercent =
      traceDurationMs > 0
        ? ((new Date(span.start_ts).getTime() - traceStartMs) / traceDurationMs) * 100
        : 0;
    const widthPercent =
      traceDurationMs > 0
        ? Math.max((durationMs / traceDurationMs) * 100, 0.5) // Min 0.5% width
        : 100;

    // Short display name (strip qontinui. prefix)
    const displayName = span.name.replace(/^qontinui\./, "");

    // Critical path styling
    const onCritPath = criticalPath?.spanIds.has(span.span_id);
    const dimmed = criticalPath && !onCritPath;

    return (
      <div
        className={`flex items-center h-8 cursor-pointer hover:bg-zinc-800/50 border-b border-zinc-800/30 ${
          isSelected ? "bg-zinc-700/50" : ""
        } ${isHighlighted ? "ring-1 ring-blue-500 animate-pulse" : ""}`}
        style={{
          opacity: dimmed ? 0.4 : 1,
          ...(tokenHeat && tokenHeat > 0 ? { backgroundColor: `rgba(239, 68, 68, ${tokenHeat * 0.3})` } : {}),
        }}
        onClick={onClick}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            onClick();
          }
        }}
        role="button"
        tabIndex={0}
      >
        {/* Name column */}
        <div
          className="w-[240px] min-w-[240px] flex items-center gap-1.5 px-2 text-xs truncate"
          style={{ paddingLeft: `${depth * 20 + 8}px` }}
        >
          {!span.success && <span className="w-1.5 h-1.5 rounded-full bg-red-500 shrink-0" />}
          <span className={`truncate ${span.success ? "text-zinc-300" : "text-red-400"} ${onCritPath ? "font-medium" : ""}`}>
            {displayName}
          </span>
          <span className={`text-[10px] px-1 rounded ${colors.badge} ${colors.text} shrink-0`}>
            {phase}
          </span>
        </div>

        {/* Timeline bar column */}
        <div className="flex-1 relative h-full">
          <div
            className={`absolute top-1.5 h-5 rounded-xs ${colors.bar} ${
              !span.success ? "ring-1 ring-red-500/50" : ""
            } ${onCritPath ? "ring-1 ring-red-400/60 shadow-[0_0_6px_rgba(239,68,68,0.3)]" : ""}`}
            style={{
              left: `${leftPercent}%`,
              width: `${widthPercent}%`,
              minWidth: "2px",
            }}
          />
          {/* Duration label */}
          <span
            className="absolute top-1.5 text-[10px] text-zinc-400"
            style={{ left: `${leftPercent + widthPercent + 0.5}%` }}
          >
            {formatDuration(durationMs)}
          </span>
        </div>
      </div>
    );
  },
);

SpanRow.displayName = "SpanRow";
