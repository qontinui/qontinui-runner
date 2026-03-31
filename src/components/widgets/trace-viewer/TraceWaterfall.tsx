import React, { useMemo, useCallback, useEffect } from "react";
import { List, useListRef, type RowComponentProps } from "react-window";
import type { TraceSpan, TraceTreeNode, CriticalPathInfo, TokenOverlayMode } from "./types";
import { formatDuration, getTokenHeat, computeTokenInsights } from "./trace-utils";
import { SpanRow } from "./SpanRow";

interface TraceWaterfallProps {
  spans: TraceSpan[];
  /** Pre-filtered flat nodes from the parent widget. */
  filteredNodes: TraceTreeNode[];
  selectedSpanId: string | null;
  onSelectSpan: (span: TraceSpan) => void;
  height: number;
  criticalPath?: CriticalPathInfo | null;
  tokenOverlay?: TokenOverlayMode;
  tokenInsights?: ReturnType<typeof computeTokenInsights>;
  highlightedSpanId?: string | null;
  /** Reports visible viewport as fractions (0-1) when the waterfall scrolls. */
  onViewportChange?: (start: number, end: number) => void;
  /** Fraction (0-1) to scroll the waterfall to. */
  scrollToFraction?: number;
}

interface TraceRowProps {
  filteredNodes: TraceTreeNode[];
  traceStartMs: number;
  traceDurationMs: number;
  selectedSpanId: string | null;
  onSelectSpan: (span: TraceSpan) => void;
  criticalPath?: CriticalPathInfo | null;
  tokenOverlay?: TokenOverlayMode;
  tokenInsights?: ReturnType<typeof computeTokenInsights>;
  highlightedSpanId?: string | null;
}

function TraceRow({
  index,
  style,
  filteredNodes,
  traceStartMs,
  traceDurationMs,
  selectedSpanId,
  onSelectSpan,
  criticalPath,
  tokenOverlay,
  tokenInsights,
  highlightedSpanId,
}: RowComponentProps<TraceRowProps>) {
  const node = filteredNodes[index];
  const heat = tokenOverlay && tokenOverlay !== "none" && tokenInsights
    ? getTokenHeat(node.span, tokenOverlay,
        tokenOverlay === "input" ? tokenInsights.maxInputTokens
          : tokenOverlay === "output" ? tokenInsights.maxOutputTokens
          : tokenInsights.maxCostCents)
    : 0;
  return (
    <div style={style}>
      <SpanRow
        node={node}
        traceStartMs={traceStartMs}
        traceDurationMs={traceDurationMs}
        isSelected={selectedSpanId === node.span.span_id}
        onClick={() => onSelectSpan(node.span)}
        criticalPath={criticalPath}
        tokenHeat={heat}
        isHighlighted={highlightedSpanId === node.span.span_id}
      />
    </div>
  );
}

export const TraceWaterfall: React.FC<TraceWaterfallProps> = ({
  spans,
  filteredNodes,
  selectedSpanId,
  onSelectSpan,
  height,
  criticalPath,
  tokenOverlay,
  tokenInsights,
  highlightedSpanId,
  onViewportChange,
  scrollToFraction,
}) => {
  const listRef = useListRef(null);
  // Compute trace time bounds (uses all spans for the full timeline, not just filtered)
  const { traceStartMs, traceDurationMs } = useMemo(() => {
    if (spans.length === 0) return { traceStartMs: 0, traceDurationMs: 0 };

    const starts = spans.map((s) => new Date(s.start_ts).getTime());
    const ends = spans.filter((s) => s.end_ts).map((s) => new Date(s.end_ts!).getTime());

    const start = starts.reduce((a, b) => Math.min(a, b), Infinity);
    const end = ends.length > 0 ? ends.reduce((a, b) => Math.max(a, b), -Infinity) : start + 1000;

    return { traceStartMs: start, traceDurationMs: end - start };
  }, [spans]);

  const ROW_HEIGHT = 32;

  // Compute critical path connector line segments (row index pairs)
  // Must be before any early returns to satisfy Rules of Hooks.
  const criticalPathSegments = useMemo(() => {
    if (!criticalPath || criticalPath.spanIds.size === 0) return [];

    // Find row indices of critical path spans in display order
    const critRows: number[] = [];
    for (let i = 0; i < filteredNodes.length; i++) {
      if (criticalPath.spanIds.has(filteredNodes[i].span.span_id)) {
        critRows.push(i);
      }
    }

    // Build consecutive pairs
    const segments: { fromRow: number; toRow: number; fromSpan: TraceSpan; toSpan: TraceSpan }[] = [];
    for (let i = 0; i < critRows.length - 1; i++) {
      segments.push({
        fromRow: critRows[i],
        toRow: critRows[i + 1],
        fromSpan: filteredNodes[critRows[i]].span,
        toSpan: filteredNodes[critRows[i + 1]].span,
      });
    }
    return segments;
  }, [criticalPath, filteredNodes]);

  const listHeight = Math.max(height - 24, 100);

  // Report viewport position when visible rows change
  const handleRowsRendered = useCallback(
    (_visibleRows: { startIndex: number; stopIndex: number }, allRows: { startIndex: number; stopIndex: number }) => {
      if (!onViewportChange || filteredNodes.length === 0) return;
      const start = allRows.startIndex / filteredNodes.length;
      const end = Math.min(1, (allRows.stopIndex + 1) / filteredNodes.length);
      onViewportChange(start, end);
    },
    [onViewportChange, filteredNodes.length],
  );

  // Scroll to a specific fraction when requested by the minimap
  useEffect(() => {
    if (scrollToFraction == null || filteredNodes.length === 0) return;
    const ref = listRef as React.RefObject<{ scrollToRow: (config: { index: number; align?: string }) => void } | null>;
    if (!ref?.current) return;
    const targetIndex = Math.round(scrollToFraction * filteredNodes.length);
    const clampedIndex = Math.max(0, Math.min(filteredNodes.length - 1, targetIndex));
    ref.current.scrollToRow({ index: clampedIndex, align: "start" });
  }, [scrollToFraction, filteredNodes.length, listRef]);

  // Timeline header
  const timeMarks = [0, 0.25, 0.5, 0.75, 1.0];

  if (filteredNodes.length === 0) {
    return (
      <div className="flex-1 flex items-center justify-center text-zinc-500 text-sm">
        No spans to display
      </div>
    );
  }

  return (
    <div className="flex-1 flex flex-col min-w-0">
      {/* Timeline header */}
      <div className="flex items-center h-6 border-b border-zinc-700 bg-zinc-900/50">
        <div className="w-[240px] min-w-[240px] px-2 text-[10px] text-zinc-500">Name</div>
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

      {/* Virtualized span list with critical path overlay */}
      <div className="relative" style={{ height: listHeight }}>
        <List
          listRef={listRef}
          onRowsRendered={handleRowsRendered}
          rowCount={filteredNodes.length}
          rowHeight={ROW_HEIGHT}
          rowComponent={TraceRow}
          rowProps={{
            filteredNodes,
            traceStartMs,
            traceDurationMs,
            selectedSpanId,
            onSelectSpan,
            criticalPath,
            tokenOverlay,
            tokenInsights,
            highlightedSpanId,
          }}
          style={{ width: "100%", height: listHeight }}
        />

        {/* Critical path connector lines (SVG overlay) */}
        {criticalPathSegments.length > 0 && (
          <svg
            className="absolute top-0 left-[240px] pointer-events-none"
            style={{ width: "calc(100% - 240px)", height: listHeight }}
          >
            {criticalPathSegments.map((seg, i) => {
              // Center of each span's timeline bar
              const fromDur = seg.fromSpan.duration_ms ?? 0;
              const toDur = seg.toSpan.duration_ms ?? 0;
              const fromCenterPct =
                traceDurationMs > 0
                  ? ((new Date(seg.fromSpan.start_ts).getTime() - traceStartMs + fromDur / 2) /
                      traceDurationMs) *
                    100
                  : 50;
              const toCenterPct =
                traceDurationMs > 0
                  ? ((new Date(seg.toSpan.start_ts).getTime() - traceStartMs + toDur / 2) /
                      traceDurationMs) *
                    100
                  : 50;

              const y1 = seg.fromRow * ROW_HEIGHT + ROW_HEIGHT / 2;
              const y2 = seg.toRow * ROW_HEIGHT + ROW_HEIGHT / 2;

              return (
                <line
                  key={i}
                  x1={`${fromCenterPct}%`}
                  y1={y1}
                  x2={`${toCenterPct}%`}
                  y2={y2}
                  stroke="#ef4444"
                  strokeWidth={1.5}
                  strokeOpacity={0.5}
                  strokeDasharray="4 3"
                />
              );
            })}
          </svg>
        )}
      </div>
    </div>
  );
};
