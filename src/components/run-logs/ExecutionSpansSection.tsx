import { useState, useEffect, useReducer } from "react";
import {
  Loader2,
  AlertCircle,
  ChevronDown,
  ChevronRight,
  CheckCircle,
  XCircle,
  Timer,
} from "lucide-react";
import { useRunSelection } from "../../contexts/RunSelectionContext";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { formatDurationMs, getSpanColor } from "./ai-data-viewer-utils";

export interface ExecutionSpan {
  id: number;
  execution_id: string | null;
  trace_id: string;
  span_id: string;
  parent_span_id: string | null;
  name: string;
  start_ts: string;
  end_ts: string | null;
  duration_ms: number | null;
  attributes: string | null;
  success: boolean;
  error: string | null;
  created_at: string;
}

interface SpansState {
  spans: ExecutionSpan[];
  allSpans: ExecutionSpan[];
  isLoading: boolean;
  error: string | null;
}

type SpansAction =
  | { type: "FETCH_START" }
  | { type: "FETCH_SUCCESS"; spans: ExecutionSpan[] }
  | { type: "FETCH_ALL_SPANS"; allSpans: ExecutionSpan[] }
  | { type: "FETCH_ERROR"; error: string }
  | { type: "RESET" };

function spansReducer(state: SpansState, action: SpansAction): SpansState {
  switch (action.type) {
    case "FETCH_START":
      return { ...state, isLoading: true, error: null };
    case "FETCH_SUCCESS":
      return { ...state, spans: action.spans, isLoading: false };
    case "FETCH_ALL_SPANS":
      return { ...state, allSpans: action.allSpans };
    case "FETCH_ERROR":
      return { ...state, error: action.error, isLoading: false };
    case "RESET":
      return { spans: [], allSpans: [], isLoading: false, error: null };
  }
}

const initialSpansState: SpansState = {
  spans: [],
  allSpans: [],
  isLoading: false,
  error: null,
};

function SpanDetailPanel({ span }: { span: ExecutionSpan }) {
  let attrs: Record<string, unknown> = {};
  try {
    if (span.attributes) attrs = JSON.parse(span.attributes);
  } catch {
    /* JSON parse may fail for invalid attributes */
  }

  return (
    <div className="px-3 pb-3 pl-10 space-y-1.5">
      <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
        <div>
          <span className="text-muted-foreground">Trace ID: </span>
          <span className="font-mono">{span.trace_id}</span>
        </div>
        <div>
          <span className="text-muted-foreground">Span ID: </span>
          <span className="font-mono">{span.span_id}</span>
        </div>
        {span.parent_span_id && (
          <div>
            <span className="text-muted-foreground">Parent: </span>
            <span className="font-mono">{span.parent_span_id}</span>
          </div>
        )}
        <div>
          <span className="text-muted-foreground">Start: </span>
          <span className="font-mono">{new Date(span.start_ts).toLocaleTimeString()}</span>
        </div>
        {span.end_ts && (
          <div>
            <span className="text-muted-foreground">End: </span>
            <span className="font-mono">{new Date(span.end_ts).toLocaleTimeString()}</span>
          </div>
        )}
      </div>
      {span.error && (
        <div className="text-xs text-destructive bg-destructive/10 rounded p-2 mt-1">
          {span.error}
        </div>
      )}
      {Object.keys(attrs).length > 0 && (
        <div className="mt-1">
          <div className="text-xs text-muted-foreground mb-1">Attributes</div>
          <pre className="text-xs bg-muted/50 rounded p-2 overflow-x-auto max-h-48">
            {JSON.stringify(attrs, null, 2)}
          </pre>
        </div>
      )}
    </div>
  );
}

function PhaseTimeline({ phaseSpans }: { phaseSpans: ExecutionSpan[] }) {
  const maxDuration = Math.max(...phaseSpans.map((s) => s.duration_ms ?? 0), 1);

  return (
    <div className="rounded-lg border border-border bg-card p-3">
      <h4 className="text-sm font-medium mb-2">Phase Timings</h4>
      <div className="space-y-1.5">
        {phaseSpans.map((span) => {
          const phaseName = span.name.replace("workflow.phase.", "");
          const duration = span.duration_ms ?? 0;
          const widthPct = Math.max((duration / maxDuration) * 100, 2);

          return (
            <div key={span.id} className="flex items-center gap-2 text-sm">
              <span className="w-24 text-muted-foreground truncate">{phaseName}</span>
              <div className="flex-1 h-5 bg-muted/50 rounded overflow-hidden">
                <div
                  className={`h-full rounded ${
                    span.success ? "bg-blue-500/60" : "bg-destructive/60"
                  }`}
                  style={{ width: `${widthPct}%` }}
                />
              </div>
              <span className="w-20 text-right text-xs font-mono text-muted-foreground">
                {formatDurationMs(duration)}
              </span>
              {!span.success && <XCircle className="w-3.5 h-3.5 text-destructive shrink-0" />}
            </div>
          );
        })}
      </div>
    </div>
  );
}

export function ExecutionSpansSection() {
  const { selectedRunId } = useRunSelection();
  const [state, dispatch] = useReducer(spansReducer, initialSpansState);
  const [expandedIds, setExpandedIds] = useState<Set<number>>(new Set());

  useEffect(() => {
    if (!selectedRunId) {
      dispatch({ type: "RESET" });
      return;
    }
    const controller = new AbortController();
    let cancelled = false;
    dispatch({ type: "FETCH_START" });

    (async () => {
      try {
        const res = await tracedFetch(
          `${getApiBase()}/execution-spans?execution_id=${encodeURIComponent(selectedRunId)}&limit=200`,
          { signal: controller.signal },
        );
        const data = await res.json();
        if (cancelled) return;
        const runSpans = data.spans ?? [];
        dispatch({ type: "FETCH_SUCCESS", spans: runSpans });

        if (runSpans.length === 0) {
          try {
            const allRes = await tracedFetch(`${getApiBase()}/execution-spans?limit=50`, {
              signal: controller.signal,
            });
            const allData = await allRes.json();
            if (!cancelled)
              dispatch({
                type: "FETCH_ALL_SPANS",
                allSpans: allData.spans ?? [],
              });
          } catch {
            // ignore - may be aborted
          }
        }
      } catch (err) {
        if (!cancelled) {
          dispatch({ type: "FETCH_ERROR", error: String(err) });
        }
      }
    })();

    return () => {
      cancelled = true;
      controller.abort();
    };
  }, [selectedRunId]);

  const showingAll = state.spans.length === 0 && state.allSpans.length > 0;
  const displaySpans = state.spans.length > 0 ? state.spans : state.allSpans;

  const toggleExpanded = (id: number) => {
    setExpandedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  if (!selectedRunId) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Timer className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">Select a task run to view execution spans</p>
      </div>
    );
  }

  if (state.isLoading) {
    return (
      <div className="flex items-center justify-center py-8">
        <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (state.error) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-destructive">
        <AlertCircle className="w-8 h-8 mb-3" />
        <p className="text-sm">Error loading spans: {state.error}</p>
      </div>
    );
  }

  const phaseSpans = displaySpans.filter((s) => s.name.startsWith("workflow.phase."));
  const aiSpans = displaySpans.filter((s) => s.name === "ai.session");
  const totalAiMs = aiSpans.reduce((sum, s) => sum + (s.duration_ms ?? 0), 0);
  const failedSpans = displaySpans.filter((s) => !s.success);
  const slowSpans = displaySpans.filter((s) => (s.duration_ms ?? 0) > 5000);

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h3 className="text-lg font-semibold">Execution Spans</h3>
        <span className="text-xs text-muted-foreground">
          {displaySpans.length} spans
          {showingAll ? " (all executions)" : ""}
        </span>
      </div>

      {displaySpans.length === 0 ? (
        <div className="text-center py-8 text-muted-foreground">
          <Timer className="w-8 h-8 mx-auto mb-3 opacity-50" />
          <p className="text-sm">No execution spans recorded</p>
          <p className="text-xs mt-1 opacity-70">
            Spans are captured when workflows execute instrumented code paths
          </p>
        </div>
      ) : (
        <>
          <div className="grid grid-cols-4 gap-3">
            <div className="rounded-lg border border-border bg-card p-3">
              <div className="text-xs text-muted-foreground mb-1">Phases</div>
              <div className="text-lg font-semibold">{phaseSpans.length}</div>
            </div>
            <div className="rounded-lg border border-border bg-card p-3">
              <div className="text-xs text-muted-foreground mb-1">AI Sessions</div>
              <div className="text-lg font-semibold">{aiSpans.length}</div>
              {aiSpans.length > 0 && (
                <div className="text-xs text-muted-foreground">
                  {formatDurationMs(totalAiMs)} total
                </div>
              )}
            </div>
            <div className="rounded-lg border border-border bg-card p-3">
              <div className="text-xs text-muted-foreground mb-1">Slow (&gt;5s)</div>
              <div className="text-lg font-semibold">{slowSpans.length}</div>
            </div>
            <div className="rounded-lg border border-border bg-card p-3">
              <div className="text-xs text-muted-foreground mb-1">Failed</div>
              <div
                className={`text-lg font-semibold ${failedSpans.length > 0 ? "text-destructive" : ""}`}
              >
                {failedSpans.length}
              </div>
            </div>
          </div>

          {phaseSpans.length > 0 && <PhaseTimeline phaseSpans={phaseSpans} />}

          <div className="rounded-lg border border-border bg-card">
            <h4 className="text-sm font-medium p-3 border-b border-border">All Spans</h4>
            <div className="divide-y divide-border max-h-[500px] overflow-y-auto">
              {displaySpans.map((span) => {
                const isExpanded = expandedIds.has(span.id);

                return (
                  <div key={span.id}>
                    <button
                      onClick={() => toggleExpanded(span.id)}
                      className="w-full flex items-center gap-2 px-3 py-2 text-sm hover:bg-muted/50 transition-colors"
                    >
                      {isExpanded ? (
                        <ChevronDown className="w-3.5 h-3.5 shrink-0 text-muted-foreground" />
                      ) : (
                        <ChevronRight className="w-3.5 h-3.5 shrink-0 text-muted-foreground" />
                      )}
                      {span.success ? (
                        <CheckCircle className="w-3.5 h-3.5 shrink-0 text-green-500" />
                      ) : (
                        <XCircle className="w-3.5 h-3.5 shrink-0 text-destructive" />
                      )}
                      <span className={`font-mono text-xs ${getSpanColor(span.name)}`}>
                        {span.name}
                      </span>
                      <span className="flex-1" />
                      {span.duration_ms != null && (
                        <span className="text-xs font-mono text-muted-foreground">
                          {formatDurationMs(span.duration_ms)}
                        </span>
                      )}
                    </button>
                    {isExpanded && <SpanDetailPanel span={span} />}
                  </div>
                );
              })}
            </div>
          </div>
        </>
      )}
    </div>
  );
}
