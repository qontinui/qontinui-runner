import React, { useState, useMemo, useCallback } from "react";
import type { TraceSpan, SpanFilter } from "./types";
import { useTraceViewerData } from "./useTraceViewerData";
import { buildTraceTree, flattenTree, filterSpans, computeInsights } from "./trace-utils";
import { TraceToolbar } from "./TraceToolbar";
import { TraceWaterfall } from "./TraceWaterfall";
import { SpanDetailPanel } from "./SpanDetailPanel";
import { PerformanceInsights } from "./PerformanceInsights";

interface TraceViewerWidgetProps {
  executionId: string | null;
  /** Pre-fetched spans — when provided, skips internal data fetching. */
  spans?: TraceSpan[];
  isLoading?: boolean;
  error?: string | null;
  height?: number;
}

const DEFAULT_FILTER: SpanFilter = {
  nameSearch: "",
  minDurationMs: null,
  phase: "all",
  status: "all",
};

export const TraceViewerWidget: React.FC<TraceViewerWidgetProps> = ({
  executionId,
  spans: externalSpans,
  isLoading: externalIsLoading,
  error: externalError,
  height = 400,
}) => {
  // Only fetch internally when no external spans are provided
  const internalQuery = useTraceViewerData(externalSpans !== undefined ? null : executionId);

  const spans = useMemo(
    () => externalSpans ?? internalQuery.data ?? [],
    [externalSpans, internalQuery.data],
  );
  const isLoading = externalIsLoading ?? internalQuery.isLoading;
  const error = externalError ?? (internalQuery.error ? String(internalQuery.error) : null);

  const [filter, setFilter] = useState<SpanFilter>(DEFAULT_FILTER);
  const [selectedSpan, setSelectedSpan] = useState<TraceSpan | null>(null);

  const insights = useMemo(() => computeInsights(spans), [spans]);

  // Build tree, flatten, and filter — shared between toolbar count and waterfall
  const filteredNodes = useMemo(() => {
    const tree = buildTraceTree(spans);
    const flat = flattenTree(tree);
    return filterSpans(flat, filter);
  }, [spans, filter]);

  const handleSelectSpan = useCallback((span: TraceSpan) => {
    setSelectedSpan((prev) => (prev?.span_id === span.span_id ? null : span));
  }, []);

  if (!executionId) {
    return (
      <div className="flex items-center justify-center h-40 text-zinc-500 text-sm">
        Select a task run to view traces
      </div>
    );
  }

  if (isLoading && spans.length === 0) {
    return (
      <div className="flex items-center justify-center h-40 text-zinc-500 text-sm">
        Loading trace data...
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex items-center justify-center h-40 text-red-400 text-sm">
        Failed to load traces: {error}
      </div>
    );
  }

  return (
    <div className="flex flex-col bg-zinc-900 rounded border border-zinc-700 overflow-hidden">
      <TraceToolbar
        filter={filter}
        onFilterChange={setFilter}
        spanCount={spans.length}
        filteredCount={filteredNodes.length}
      />

      <div className="flex flex-1 min-h-0">
        <TraceWaterfall
          spans={spans}
          filteredNodes={filteredNodes}
          selectedSpanId={selectedSpan?.span_id ?? null}
          onSelectSpan={handleSelectSpan}
          height={height - 64}
        />
        <SpanDetailPanel span={selectedSpan} onClose={() => setSelectedSpan(null)} />
      </div>

      <PerformanceInsights insights={insights} />
    </div>
  );
};
