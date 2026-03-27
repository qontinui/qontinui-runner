/**
 * Trace Analysis Tutorial (Informational)
 *
 * Explains the trace viewer's three visualization modes, critical path analysis,
 * and cross-run comparison — how they work, what algorithms power them,
 * and how to read the information they present.
 */

import type { Tutorial } from "../../../types/tutorial";

export const traceAnalysisTutorial: Tutorial = {
  id: "trace-analysis",
  title: "Understanding Execution Traces",
  description:
    "A deep dive into the trace viewer's three visualization modes — Waterfall, Flamechart, and Comparison — plus critical path analysis. Learn how to read execution traces, identify bottlenecks, and detect regressions across runs.",
  duration: "12 minutes",
  difficulty: "intermediate",
  mode: "contextual",
  focusPage: "gui-automation",
  category: "Observability",
  tags: ["featured", "traces", "performance", "observability", "debugging"],
  prerequisites: ["workflow-execution"],
  learningObjectives: [
    "Understand how execution spans form a hierarchical trace",
    "Read waterfall timelines to identify parallelism and sequencing",
    "Use the flamechart to find where time is spent (not just when)",
    "Interpret critical path analysis to find the true bottleneck",
    "Compare runs to detect regressions and performance changes",
  ],
  steps: [
    {
      id: "intro",
      title: "What Are Execution Traces?",
      content: `Every workflow execution produces **execution spans** — timestamped records of each operation that ran: AI sessions, verification steps, shell commands, phase transitions, and more.

Each span has:
- A **name** and **phase** (setup, verification, agentic, completion)
- A **start time** and **duration**
- A **parent span** (forming a tree hierarchy)
- A **success/error** status and optional error message

The trace viewer turns this raw data into visual insights. It fetches spans from the \`/execution-spans\` API and polls every 2 seconds, so you can watch a live execution unfold.`,
      tips: [
        "Spans are linked by span_id and parent_span_id, forming a tree",
        "Phase is inferred from the span name (e.g., 'ai.session' maps to the AI phase)",
        "The trace viewer shares the same data across all three views",
      ],
      estimatedDuration: 1,
    },
    {
      id: "view-modes",
      title: "Three Ways to See a Trace",
      content: `The trace viewer offers three visualization modes, each answering a different question:

**Waterfall** — *When* did things happen?
Shows spans positioned on a timeline. Width = wall-clock duration. Great for seeing parallelism, sequencing, and gaps.

**Flamechart** — *Where* was time spent?
Collapses idle gaps. Width = proportion of active time. Great for finding which operations dominate execution.

**Comparison** — *What changed* between runs?
Aligns spans from two runs and highlights timing differences, new errors, and regressions.

Switch between them using the tabs at the top of the widget.`,
      targetElement: {
        selector: "trace-view-tabs",
        highlightType: "spotlight",
        position: "bottom",
      },
      estimatedDuration: 1,
    },
    {
      id: "waterfall-reading",
      title: "Reading the Waterfall",
      content: `The waterfall is the default view. Each row is a span:

- **Left column** — Span name, indented by depth. Children are nested under parents.
- **Right column** — Timeline bar whose left edge = start time, width = duration, color = phase.

**What to look for:**
- **Wide bars** = slow operations (bottlenecks)
- **Stacked bars at the same x position** = parallel execution
- **Gaps between bars** = idle time (waiting for something)
- **Red dots** = error spans (failed operations)

The timeline header shows marks at 0%, 25%, 50%, 75%, and 100% of total duration for orientation.`,
      targetElement: {
        selector: "trace-main-content",
        highlightType: "border",
        position: "left",
      },
      tips: [
        "Click any span to see its full details in the side panel",
        "The waterfall uses react-window virtualization — it handles hundreds of spans smoothly",
        "Time bounds are computed from ALL spans, not just filtered ones, so the timeline stays stable as you filter",
      ],
      estimatedDuration: 2,
    },
    {
      id: "filtering",
      title: "Filtering Spans",
      content: `With complex traces, filtering helps you focus:

- **Search** — Filter by span name (case-insensitive substring match)
- **Min duration** — Hide fast operations to find the slow ones
- **Phase** — Focus on a specific workflow phase (e.g., only AI operations)
- **Status** — Isolate errors or show only successful spans

Filters apply to both Waterfall and Flamechart views. The span count indicator shows how many spans match: "N / M spans" when filtered.

Filters are applied client-side after building and flattening the span tree, so they're instant even for large traces.`,
      tips: [
        "Combine min duration + phase filter to find 'slow AI operations' or 'slow verification steps'",
        "The status filter is useful for error investigation — show only errors, then click each to see details",
      ],
      estimatedDuration: 1,
    },
    {
      id: "flamechart-concept",
      title: "The Flamechart: Gap Collapsing",
      content: `The flamechart answers a different question than the waterfall: not *when* things happened, but *where time was spent*.

**Key difference: gap collapsing.** In the waterfall, if span A runs from 0-100ms and span B runs from 500-600ms, there's a visible 400ms gap. In the flamechart, A and B are placed side by side — each gets 50% of the width because they each took 100ms.

**The algorithm:**
1. Children are sorted by start time
2. Each child's width = (its duration / total sibling duration) x parent's width
3. Children are packed left-to-right with no gaps

This means wider rectangles = more time, regardless of when the operation ran. It's how you find the true time-eaters in a trace.`,
      targetElement: {
        selector: "trace-flamechart",
        highlightType: "spotlight",
        position: "right",
      },
      details: `**Technical detail:** The flamechart renders on an HTML5 Canvas (not DOM elements) for performance. Each span is a colored rectangle with phase-specific hex colors. Text labels are clipped to rectangle bounds and only rendered when the rectangle is wider than 60px. The canvas scales to devicePixelRatio for crisp HiDPI display.

The \`buildFlameData()\` function in \`trace-utils.ts\` takes a TraceTreeNode[] and produces FlameNode[] with fractional xStart/xEnd values in [0, 1]. This is computed once and cached via useMemo.`,
      estimatedDuration: 2,
    },
    {
      id: "flamechart-interactions",
      title: "Navigating the Flamechart",
      content: `The flamechart supports several navigation modes:

**Zoom** — Mouse wheel zooms horizontally (1x to 50x). The zoom keeps your mouse position stable so you can zoom into a specific area.

**Pan** — Click and drag to pan left/right when zoomed in.

**Depth filter** — The slider limits how many nesting levels are shown. Reduce depth to see only high-level phases; increase to see leaf operations.

**Focus subtree** — Double-click any span with children to "zoom into" that subtree. It becomes the new root, filling the full width. A breadcrumb trail appears so you can navigate back.

**Hover tooltip** — Shows span name, duration, percentage of total, and critical path info.`,
      targetElement: {
        selector: "trace-flamechart-controls",
        highlightType: "border",
        position: "bottom",
      },
      tips: [
        "Start with depth=2 or 3 to see the high-level structure, then increase for detail",
        "Focus subtree is powerful for deep traces — double-click a phase root to isolate it",
        "Click a span to select it and see details in the shared side panel",
      ],
      estimatedDuration: 1,
    },
    {
      id: "critical-path-concept",
      title: "Critical Path Analysis",
      content: `The **critical path** is the longest sequential chain of spans from root to leaf. It determines the total execution time — every other span could theoretically run longer without affecting the total.

**The algorithm:**
1. For each span, compute its "weight" = own duration + max(child weights)
2. For parallel children, only the heaviest child is on the critical path
3. Walk from root to leaf following the heaviest child at each level

**Slack** — Non-critical spans have "slack": how much longer they could run without extending the trace. A span with 500ms slack means it could take 500ms longer and the total wouldn't change.

Enable it with the **Critical Path** checkbox.`,
      targetElement: {
        selector: "trace-critical-path-toggle",
        highlightType: "pulse",
        position: "bottom",
      },
      details: `**Why this matters:** In a trace where phase A takes 2s and phase B takes 5s running in parallel, the total time is 5s. Optimizing phase A won't help — it has 3s of slack. Only optimizing phase B (the critical path) reduces total time.

The bottleneck is the single longest span on the critical path. The PerformanceInsights footer shows: critical path duration, bottleneck name, and parallel slack (total time - critical path time = time saved by parallelism).`,
      estimatedDuration: 2,
    },
    {
      id: "critical-path-visual",
      title: "Reading Critical Path Highlights",
      content: `When critical path is enabled, the visualization changes:

**In the Waterfall:**
- Critical path spans: **full opacity**, bold name, red glow border
- Non-critical spans: **dimmed to 40%** opacity
- **Red dashed connector lines** link critical path spans in sequence

**In the Flamechart:**
- Critical path spans: full opacity, red stroke border
- Non-critical spans: 35% opacity
- Hover tooltip shows "On critical path" or "Slack: Xs"

**In the Detail Panel:**
- "On critical path" badge for critical spans
- "Off critical path — Slack: Xs" for non-critical spans

**In the Insights Footer:**
- Critical path duration and percentage of total
- Bottleneck span name and duration
- Parallel slack (time saved by concurrent execution)`,
      targetElement: {
        selector: "trace-performance-insights",
        highlightType: "spotlight",
        position: "top",
      },
      estimatedDuration: 1,
    },
    {
      id: "comparison-concept",
      title: "Cross-Run Comparison",
      content: `The Comparison view diffs execution spans between two runs to detect regressions and improvements.

**How alignment works:**
Spans are matched by **name + phase + iteration** across runs. This means "verification.check iteration 1" in Run A is compared against "verification.check iteration 1" in Run B.

**Status classification:**
- **Unchanged** — <5% duration change, same success/error status
- **Faster** / **Slower** — >5% duration change
- **Added** — Span exists only in Run B (new operation)
- **Removed** — Span exists only in Run A (operation dropped)
- **Status Changed** — Success/error status differs between runs

By default, Run A is the most recent prior run with the same workflow name, and Run B is the current run.`,
      tips: [
        "You can override both Run A and Run B using the dropdown selectors",
        "Rows are sorted by severity: status changes first, then slower, added, removed, faster, unchanged",
        "Click any row to expand inline details with a duration bar chart and attribute diff",
      ],
      estimatedDuration: 1,
    },
    {
      id: "regression-detection",
      title: "Regression Detection",
      content: `The comparison view automatically flags **regressions** — changes that likely indicate a problem:

- **>50% slower** — A span that took significantly longer
- **Status changed to error** — A previously-passing span now fails
- **New error span** — A span added in Run B that immediately fails

When regressions are detected, a **red banner** appears at the top listing each one by name with the percentage increase or failure details.

The summary bar shows the overall picture: total duration delta, and counts for each category (faster, slower, added, removed, status changed).

**Expanded detail** for each match shows:
- Duration bar chart comparing Run A vs Run B
- Status change details with error messages
- Attribute diff showing what metadata changed`,
      details: `**Data flow:** The \`useTraceComparisonData\` hook calls \`useTraceViewerData\` independently for each run ID. The \`alignSpans()\` function matches spans by a composite key of name + phase + iteration. The \`computeComparisonSummary()\` function then classifies matches and detects regressions. All computation is client-side — no backend support needed beyond the existing execution_spans API.`,
      estimatedDuration: 1,
    },
    {
      id: "detail-panel",
      title: "The Span Detail Panel",
      content: `Clicking any span in the Waterfall or Flamechart opens a **280px detail panel** on the right showing everything about that span:

- **Name** (stripped of the "qontinui." prefix)
- **Status** — Success (green) or Error (red)
- **Phase** — Color-coded badge
- **Duration** — Formatted as ms/s/m
- **Timestamps** — Start and end times
- **IDs** — Trace, span, and parent span IDs (monospace)
- **Error** — Full error message in a red box (if failed)
- **Attributes** — All key-value metadata attached to the span
- **Critical path info** — On/off critical path with slack (when enabled)

This panel is shared between Waterfall and Flamechart views. Click the same span again or click the close button to dismiss it.`,
      targetElement: {
        selector: "trace-span-detail-panel",
        highlightType: "border",
        position: "left",
      },
      estimatedDuration: 1,
    },
    {
      id: "mental-model",
      title: "Putting It All Together",
      content: `Here's the mental model for using the trace viewer effectively:

**1. Start with the Waterfall** — Get a timeline overview. Notice wide bars, gaps, and error indicators.

**2. Enable Critical Path** — See what actually determines total time. Ignore the dimmed spans.

**3. Switch to Flamechart** — Focus on where time is spent. Use depth filter and subtree focus to drill in.

**4. Compare across runs** — Switch to Comparison view to verify that changes improved (or didn't regress) performance.

**Quick reference:**
- Waterfall = *when* things happen (timeline positioning)
- Flamechart = *where* time goes (proportional sizing)
- Critical path = *what matters* for total time
- Comparison = *what changed* between runs

All three views share the same span data, detail panel, and filtering — they're just different lenses on the same trace.`,
      tips: [
        "The insights footer always shows total duration, span count, slowest span, and phase breakdown",
        "Critical path analysis + flamechart is the most effective combination for finding optimization targets",
        "Comparison is most useful after making a change — run before and after, then compare",
      ],
      estimatedDuration: 1,
    },
  ],
};
