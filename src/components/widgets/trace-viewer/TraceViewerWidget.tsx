import React, { useState, useMemo, useCallback } from "react";
import type { TraceSpan, SpanFilter, TraceViewMode, CriticalPathInfo } from "./types";
import { useTraceViewerData } from "./useTraceViewerData";
import { buildTraceTree, flattenTree, filterSpans, computeInsights, computeCriticalPath } from "./trace-utils";
import { TraceToolbar } from "./TraceToolbar";
import { TraceWaterfall } from "./TraceWaterfall";
import { FlameChart } from "./FlameChart";
import { TraceComparison } from "./TraceComparison";
import { SpanDetailPanel } from "./SpanDetailPanel";
import { PerformanceInsights } from "./PerformanceInsights";

interface TraceViewerWidgetProps {
  executionId: string | null;
  /** Pre-fetched spans — when provided, skips internal data fetching. */
  spans?: TraceSpan[];
  isLoading?: boolean;
  error?: string | null;
  height?: number;
  /** Baseline execution ID for comparison view. */
  baselineExecutionId?: string | null;
  /** Available runs for comparison dropdowns. */
  availableRuns?: { id: string; label: string }[];
}

const DEFAULT_FILTER: SpanFilter = {
  nameSearch: "",
  minDurationMs: null,
  phase: "all",
  status: "all",
};

const VIEW_OPTIONS: { value: TraceViewMode; label: string }[] = [
  { value: "waterfall", label: "Waterfall" },
  { value: "flamechart", label: "Flamechart" },
  { value: "comparison", label: "Comparison" },
];

export const TraceViewerWidget: React.FC<TraceViewerWidgetProps> = ({
  executionId,
  spans: externalSpans,
  isLoading: externalIsLoading,
  error: externalError,
  height = 400,
  baselineExecutionId = null,
  availableRuns,
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
  const [viewMode, setViewMode] = useState<TraceViewMode>("waterfall");
  const [showCriticalPath, setShowCriticalPath] = useState(false);

  // Comparison run overrides (user can change via dropdowns)
  const [compRunA, setCompRunA] = useState<string | null>(null);
  const [compRunB, setCompRunB] = useState<string | null>(null);
  const effectiveRunA = compRunA ?? baselineExecutionId;
  const effectiveRunB = compRunB ?? executionId;

  const insights = useMemo(() => computeInsights(spans), [spans]);

  // Build tree (shared across views)
  const tree = useMemo(() => buildTraceTree(spans), [spans]);

  // Build tree, flatten, and filter — shared between toolbar count and waterfall
  const filteredNodes = useMemo(() => {
    const flat = flattenTree(tree);
    return filterSpans(flat, filter);
  }, [tree, filter]);

  // Critical path (computed on demand)
  const criticalPath: CriticalPathInfo | null = useMemo(
    () => (showCriticalPath ? computeCriticalPath(tree) : null),
    [tree, showCriticalPath],
  );

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
    <div className="flex flex-col bg-zinc-900 rounded border border-zinc-700 overflow-hidden" data-tutorial-id="trace-viewer-widget">
      {/* Toolbar with view toggle */}
      <div className="flex items-center gap-2 bg-zinc-900 border-b border-zinc-700" data-tutorial-id="trace-view-mode-bar">
        {/* View mode tabs */}
        <div className="flex items-center gap-0.5 px-3 py-1.5" data-tutorial-id="trace-view-tabs">
          {VIEW_OPTIONS.map((opt) => (
            <button
              key={opt.value}
              onClick={() => setViewMode(opt.value)}
              className={`px-2.5 py-1 text-xs rounded transition-colors ${
                viewMode === opt.value
                  ? "bg-zinc-700 text-zinc-200"
                  : "text-zinc-500 hover:text-zinc-300 hover:bg-zinc-800"
              }`}
            >
              {opt.label}
            </button>
          ))}
        </div>

        {/* Critical path toggle (waterfall & flamechart only) */}
        {viewMode !== "comparison" && (
          <label className="flex items-center gap-1.5 text-[11px] text-zinc-500 cursor-pointer select-none" data-tutorial-id="trace-critical-path-toggle">
            <input
              type="checkbox"
              checked={showCriticalPath}
              onChange={(e) => setShowCriticalPath(e.target.checked)}
              className="w-3 h-3 rounded border-zinc-600 bg-zinc-800 accent-red-500"
            />
            Critical Path
          </label>
        )}

        <div className="flex-1" />
      </div>

      {/* Filter toolbar (not shown for comparison) */}
      {viewMode !== "comparison" && (
        <TraceToolbar
          filter={filter}
          onFilterChange={setFilter}
          spanCount={spans.length}
          filteredCount={filteredNodes.length}
        />
      )}

      {/* Main content area */}
      <div className="flex flex-1 min-h-0" data-tutorial-id="trace-main-content">
        {viewMode === "waterfall" && (
          <TraceWaterfall
            spans={spans}
            filteredNodes={filteredNodes}
            selectedSpanId={selectedSpan?.span_id ?? null}
            onSelectSpan={handleSelectSpan}
            height={height - 64}
            criticalPath={criticalPath}
          />
        )}

        {viewMode === "flamechart" && (
          <FlameChart
            roots={tree}
            onSelectSpan={handleSelectSpan}
            selectedSpanId={selectedSpan?.span_id ?? null}
            criticalPath={criticalPath}
            height={height - 64}
          />
        )}

        {viewMode === "comparison" && (
          <TraceComparison
            currentExecutionId={effectiveRunB}
            baselineExecutionId={effectiveRunA}
            onSelectRunA={setCompRunA}
            onSelectRunB={setCompRunB}
            availableRuns={availableRuns}
            height={height - 64}
          />
        )}

        {/* Detail panel (shared, shown for waterfall & flamechart) */}
        {viewMode !== "comparison" && (
          <SpanDetailPanel
            span={selectedSpan}
            onClose={() => setSelectedSpan(null)}
            criticalPath={criticalPath}
          />
        )}
      </div>

      {/* Footer insights (not shown for comparison — it has its own summary) */}
      {viewMode !== "comparison" && (
        <PerformanceInsights insights={insights} criticalPath={criticalPath} />
      )}
    </div>
  );
};
