/** Raw span data from the execution_spans API. */
export interface TraceSpan {
  id: number;
  execution_id: string | null;
  trace_id: string;
  span_id: string;
  parent_span_id: string | null;
  name: string;
  target: string | null;
  level: string | null;
  start_ts: string;
  end_ts: string | null;
  duration_ms: number | null;
  attributes: Record<string, unknown>;
  success: boolean;
  error: string | null;
}

/** A span with computed tree information. */
export interface TraceTreeNode {
  span: TraceSpan;
  children: TraceTreeNode[];
  depth: number;
  /** Absolute start offset in ms from trace start. */
  offsetMs: number;
}

/** Workflow phase type. */
export type WorkflowPhase =
  | "setup"
  | "verification"
  | "agentic"
  | "completion"
  | "ai"
  | "python"
  | "unknown";

/** Phase color configuration. */
export interface PhaseColors {
  bar: string;
  badge: string;
  text: string;
}

/** Map of phases to their Tailwind color classes. */
export const PHASE_COLORS: Record<WorkflowPhase, PhaseColors> = {
  setup: { bar: "bg-blue-500/60", badge: "bg-blue-500/20", text: "text-blue-400" },
  verification: { bar: "bg-amber-500/60", badge: "bg-amber-500/20", text: "text-amber-400" },
  agentic: { bar: "bg-purple-500/60", badge: "bg-purple-500/20", text: "text-purple-400" },
  completion: { bar: "bg-emerald-500/60", badge: "bg-emerald-500/20", text: "text-emerald-400" },
  ai: { bar: "bg-green-500/60", badge: "bg-green-500/20", text: "text-green-400" },
  python: { bar: "bg-orange-500/60", badge: "bg-orange-500/20", text: "text-orange-400" },
  unknown: { bar: "bg-zinc-500/60", badge: "bg-zinc-500/20", text: "text-zinc-400" },
};

/** Filter state for the trace viewer. */
export interface SpanFilter {
  nameSearch: string;
  minDurationMs: number | null;
  phase: WorkflowPhase | "all";
  status: "all" | "success" | "error";
}

/** Performance insights summary. */
export interface TraceInsights {
  totalDurationMs: number;
  spanCount: number;
  errorCount: number;
  slowestSpan: { name: string; durationMs: number } | null;
  phaseBreakdown: Record<string, number>;
}
