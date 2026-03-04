import React, { useMemo, useCallback } from "react";
import { FixedSizeList as List } from "react-window";
import type { TraceSpan, TraceTreeNode } from "./types";
import { formatDuration } from "./trace-utils";
import { SpanRow } from "./SpanRow";

interface TraceWaterfallProps {
  spans: TraceSpan[];
  /** Pre-filtered flat nodes from the parent widget. */
  filteredNodes: TraceTreeNode[];
  selectedSpanId: string | null;
  onSelectSpan: (span: TraceSpan) => void;
  height: number;
}

export const TraceWaterfall: React.FC<TraceWaterfallProps> = ({
  spans,
  filteredNodes,
  selectedSpanId,
  onSelectSpan,
  height,
}) => {
  // Compute trace time bounds (uses all spans for the full timeline, not just filtered)
  const { traceStartMs, traceDurationMs } = useMemo(() => {
    if (spans.length === 0) return { traceStartMs: 0, traceDurationMs: 0 };

    const starts = spans.map((s) => new Date(s.start_ts).getTime());
    const ends = spans
      .filter((s) => s.end_ts)
      .map((s) => new Date(s.end_ts!).getTime());

    const start = starts.reduce((a, b) => Math.min(a, b), Infinity);
    const end = ends.length > 0 ? ends.reduce((a, b) => Math.max(a, b), -Infinity) : start + 1000;

    return { traceStartMs: start, traceDurationMs: end - start };
  }, [spans]);

  const ROW_HEIGHT = 32;

  const Row = useCallback(
    ({ index, style }: { index: number; style: React.CSSProperties }) => {
      const node = filteredNodes[index];
      return (
        <div style={style}>
          <SpanRow
            node={node}
            traceStartMs={traceStartMs}
            traceDurationMs={traceDurationMs}
            isSelected={selectedSpanId === node.span.span_id}
            onClick={() => onSelectSpan(node.span)}
          />
        </div>
      );
    },
    [filteredNodes, traceStartMs, traceDurationMs, selectedSpanId, onSelectSpan]
  );

  if (filteredNodes.length === 0) {
    return (
      <div className="flex-1 flex items-center justify-center text-zinc-500 text-sm">
        No spans to display
      </div>
    );
  }

  // Timeline header
  const timeMarks = [0, 0.25, 0.5, 0.75, 1.0];

  return (
    <div className="flex-1 flex flex-col min-w-0">
      {/* Timeline header */}
      <div className="flex items-center h-6 border-b border-zinc-700 bg-zinc-900/50">
        <div className="w-[240px] min-w-[240px] px-2 text-[10px] text-zinc-500">
          Name
        </div>
        <div className="flex-1 relative">
          {timeMarks.map((pct) => (
            <span
              key={pct}
              className="absolute text-[10px] text-zinc-600"
              style={{ left: `${pct * 100}%`, transform: "translateX(-50%)" }}
            >
              {formatDuration(pct * traceDurationMs)}
            </span>
          ))}
        </div>
      </div>

      {/* Virtualized span list */}
      <List
        height={Math.max(height - 24, 100)}
        itemCount={filteredNodes.length}
        itemSize={ROW_HEIGHT}
        width="100%"
      >
        {Row}
      </List>
    </div>
  );
};
