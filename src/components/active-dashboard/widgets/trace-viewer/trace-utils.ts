import type { TraceSpan, TraceTreeNode, WorkflowPhase, SpanFilter, TraceInsights } from "./types";

/** Infer the workflow phase from a span name. */
export function inferPhase(name: string): WorkflowPhase {
  if (name.includes("phase.setup") || name.includes("setup")) return "setup";
  if (name.includes("phase.verification") || name.includes("verification")) return "verification";
  if (name.includes("phase.agentic") || name.includes("agentic")) return "agentic";
  if (name.includes("phase.completion") || name.includes("completion")) return "completion";
  if (name.includes("ai.session") || name.includes("ai.")) return "ai";
  if (name.includes("python.")) return "python";
  return "unknown";
}

/** Build a tree from flat span data. */
export function buildTraceTree(spans: TraceSpan[]): TraceTreeNode[] {
  if (spans.length === 0) return [];

  // Sort by start timestamp
  const sorted = [...spans].sort(
    (a, b) => new Date(a.start_ts).getTime() - new Date(b.start_ts).getTime()
  );

  const traceStart = new Date(sorted[0].start_ts).getTime();

  // Build a map of span_id -> node
  const nodeMap = new Map<string, TraceTreeNode>();
  const roots: TraceTreeNode[] = [];

  for (const span of sorted) {
    const node: TraceTreeNode = {
      span,
      children: [],
      depth: 0,
      offsetMs: new Date(span.start_ts).getTime() - traceStart,
    };
    nodeMap.set(span.span_id, node);
  }

  // Link children to parents
  for (const span of sorted) {
    const node = nodeMap.get(span.span_id)!;
    if (span.parent_span_id && nodeMap.has(span.parent_span_id)) {
      const parent = nodeMap.get(span.parent_span_id)!;
      parent.children.push(node);
      node.depth = parent.depth + 1;
    } else {
      roots.push(node);
    }
  }

  return roots;
}

/** Flatten a tree into DFS order for rendering. */
export function flattenTree(roots: TraceTreeNode[]): TraceTreeNode[] {
  const result: TraceTreeNode[] = [];

  function dfs(node: TraceTreeNode) {
    result.push(node);
    // Sort children by start time
    const sortedChildren = [...node.children].sort((a, b) => a.offsetMs - b.offsetMs);
    for (const child of sortedChildren) {
      dfs(child);
    }
  }

  for (const root of roots) {
    dfs(root);
  }

  return result;
}

/** Filter spans based on filter criteria. */
export function filterSpans(nodes: TraceTreeNode[], filter: SpanFilter): TraceTreeNode[] {
  return nodes.filter((node) => {
    const span = node.span;

    // Name search
    if (filter.nameSearch && !span.name.toLowerCase().includes(filter.nameSearch.toLowerCase())) {
      return false;
    }

    // Min duration
    if (filter.minDurationMs !== null && (span.duration_ms ?? 0) < filter.minDurationMs) {
      return false;
    }

    // Phase filter
    if (filter.phase !== "all" && inferPhase(span.name) !== filter.phase) {
      return false;
    }

    // Status filter
    if (filter.status === "success" && !span.success) return false;
    if (filter.status === "error" && span.success) return false;

    return true;
  });
}

/** Compute performance insights from spans. */
export function computeInsights(spans: TraceSpan[]): TraceInsights {
  if (spans.length === 0) {
    return {
      totalDurationMs: 0,
      spanCount: 0,
      errorCount: 0,
      slowestSpan: null,
      phaseBreakdown: {},
    };
  }

  let slowest: { name: string; durationMs: number } | null = null;
  let errorCount = 0;
  const phaseBreakdown: Record<string, number> = {};

  for (const span of spans) {
    const dur = span.duration_ms ?? 0;

    if (!slowest || dur > slowest.durationMs) {
      slowest = { name: span.name, durationMs: dur };
    }

    if (!span.success) errorCount++;

    const phase = inferPhase(span.name);
    phaseBreakdown[phase] = (phaseBreakdown[phase] ?? 0) + dur;
  }

  // Total duration from first start to last end
  const starts = spans.map((s) => new Date(s.start_ts).getTime());
  const ends = spans
    .filter((s) => s.end_ts)
    .map((s) => new Date(s.end_ts!).getTime());

  const totalDurationMs =
    ends.length > 0 ? Math.max(...ends) - Math.min(...starts) : 0;

  return {
    totalDurationMs,
    spanCount: spans.length,
    errorCount,
    slowestSpan: slowest,
    phaseBreakdown,
  };
}

/** Format milliseconds as a human-readable duration. */
export function formatDuration(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  return `${(ms / 60000).toFixed(1)}m`;
}
