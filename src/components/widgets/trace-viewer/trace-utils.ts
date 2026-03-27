import type {
  TraceSpan,
  TraceTreeNode,
  WorkflowPhase,
  SpanFilter,
  TraceInsights,
  FlameNode,
  SpanMatch,
  ComparisonSummary,
  CriticalPathInfo,
} from "./types";

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
    (a, b) => new Date(a.start_ts).getTime() - new Date(b.start_ts).getTime(),
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
  const ends = spans.filter((s) => s.end_ts).map((s) => new Date(s.end_ts!).getTime());

  const totalDurationMs =
    ends.length > 0
      ? ends.reduce((a, b) => Math.max(a, b), -Infinity) -
        starts.reduce((a, b) => Math.min(a, b), Infinity)
      : 0;

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

// ---------------------------------------------------------------------------
// Flamechart utilities
// ---------------------------------------------------------------------------

/**
 * Build flamechart layout data from a trace tree.
 * Collapses idle gaps between sibling spans so the chart shows where time is *spent*.
 * Each node's x range [xStart, xEnd] is a fraction of the total active time.
 */
export function buildFlameData(roots: TraceTreeNode[]): FlameNode[] {
  if (roots.length === 0) return [];

  /**
   * Build a FlameNode with gap-collapsed children.
   * parentXStart/parentXEnd define the parent's fraction range — children are laid out
   * within this range proportional to their duration, with idle gaps removed.
   */
  function buildFlameNode(
    node: TraceTreeNode,
    xStart: number,
    xEnd: number,
  ): FlameNode {
    const phase = inferPhase(node.span.name);

    const sortedChildren = [...node.children].sort(
      (a, b) => new Date(a.span.start_ts).getTime() - new Date(b.span.start_ts).getTime(),
    );

    // Collapse gaps: children are packed within the parent's x range
    // proportional to their own duration (not wall-clock position).
    const parentWidth = xEnd - xStart;
    const childDurations = sortedChildren.map((c) => c.span.duration_ms ?? 0);
    const totalChildDuration = childDurations.reduce((a, b) => a + b, 0) || 1;

    let cursor = xStart;
    const children: FlameNode[] = sortedChildren.map((child, i) => {
      const childFrac = childDurations[i] / totalChildDuration;
      const childWidth = parentWidth * childFrac;
      const childXStart = cursor;
      const childXEnd = cursor + childWidth;
      cursor = childXEnd;
      return buildFlameNode(child, childXStart, childXEnd);
    });

    return { span: node.span, phase, depth: node.depth, xStart, xEnd, children };
  }

  // Lay out roots: pack proportional to duration
  const rootDurations = roots.map((r) => r.span.duration_ms ?? 0);
  const totalRootDuration = rootDurations.reduce((a, b) => a + b, 0) || 1;

  let cursor = 0;
  return roots.map((root, i) => {
    const frac = rootDurations[i] / totalRootDuration;
    const xStart = cursor;
    const xEnd = cursor + frac;
    cursor = xEnd;
    return buildFlameNode(root, xStart, xEnd);
  });
}

/** Flatten flame nodes into a flat list for rendering. */
export function flattenFlameNodes(roots: FlameNode[]): FlameNode[] {
  const result: FlameNode[] = [];
  function dfs(node: FlameNode) {
    result.push(node);
    for (const child of node.children) dfs(child);
  }
  for (const root of roots) dfs(root);
  return result;
}

// ---------------------------------------------------------------------------
// Cross-run comparison utilities
// ---------------------------------------------------------------------------

/** Create a match key for a span to align across runs. */
function spanMatchKey(span: TraceSpan): string {
  const phase = inferPhase(span.name);
  const iteration = span.attributes?.iteration ?? "";
  return `${span.name}|${phase}|${iteration}`;
}

/** Align spans from two runs and produce match results. */
export function alignSpans(spansA: TraceSpan[], spansB: TraceSpan[]): SpanMatch[] {
  const mapA = new Map<string, TraceSpan>();
  const mapB = new Map<string, TraceSpan>();

  for (const s of spansA) mapA.set(spanMatchKey(s), s);
  for (const s of spansB) mapB.set(spanMatchKey(s), s);

  const allKeys = new Set([...mapA.keys(), ...mapB.keys()]);
  const matches: SpanMatch[] = [];

  for (const key of allKeys) {
    const a = mapA.get(key) ?? null;
    const b = mapB.get(key) ?? null;

    if (!a) {
      matches.push({ spanA: null, spanB: b, durationDelta: null, durationDeltaPct: null, status: "added" });
    } else if (!b) {
      matches.push({ spanA: a, spanB: null, durationDelta: null, durationDeltaPct: null, status: "removed" });
    } else {
      const durA = a.duration_ms ?? 0;
      const durB = b.duration_ms ?? 0;
      const delta = durB - durA;
      const deltaPct = durA > 0 ? (delta / durA) * 100 : 0;

      let status: SpanMatch["status"] = "unchanged";
      if (a.success !== b.success) {
        status = "status_changed";
      } else if (Math.abs(deltaPct) > 5) {
        status = delta > 0 ? "slower" : "faster";
      }

      matches.push({ spanA: a, spanB: b, durationDelta: delta, durationDeltaPct: deltaPct, status });
    }
  }

  // Sort: regressions first, then by absolute delta descending
  matches.sort((a, b) => {
    const order: Record<SpanMatch["status"], number> = {
      status_changed: 0,
      slower: 1,
      added: 2,
      removed: 3,
      faster: 4,
      unchanged: 5,
    };
    return (order[a.status] - order[b.status]) || (Math.abs(b.durationDelta ?? 0) - Math.abs(a.durationDelta ?? 0));
  });

  return matches;
}

/** Compute comparison summary with regression detection. */
export function computeComparisonSummary(
  spansA: TraceSpan[],
  spansB: TraceSpan[],
  matches: SpanMatch[],
): ComparisonSummary {
  const totalA = computeInsights(spansA).totalDurationMs;
  const totalB = computeInsights(spansB).totalDurationMs;
  const totalDeltaPct = totalA > 0 ? ((totalB - totalA) / totalA) * 100 : 0;

  let fasterCount = 0, slowerCount = 0, addedCount = 0, removedCount = 0, statusChangedCount = 0;
  const regressions: SpanMatch[] = [];

  for (const m of matches) {
    switch (m.status) {
      case "faster": fasterCount++; break;
      case "slower": slowerCount++; break;
      case "added": addedCount++; break;
      case "removed": removedCount++; break;
      case "status_changed": statusChangedCount++; break;
    }
    // Flag regressions: >50% slower, status changed to error, or new error spans
    const isRegression =
      (m.status === "slower" && m.durationDeltaPct !== null && m.durationDeltaPct > 50) ||
      (m.status === "status_changed" && m.spanB && !m.spanB.success) ||
      (m.status === "added" && m.spanB && !m.spanB.success);
    if (isRegression) regressions.push(m);
  }

  return {
    totalDurationA: totalA,
    totalDurationB: totalB,
    totalDeltaPct,
    fasterCount,
    slowerCount,
    addedCount,
    removedCount,
    statusChangedCount,
    regressions,
  };
}

// ---------------------------------------------------------------------------
// Critical path utilities
// ---------------------------------------------------------------------------

/** Compute the critical path through a span tree (longest sequential chain). */
export function computeCriticalPath(roots: TraceTreeNode[]): CriticalPathInfo {
  const empty: CriticalPathInfo = {
    spanIds: new Set(),
    pathDurationMs: 0,
    bottleneck: null,
    slackBySpanId: new Map(),
  };
  if (roots.length === 0) return empty;

  const allNodes = flattenTree(roots);

  // For each node, compute the "weight" of the longest path from that node to a leaf.
  // Weight = own duration + max(child path weights)
  // But for parallel children, only the longest child is on the critical path.
  const pathWeight = new Map<string, number>();
  const criticalChild = new Map<string, string | null>(); // span_id -> child span_id on crit path

  function computeWeight(node: TraceTreeNode): number {
    const ownDur = node.span.duration_ms ?? 0;

    if (node.children.length === 0) {
      pathWeight.set(node.span.span_id, ownDur);
      criticalChild.set(node.span.span_id, null);
      return ownDur;
    }

    let maxChildWeight = 0;
    let maxChildId: string | null = null;

    for (const child of node.children) {
      const w = computeWeight(child);
      if (w > maxChildWeight) {
        maxChildWeight = w;
        maxChildId = child.span.span_id;
      }
    }

    const total = ownDur + maxChildWeight;
    pathWeight.set(node.span.span_id, total);
    criticalChild.set(node.span.span_id, maxChildId);
    return total;
  }

  // Compute weights for all roots, find the root with the heaviest path
  let maxRootWeight = 0;
  let criticalRootId: string | null = null;

  for (const root of roots) {
    const w = computeWeight(root);
    if (w > maxRootWeight) {
      maxRootWeight = w;
      criticalRootId = root.span.span_id;
    }
  }

  // Walk the critical path
  const spanIds = new Set<string>();
  let bottleneck: { name: string; durationMs: number } | null = null;
  let pathDurationMs = 0;

  const nodeMap = new Map<string, TraceTreeNode>();
  for (const n of allNodes) nodeMap.set(n.span.span_id, n);

  let current = criticalRootId;
  while (current) {
    spanIds.add(current);
    const node = nodeMap.get(current);
    if (node) {
      const dur = node.span.duration_ms ?? 0;
      pathDurationMs += dur;
      if (!bottleneck || dur > bottleneck.durationMs) {
        bottleneck = { name: node.span.name, durationMs: dur };
      }
    }
    current = criticalChild.get(current) ?? null;
  }

  // Compute slack for each span: how much longer it could take without affecting total time.
  // For critical path spans, slack = 0.
  // For non-critical spans, slack = totalDuration - pathDurationMs (simplified),
  // but more precisely: slack = parent's end - span's end - sum of later siblings on crit path
  // We use a simplified model: slack = totalDuration - (span's own contribution path weight)
  const slackBySpanId = new Map<string, number>();
  for (const node of allNodes) {
    if (spanIds.has(node.span.span_id)) {
      slackBySpanId.set(node.span.span_id, 0);
    } else {
      // Slack = how much longer this span could be without extending the trace.
      // Approximate: find the parent, see the gap between this span's path weight
      // and the critical child's path weight.
      const ownWeight = pathWeight.get(node.span.span_id) ?? 0;
      const parent = allNodes.find((n) =>
        n.children.some((c) => c.span.span_id === node.span.span_id),
      );
      if (parent) {
        const critChild = criticalChild.get(parent.span.span_id);
        const critWeight = critChild ? (pathWeight.get(critChild) ?? 0) : 0;
        slackBySpanId.set(node.span.span_id, Math.max(critWeight - ownWeight, 0));
      } else {
        slackBySpanId.set(node.span.span_id, Math.max(maxRootWeight - ownWeight, 0));
      }
    }
  }

  return { spanIds, pathDurationMs, bottleneck, slackBySpanId };
}
