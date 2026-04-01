import React, { useState, useRef, useMemo, useCallback, useEffect } from "react";
import type {
  TraceSpan,
  SpanFilter,
  TraceViewMode,
  CriticalPathInfo,
  TokenOverlayMode,
} from "./types";
import { useTraceViewerData } from "./useTraceViewerData";
import {
  buildTraceTree,
  flattenTree,
  filterSpans,
  computeInsights,
  computeCriticalPath,
  computeTokenInsights,
} from "./trace-utils";
import { TraceToolbar } from "./TraceToolbar";
import { TraceWaterfall } from "./TraceWaterfall";
import { FlameChart } from "./FlameChart";
import { TraceComparison } from "./TraceComparison";
import { TraceMinimap } from "./TraceMinimap";
import { SpanDetailPanel } from "./SpanDetailPanel";
import { PerformanceInsights } from "./PerformanceInsights";
import { AgentGraph } from "./AgentGraph";
import { ReplayController } from "./ReplayController";
import { ReplayPanel } from "./ReplayPanel";
import { useReplayData } from "./useReplayData";

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

const BASE_VIEW_OPTIONS: { value: TraceViewMode; label: string }[] = [
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
  const [minimapViewport, setMinimapViewport] = useState({ start: 0, end: 1 });
  const [scrollToFraction, setScrollToFraction] = useState<number | undefined>(undefined);
  const scrollSourceRef = useRef<"minimap" | "waterfall" | null>(null);

  const [tokenOverlay, setTokenOverlay] = useState<TokenOverlayMode>("none");
  const [replayMode, setReplayMode] = useState(false);
  const [replayStep, setReplayStep] = useState(0);

  // Comparison run overrides (user can change via dropdowns)
  const [compRunA, setCompRunA] = useState<string | null>(null);
  const [compRunB, setCompRunB] = useState<string | null>(null);
  const effectiveRunA = compRunA ?? baselineExecutionId;
  const effectiveRunB = compRunB ?? executionId;

  const insights = useMemo(() => computeInsights(spans), [spans]);
  const tokenInsights = useMemo(() => computeTokenInsights(spans), [spans]);

  // Replay data
  const { steps: replaySteps, totalSteps: replayTotalSteps } = useReplayData(spans);

  // Reset replay when view mode changes
  useEffect(() => {
    setReplayMode(false);
    setReplayStep(0);
  }, [viewMode]);

  // Check if there are agent spans for the Agent Graph tab
  const hasAgentSpans = useMemo(
    () => spans.some((s) => s.name.includes("agent.phase") || s.name.includes("agent.message")),
    [spans],
  );

  // Build view options dynamically
  const VIEW_OPTIONS = useMemo(() => {
    const opts = [...BASE_VIEW_OPTIONS];
    if (hasAgentSpans) {
      opts.push({ value: "agent-graph" as TraceViewMode, label: "Agent Graph" });
    }
    return opts;
  }, [hasAgentSpans]);

  // Build tree (shared across views)
  const tree = useMemo(() => buildTraceTree(spans), [spans]);

  // Full flattened tree (unfiltered — used by minimap)
  const flattenedNodes = useMemo(() => flattenTree(tree), [tree]);

  // Build tree, flatten, and filter — shared between toolbar count and waterfall
  const filteredNodes = useMemo(() => {
    return filterSpans(flattenedNodes, filter);
  }, [flattenedNodes, filter]);

  // Trace time bounds for minimap
  const traceDurationMs = useMemo(() => {
    if (spans.length === 0) return 0;
    const times = spans.filter((s) => s.start_ts).map((s) => new Date(s.start_ts).getTime());
    const endTimes = spans.filter((s) => s.end_ts).map((s) => new Date(s.end_ts!).getTime());
    const start = Math.min(...times);
    const end = Math.max(...endTimes, ...times);
    return end - start;
  }, [spans]);

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
    <div
      className="flex flex-col bg-zinc-900 rounded border border-zinc-700 overflow-hidden"
      data-tutorial-id="trace-viewer-widget"
    >
      {/* Toolbar with view toggle */}
      <div
        className="flex items-center gap-2 bg-zinc-900 border-b border-zinc-700"
        data-tutorial-id="trace-view-mode-bar"
      >
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
        {viewMode !== "comparison" && viewMode !== "agent-graph" && (
          <label
            className="flex items-center gap-1.5 text-[11px] text-zinc-500 cursor-pointer select-none"
            data-tutorial-id="trace-critical-path-toggle"
          >
            <input
              type="checkbox"
              checked={showCriticalPath}
              onChange={(e) => setShowCriticalPath(e.target.checked)}
              className="w-3 h-3 rounded border-zinc-600 bg-zinc-800 accent-red-500"
            />
            Critical Path
          </label>
        )}

        {/* Token overlay dropdown */}
        {viewMode !== "comparison" && viewMode !== "agent-graph" && (
          <select
            value={tokenOverlay}
            onChange={(e) => setTokenOverlay(e.target.value as TokenOverlayMode)}
            className="text-[11px] bg-zinc-800 border border-zinc-600 rounded px-1.5 py-0.5 text-zinc-400"
          >
            <option value="none">No token overlay</option>
            <option value="input">Input tokens</option>
            <option value="output">Output tokens</option>
            <option value="cost">Cost</option>
          </select>
        )}

        {/* Replay toggle */}
        {viewMode === "waterfall" && replayTotalSteps > 0 && (
          <button
            onClick={() => {
              setReplayMode((prev) => !prev);
              setReplayStep(0);
            }}
            className={`text-[11px] px-2 py-0.5 rounded ${
              replayMode
                ? "bg-blue-600 text-white"
                : "text-zinc-500 hover:text-zinc-300 hover:bg-zinc-800"
            }`}
          >
            {replayMode ? "Exit Replay" : "Replay"}
          </button>
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

      {/* Minimap overview (waterfall & flamechart, 10+ spans) */}
      {(viewMode === "waterfall" || viewMode === "flamechart") && flattenedNodes.length > 10 && (
        <TraceMinimap
          nodes={flattenedNodes}
          traceStartMs={0}
          traceDurationMs={traceDurationMs}
          viewportStart={minimapViewport.start}
          viewportEnd={minimapViewport.end}
          onViewportChange={(start, end) => {
            setMinimapViewport({ start, end });
            scrollSourceRef.current = "minimap";
            setScrollToFraction(start);
          }}
        />
      )}

      {/* Replay controller */}
      {replayMode && viewMode === "waterfall" && replayTotalSteps > 0 && (
        <ReplayController
          currentStep={replayStep}
          totalSteps={replayTotalSteps}
          onStepChange={setReplayStep}
          onClose={() => {
            setReplayMode(false);
            setReplayStep(0);
          }}
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
            tokenOverlay={tokenOverlay}
            tokenInsights={tokenInsights}
            highlightedSpanId={
              replayMode && replaySteps[replayStep] ? replaySteps[replayStep].spanId : null
            }
            onViewportChange={(start, end) => {
              if (scrollSourceRef.current !== "minimap") {
                setMinimapViewport({ start, end });
              }
              scrollSourceRef.current = null;
            }}
            scrollToFraction={scrollToFraction}
          />
        )}

        {viewMode === "flamechart" && (
          <FlameChart
            roots={tree}
            onSelectSpan={handleSelectSpan}
            selectedSpanId={selectedSpan?.span_id ?? null}
            criticalPath={criticalPath}
            height={height - 64}
            tokenOverlay={tokenOverlay}
            tokenInsights={tokenInsights}
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

        {viewMode === "agent-graph" && (
          <AgentGraph
            spans={spans}
            selectedSpanId={selectedSpan?.span_id}
            onSelectSpan={(span) => setSelectedSpan(span)}
            height={height - 64}
          />
        )}

        {/* Detail panel (shared, shown for waterfall & flamechart & agent-graph) */}
        {viewMode !== "comparison" && !replayMode && (
          <SpanDetailPanel
            span={selectedSpan}
            onClose={() => setSelectedSpan(null)}
            criticalPath={criticalPath}
            allSpans={spans}
          />
        )}

        {/* Replay panel (shown instead of SpanDetailPanel when in replay mode) */}
        {replayMode && replaySteps[replayStep] && (
          <ReplayPanel
            step={replaySteps[replayStep]}
            previousStep={replayStep > 0 ? replaySteps[replayStep - 1] : undefined}
          />
        )}
      </div>

      {/* Footer insights (not shown for comparison — it has its own summary) */}
      {viewMode !== "comparison" && (
        <PerformanceInsights
          insights={insights}
          criticalPath={criticalPath}
          tokenInsights={tokenInsights}
        />
      )}
    </div>
  );
};
