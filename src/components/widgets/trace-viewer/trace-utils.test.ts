import { describe, it, expect } from "vitest";
import type { TraceSpan, TraceTreeNode } from "./types";
import {
  buildFlameData,
  flattenFlameNodes,
  flattenTree,
  filterSpans,
  alignSpans,
  computeComparisonSummary,
  computeCriticalPath,
  computeInsights,
  buildTraceTree,
  computeTokenInsights,
  getTokenHeat,
} from "./trace-utils";
import { deriveReplaySteps } from "./useReplayData";

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

let _id = 0;
function makeSpan(overrides: Partial<TraceSpan> & { name: string; span_id: string }): TraceSpan {
  _id++;
  return {
    id: _id,
    execution_id: "exec-1",
    trace_id: "trace-1",
    span_id: overrides.span_id,
    parent_span_id: overrides.parent_span_id ?? null,
    name: overrides.name,
    target: null,
    level: null,
    start_ts: overrides.start_ts ?? "2026-01-01T00:00:00.000Z",
    end_ts: overrides.end_ts ?? null,
    duration_ms: overrides.duration_ms ?? null,
    attributes: overrides.attributes ?? {},
    success: overrides.success ?? true,
    error: overrides.error ?? null,
    input_tokens: overrides.input_tokens,
    output_tokens: overrides.output_tokens,
    cost_cents: overrides.cost_cents,
  };
}

/** Build a tree from spans for convenience. */
function treeFrom(spans: TraceSpan[]): TraceTreeNode[] {
  return buildTraceTree(spans);
}

// ---------------------------------------------------------------------------
// buildFlameData tests
// ---------------------------------------------------------------------------

describe("buildFlameData", () => {
  it("returns empty array for empty input", () => {
    expect(buildFlameData([])).toEqual([]);
  });

  it("correctly calculates widths and positions for a 3-level span tree", () => {
    // Root: 0-1000ms
    //   Child A: 0-600ms
    //     Grandchild: 0-300ms
    //   Child B: 600-1000ms
    const spans: TraceSpan[] = [
      makeSpan({
        name: "root",
        span_id: "root",
        start_ts: "2026-01-01T00:00:00.000Z",
        end_ts: "2026-01-01T00:00:01.000Z",
        duration_ms: 1000,
      }),
      makeSpan({
        name: "child.a",
        span_id: "child-a",
        parent_span_id: "root",
        start_ts: "2026-01-01T00:00:00.000Z",
        end_ts: "2026-01-01T00:00:00.600Z",
        duration_ms: 600,
      }),
      makeSpan({
        name: "child.b",
        span_id: "child-b",
        parent_span_id: "root",
        start_ts: "2026-01-01T00:00:00.600Z",
        end_ts: "2026-01-01T00:00:01.000Z",
        duration_ms: 400,
      }),
      makeSpan({
        name: "grandchild",
        span_id: "grandchild",
        parent_span_id: "child-a",
        start_ts: "2026-01-01T00:00:00.000Z",
        end_ts: "2026-01-01T00:00:00.300Z",
        duration_ms: 300,
      }),
    ];

    const tree = treeFrom(spans);
    const flame = buildFlameData(tree);
    const flat = flattenFlameNodes(flame);

    // Root should span full width
    const rootNode = flat.find((n) => n.span.span_id === "root")!;
    expect(rootNode.xStart).toBeCloseTo(0, 5);
    expect(rootNode.xEnd).toBeCloseTo(1, 5);
    expect(rootNode.depth).toBe(0);

    // Child A: starts at 0, duration=600/1000=0.6
    const childA = flat.find((n) => n.span.span_id === "child-a")!;
    expect(childA.xStart).toBeCloseTo(0, 5);
    expect(childA.xEnd).toBeCloseTo(0.6, 5);
    expect(childA.depth).toBe(1);

    // Child B: starts at 600/1000=0.6, duration=400/1000=0.4
    const childB = flat.find((n) => n.span.span_id === "child-b")!;
    expect(childB.xStart).toBeCloseTo(0.6, 5);
    expect(childB.xEnd).toBeCloseTo(1.0, 5);
    expect(childB.depth).toBe(1);

    // Grandchild: only child of child-a, so it fills child-a's entire range [0, 0.6]
    // (gap collapsing packs children proportionally within parent's width)
    const gc = flat.find((n) => n.span.span_id === "grandchild")!;
    expect(gc.xStart).toBeCloseTo(0, 5);
    expect(gc.xEnd).toBeCloseTo(0.6, 5);
    expect(gc.depth).toBe(2);
  });

  it("handles single root with no children", () => {
    const spans: TraceSpan[] = [
      makeSpan({
        name: "solo",
        span_id: "solo",
        start_ts: "2026-01-01T00:00:00.000Z",
        end_ts: "2026-01-01T00:00:05.000Z",
        duration_ms: 5000,
      }),
    ];

    const tree = treeFrom(spans);
    const flame = buildFlameData(tree);
    expect(flame).toHaveLength(1);
    expect(flame[0].xStart).toBeCloseTo(0, 5);
    expect(flame[0].xEnd).toBeCloseTo(1, 5);
  });

  it("assigns correct phase from span name", () => {
    const spans: TraceSpan[] = [
      makeSpan({
        name: "qontinui.ai.session",
        span_id: "ai",
        start_ts: "2026-01-01T00:00:00.000Z",
        end_ts: "2026-01-01T00:00:01.000Z",
        duration_ms: 1000,
      }),
    ];
    const tree = treeFrom(spans);
    const flame = buildFlameData(tree);
    expect(flame[0].phase).toBe("ai");
  });
});

// ---------------------------------------------------------------------------
// alignSpans tests
// ---------------------------------------------------------------------------

describe("alignSpans", () => {
  it("matches spans by name+phase+iteration", () => {
    const spansA = [
      makeSpan({ name: "setup.init", span_id: "a1", duration_ms: 100 }),
      makeSpan({ name: "ai.session", span_id: "a2", duration_ms: 500 }),
    ];
    const spansB = [
      makeSpan({ name: "setup.init", span_id: "b1", duration_ms: 120 }),
      makeSpan({ name: "ai.session", span_id: "b2", duration_ms: 400 }),
    ];

    const matches = alignSpans(spansA, spansB);
    expect(matches).toHaveLength(2);

    const initMatch = matches.find((m) => (m.spanA ?? m.spanB)!.name === "setup.init")!;
    expect(initMatch.spanA).toBeTruthy();
    expect(initMatch.spanB).toBeTruthy();
    expect(initMatch.durationDelta).toBe(20); // 120 - 100
  });

  it("detects added spans (only in B)", () => {
    const spansA = [makeSpan({ name: "setup.init", span_id: "a1", duration_ms: 100 })];
    const spansB = [
      makeSpan({ name: "setup.init", span_id: "b1", duration_ms: 100 }),
      makeSpan({ name: "ai.extra", span_id: "b2", duration_ms: 200 }),
    ];

    const matches = alignSpans(spansA, spansB);
    const added = matches.filter((m) => m.status === "added");
    expect(added).toHaveLength(1);
    expect(added[0].spanA).toBeNull();
    expect(added[0].spanB!.name).toBe("ai.extra");
  });

  it("detects removed spans (only in A)", () => {
    const spansA = [
      makeSpan({ name: "setup.init", span_id: "a1", duration_ms: 100 }),
      makeSpan({ name: "ai.old", span_id: "a2", duration_ms: 200 }),
    ];
    const spansB = [makeSpan({ name: "setup.init", span_id: "b1", duration_ms: 100 })];

    const matches = alignSpans(spansA, spansB);
    const removed = matches.filter((m) => m.status === "removed");
    expect(removed).toHaveLength(1);
    expect(removed[0].spanA!.name).toBe("ai.old");
    expect(removed[0].spanB).toBeNull();
  });

  it("flags status_changed when success differs", () => {
    const spansA = [
      makeSpan({ name: "setup.init", span_id: "a1", duration_ms: 100, success: true }),
    ];
    const spansB = [
      makeSpan({ name: "setup.init", span_id: "b1", duration_ms: 100, success: false }),
    ];

    const matches = alignSpans(spansA, spansB);
    expect(matches[0].status).toBe("status_changed");
  });

  it("flags slower/faster based on >5% threshold", () => {
    const spansA = [makeSpan({ name: "setup.init", span_id: "a1", duration_ms: 1000 })];
    const spansB = [makeSpan({ name: "setup.init", span_id: "b1", duration_ms: 1200 })]; // +20%

    const matches = alignSpans(spansA, spansB);
    expect(matches[0].status).toBe("slower");
    expect(matches[0].durationDeltaPct).toBeCloseTo(20, 1);
  });

  it("treats small differences as unchanged", () => {
    const spansA = [makeSpan({ name: "setup.init", span_id: "a1", duration_ms: 1000 })];
    const spansB = [makeSpan({ name: "setup.init", span_id: "b1", duration_ms: 1020 })]; // +2%

    const matches = alignSpans(spansA, spansB);
    expect(matches[0].status).toBe("unchanged");
  });

  it("handles runs with different iteration counts", () => {
    const spansA = [
      makeSpan({
        name: "verification.check",
        span_id: "a1",
        duration_ms: 100,
        attributes: { iteration: 1 },
      }),
      makeSpan({
        name: "verification.check",
        span_id: "a2",
        duration_ms: 100,
        attributes: { iteration: 2 },
      }),
    ];
    const spansB = [
      makeSpan({
        name: "verification.check",
        span_id: "b1",
        duration_ms: 120,
        attributes: { iteration: 1 },
      }),
      // No iteration 2 in B
    ];

    const matches = alignSpans(spansA, spansB);
    const removed = matches.filter((m) => m.status === "removed");
    expect(removed).toHaveLength(1); // iteration 2 only in A
  });
});

describe("computeComparisonSummary", () => {
  it("flags >50% slower spans as regressions", () => {
    const spansA = [
      makeSpan({
        name: "ai.session",
        span_id: "a1",
        duration_ms: 1000,
        start_ts: "2026-01-01T00:00:00.000Z",
        end_ts: "2026-01-01T00:00:01.000Z",
      }),
    ];
    const spansB = [
      makeSpan({
        name: "ai.session",
        span_id: "b1",
        duration_ms: 2000,
        start_ts: "2026-01-01T00:00:00.000Z",
        end_ts: "2026-01-01T00:00:02.000Z",
      }),
    ];

    const matches = alignSpans(spansA, spansB);
    const summary = computeComparisonSummary(spansA, spansB, matches);

    expect(summary.slowerCount).toBe(1);
    expect(summary.regressions).toHaveLength(1);
    expect(summary.totalDeltaPct).toBeCloseTo(100, 1);
  });

  it("flags new error spans as regressions", () => {
    const spansA: TraceSpan[] = [];
    const spansB = [
      makeSpan({ name: "ai.session", span_id: "b1", duration_ms: 100, success: false }),
    ];

    const matches = alignSpans(spansA, spansB);
    const summary = computeComparisonSummary(spansA, spansB, matches);

    expect(summary.addedCount).toBe(1);
    expect(summary.regressions).toHaveLength(1);
  });
});

// ---------------------------------------------------------------------------
// computeCriticalPath tests
// ---------------------------------------------------------------------------

describe("computeCriticalPath", () => {
  it("returns empty for empty input", () => {
    const result = computeCriticalPath([]);
    expect(result.spanIds.size).toBe(0);
    expect(result.pathDurationMs).toBe(0);
  });

  it("finds the longest path in a simple 3-span linear tree", () => {
    // Root (100ms) -> Child (200ms) -> Grandchild (300ms)
    const spans: TraceSpan[] = [
      makeSpan({
        name: "root",
        span_id: "root",
        start_ts: "2026-01-01T00:00:00.000Z",
        end_ts: "2026-01-01T00:00:00.100Z",
        duration_ms: 100,
      }),
      makeSpan({
        name: "child",
        span_id: "child",
        parent_span_id: "root",
        start_ts: "2026-01-01T00:00:00.100Z",
        end_ts: "2026-01-01T00:00:00.300Z",
        duration_ms: 200,
      }),
      makeSpan({
        name: "grandchild",
        span_id: "gc",
        parent_span_id: "child",
        start_ts: "2026-01-01T00:00:00.300Z",
        end_ts: "2026-01-01T00:00:00.600Z",
        duration_ms: 300,
      }),
    ];

    const tree = treeFrom(spans);
    const result = computeCriticalPath(tree);

    expect(result.spanIds.size).toBe(3);
    expect(result.spanIds.has("root")).toBe(true);
    expect(result.spanIds.has("child")).toBe(true);
    expect(result.spanIds.has("gc")).toBe(true);
    expect(result.pathDurationMs).toBe(600); // 100 + 200 + 300
  });

  it("picks the longest branch when siblings are parallel", () => {
    // Root (50ms)
    //   ├── ShortChild (100ms)
    //   └── LongChild (500ms) -> Leaf (200ms)
    const spans: TraceSpan[] = [
      makeSpan({
        name: "root",
        span_id: "root",
        start_ts: "2026-01-01T00:00:00.000Z",
        end_ts: "2026-01-01T00:00:00.050Z",
        duration_ms: 50,
      }),
      makeSpan({
        name: "short",
        span_id: "short",
        parent_span_id: "root",
        start_ts: "2026-01-01T00:00:00.050Z",
        end_ts: "2026-01-01T00:00:00.150Z",
        duration_ms: 100,
      }),
      makeSpan({
        name: "long",
        span_id: "long",
        parent_span_id: "root",
        start_ts: "2026-01-01T00:00:00.050Z",
        end_ts: "2026-01-01T00:00:00.550Z",
        duration_ms: 500,
      }),
      makeSpan({
        name: "leaf",
        span_id: "leaf",
        parent_span_id: "long",
        start_ts: "2026-01-01T00:00:00.550Z",
        end_ts: "2026-01-01T00:00:00.750Z",
        duration_ms: 200,
      }),
    ];

    const tree = treeFrom(spans);
    const result = computeCriticalPath(tree);

    // Critical path: root -> long -> leaf = 50 + 500 + 200 = 750
    expect(result.spanIds.has("root")).toBe(true);
    expect(result.spanIds.has("long")).toBe(true);
    expect(result.spanIds.has("leaf")).toBe(true);
    expect(result.spanIds.has("short")).toBe(false);
    expect(result.pathDurationMs).toBe(750);
  });

  it("identifies the bottleneck span", () => {
    const spans: TraceSpan[] = [
      makeSpan({
        name: "root",
        span_id: "root",
        start_ts: "2026-01-01T00:00:00.000Z",
        end_ts: "2026-01-01T00:00:00.100Z",
        duration_ms: 100,
      }),
      makeSpan({
        name: "bottleneck.ai.session",
        span_id: "bottleneck",
        parent_span_id: "root",
        start_ts: "2026-01-01T00:00:00.100Z",
        end_ts: "2026-01-01T00:00:05.100Z",
        duration_ms: 5000,
      }),
    ];

    const tree = treeFrom(spans);
    const result = computeCriticalPath(tree);

    expect(result.bottleneck!.name).toBe("bottleneck.ai.session");
    expect(result.bottleneck!.durationMs).toBe(5000);
  });

  it("computes zero slack for critical path spans", () => {
    const spans: TraceSpan[] = [
      makeSpan({
        name: "root",
        span_id: "root",
        start_ts: "2026-01-01T00:00:00.000Z",
        end_ts: "2026-01-01T00:00:01.000Z",
        duration_ms: 1000,
      }),
    ];

    const tree = treeFrom(spans);
    const result = computeCriticalPath(tree);
    expect(result.slackBySpanId.get("root")).toBe(0);
  });

  it("computes positive slack for non-critical path spans", () => {
    // Root (50ms)
    //   ├── Short (100ms)
    //   └── Long (500ms)
    const spans: TraceSpan[] = [
      makeSpan({
        name: "root",
        span_id: "root",
        start_ts: "2026-01-01T00:00:00.000Z",
        end_ts: "2026-01-01T00:00:00.050Z",
        duration_ms: 50,
      }),
      makeSpan({
        name: "short",
        span_id: "short",
        parent_span_id: "root",
        start_ts: "2026-01-01T00:00:00.050Z",
        end_ts: "2026-01-01T00:00:00.150Z",
        duration_ms: 100,
      }),
      makeSpan({
        name: "long",
        span_id: "long",
        parent_span_id: "root",
        start_ts: "2026-01-01T00:00:00.050Z",
        end_ts: "2026-01-01T00:00:00.550Z",
        duration_ms: 500,
      }),
    ];

    const tree = treeFrom(spans);
    const result = computeCriticalPath(tree);

    // "short" is not on critical path, slack = 500 - 100 = 400
    expect(result.slackBySpanId.get("short")).toBe(400);
    // "long" is on critical path, slack = 0
    expect(result.slackBySpanId.get("long")).toBe(0);
  });
});

// ---------------------------------------------------------------------------
// computeTokenInsights tests
// ---------------------------------------------------------------------------

describe("computeTokenInsights", () => {
  it("should aggregate token counts across spans", () => {
    const spans: TraceSpan[] = [
      makeSpan({
        name: "qontinui.ai.session",
        span_id: "t1",
        input_tokens: 100,
        output_tokens: 50,
        cost_cents: 10,
      }),
      makeSpan({
        name: "qontinui.ai.session",
        span_id: "t2",
        input_tokens: 200,
        output_tokens: 150,
        cost_cents: 25,
      }),
    ];
    const insights = computeTokenInsights(spans);
    expect(insights.totalInputTokens).toBe(300);
    expect(insights.totalOutputTokens).toBe(200);
    expect(insights.totalCostCents).toBe(35);
    expect(insights.maxInputTokens).toBe(200);
    expect(insights.maxOutputTokens).toBe(150);
  });

  it("should handle spans without token data", () => {
    const spans: TraceSpan[] = [
      makeSpan({ name: "qontinui.workflow", span_id: "t3" }),
      makeSpan({ name: "qontinui.ai.session", span_id: "t4", input_tokens: 100 }),
    ];
    const insights = computeTokenInsights(spans);
    expect(insights.totalInputTokens).toBe(100);
    expect(insights.totalOutputTokens).toBe(0);
  });

  it("should break down tokens by phase", () => {
    const spans: TraceSpan[] = [
      makeSpan({ name: "qontinui.workflow.phase.setup", span_id: "t5", input_tokens: 50 }),
      makeSpan({ name: "qontinui.ai.session", span_id: "t6", input_tokens: 200 }),
    ];
    const insights = computeTokenInsights(spans);
    expect(insights.phaseTokens["setup"]?.input).toBe(50);
    expect(insights.phaseTokens["ai"]?.input).toBe(200);
  });

  it("should return zeros for empty spans array", () => {
    const insights = computeTokenInsights([]);
    expect(insights.totalInputTokens).toBe(0);
    expect(insights.totalOutputTokens).toBe(0);
    expect(insights.totalCostCents).toBe(0);
  });
});

// ---------------------------------------------------------------------------
// getTokenHeat tests
// ---------------------------------------------------------------------------

describe("getTokenHeat", () => {
  it("should return 0 for 'none' mode", () => {
    const span = makeSpan({ name: "test", span_id: "h1", input_tokens: 100 });
    expect(getTokenHeat(span, "none", 100)).toBe(0);
  });

  it("should return correct heat for input mode", () => {
    const span = makeSpan({ name: "test", span_id: "h2", input_tokens: 50 });
    expect(getTokenHeat(span, "input", 100)).toBe(0.5);
  });

  it("should clamp at 1", () => {
    const span = makeSpan({ name: "test", span_id: "h3", input_tokens: 200 });
    expect(getTokenHeat(span, "input", 100)).toBe(1);
  });

  it("should return 0 when maxValue is 0", () => {
    const span = makeSpan({ name: "test", span_id: "h4", input_tokens: 100 });
    expect(getTokenHeat(span, "input", 0)).toBe(0);
  });

  it("should handle missing token fields", () => {
    const span = makeSpan({ name: "test", span_id: "h5" });
    expect(getTokenHeat(span, "input", 100)).toBe(0);
  });
});

// ---------------------------------------------------------------------------
// deriveReplaySteps tests
// ---------------------------------------------------------------------------

describe("deriveReplaySteps", () => {
  it("should categorize phase transition spans", () => {
    const spans: TraceSpan[] = [
      makeSpan({
        name: "qontinui.workflow.phase.setup",
        span_id: "r1",
        start_ts: "2024-01-01T00:00:00Z",
      }),
      makeSpan({
        name: "qontinui.workflow.phase.verification",
        span_id: "r2",
        start_ts: "2024-01-01T00:01:00Z",
      }),
    ];
    const steps = deriveReplaySteps(spans);
    expect(steps).toHaveLength(2);
    expect(steps[0].type).toBe("phase_transition");
    expect(steps[1].type).toBe("phase_transition");
  });

  it("should categorize AI session spans", () => {
    const spans: TraceSpan[] = [
      makeSpan({
        name: "qontinui.ai.session",
        span_id: "r3",
        start_ts: "2024-01-01T00:00:00Z",
        input_tokens: 100,
      }),
    ];
    const steps = deriveReplaySteps(spans);
    expect(steps).toHaveLength(1);
    expect(steps[0].type).toBe("ai_session");
  });

  it("should categorize tool call spans", () => {
    const spans: TraceSpan[] = [
      makeSpan({
        name: "qontinui.ai.tool_call",
        span_id: "r4",
        start_ts: "2024-01-01T00:00:00Z",
        attributes: { "ai.tool_name": "Read" },
      }),
    ];
    const steps = deriveReplaySteps(spans);
    expect(steps).toHaveLength(1);
    expect(steps[0].type).toBe("tool_call");
    expect(steps[0].title).toBe("Read");
  });

  it("should sort steps by start_ts", () => {
    const spans: TraceSpan[] = [
      makeSpan({ name: "qontinui.ai.session", span_id: "r5", start_ts: "2024-01-01T00:02:00Z" }),
      makeSpan({
        name: "qontinui.workflow.phase.setup",
        span_id: "r6",
        start_ts: "2024-01-01T00:00:00Z",
      }),
      makeSpan({ name: "qontinui.ai.tool_call", span_id: "r7", start_ts: "2024-01-01T00:01:00Z" }),
    ];
    const steps = deriveReplaySteps(spans);
    expect(steps[0].type).toBe("phase_transition");
    expect(steps[1].type).toBe("tool_call");
    expect(steps[2].type).toBe("ai_session");
  });

  it("should return empty array for empty spans", () => {
    expect(deriveReplaySteps([])).toHaveLength(0);
  });

  it("should skip unrecognized span names", () => {
    const spans: TraceSpan[] = [
      makeSpan({
        name: "qontinui.python.startup",
        span_id: "r8",
        start_ts: "2024-01-01T00:00:00Z",
      }),
    ];
    const steps = deriveReplaySteps(spans);
    expect(steps).toHaveLength(0);
  });
});

// ---------------------------------------------------------------------------
// Defensive guards for non-array input
// ---------------------------------------------------------------------------

describe("defensive guards for non-array input", () => {
  it("computeInsights returns empty result for undefined", () => {
    // @ts-expect-error - testing runtime defense against non-array input
    const result = computeInsights(undefined);
    expect(result.spanCount).toBe(0);
    expect(result.totalDurationMs).toBe(0);
  });

  it("computeInsights returns empty result for object input", () => {
    // @ts-expect-error - testing runtime defense against envelope object
    const result = computeInsights({ spans: [] });
    expect(result.spanCount).toBe(0);
    expect(result.totalDurationMs).toBe(0);
  });

  it("computeInsights returns empty result for null", () => {
    // @ts-expect-error - testing runtime defense against null
    const result = computeInsights(null);
    expect(result.spanCount).toBe(0);
  });

  it("buildTraceTree returns empty array for null", () => {
    // @ts-expect-error - testing runtime defense against null
    expect(buildTraceTree(null)).toEqual([]);
  });

  it("buildTraceTree returns empty array for undefined", () => {
    // @ts-expect-error - testing runtime defense against undefined
    expect(buildTraceTree(undefined)).toEqual([]);
  });

  it("buildTraceTree returns empty array for object", () => {
    // @ts-expect-error - testing runtime defense against envelope object
    expect(buildTraceTree({ spans: [] })).toEqual([]);
  });

  it("flattenTree returns empty array for undefined", () => {
    // @ts-expect-error - testing runtime defense against undefined
    expect(flattenTree(undefined)).toEqual([]);
  });

  it("flattenTree returns empty array for null", () => {
    // @ts-expect-error - testing runtime defense against null
    expect(flattenTree(null)).toEqual([]);
  });

  it("filterSpans returns empty array for non-array", () => {
    // @ts-expect-error - testing runtime defense against non-array
    expect(filterSpans(undefined, { nameSearch: "", minDurationMs: null, phase: "all", status: "all" })).toEqual([]);
  });

  it("buildFlameData returns empty array for non-array", () => {
    // @ts-expect-error - testing runtime defense against non-array
    expect(buildFlameData(null)).toEqual([]);
  });

  it("flattenFlameNodes returns empty array for non-array", () => {
    // @ts-expect-error - testing runtime defense against non-array
    expect(flattenFlameNodes(undefined)).toEqual([]);
  });

  it("alignSpans returns empty array for non-array inputs", () => {
    // @ts-expect-error - testing runtime defense against non-array
    expect(alignSpans(null, [])).toEqual([]);
    // @ts-expect-error - testing runtime defense against non-array
    expect(alignSpans([], undefined)).toEqual([]);
    // @ts-expect-error - testing runtime defense against non-array
    expect(alignSpans(null, null)).toEqual([]);
  });

  it("computeTokenInsights returns empty result for non-array", () => {
    // @ts-expect-error - testing runtime defense against non-array
    const result = computeTokenInsights(undefined);
    expect(result.totalInputTokens).toBe(0);
    expect(result.totalOutputTokens).toBe(0);
    expect(result.totalCostCents).toBe(0);
    expect(result.phaseTokens).toEqual({});
  });

  it("computeTokenInsights returns empty result for object input", () => {
    // @ts-expect-error - testing runtime defense against envelope object
    const result = computeTokenInsights({ spans: [] });
    expect(result.totalInputTokens).toBe(0);
    expect(result.maxInputTokens).toBe(0);
  });
});
