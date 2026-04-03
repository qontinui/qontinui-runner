/**
 * Trace Observability Enhancement Tutorial (Informational)
 *
 * Explains the 7 new observability features added to the trace viewer:
 * token/cost overlay, AI session drill-down, agent communication graph,
 * GenAI attributes, step-through replay, trace timeline minimap, and
 * comparison filtering.
 *
 * This is a companion to the "trace-analysis" tutorial which covers the
 * original 3 views (waterfall, flamechart, comparison) and critical path.
 */

import type { Tutorial } from "../../../types/tutorial";

export const traceObservabilityTutorial: Tutorial = {
  id: "trace-observability",
  title: "Trace Observability: Tokens, Replay & Agent Graphs",
  description:
    "Explore the trace viewer's advanced observability features — token cost heatmaps, AI session drill-down with tool call visibility, agent pipeline graphs, step-through replay, timeline minimap, and GenAI attribute interoperability.",
  duration: "10 minutes",
  difficulty: "intermediate",
  mode: "contextual",
  focusPage: "gui-automation",
  category: "Observability",
  tags: [
    "featured",
    "traces",
    "observability",
    "tokens",
    "cost",
    "replay",
    "agent-graph",
    "genai",
  ],
  prerequisites: ["trace-analysis"],
  learningObjectives: [
    "Use token/cost heatmap overlays to find expensive spans at a glance",
    "Drill into AI sessions to see prompt construction, tool calls, and token usage",
    "Read GenAI (OpenTelemetry) attributes for cross-tool interoperability",
    "Visualize multi-agent pipeline topology in the Agent Graph view",
    "Step through execution decisions using the Replay controller",
    "Navigate large traces with the Timeline Minimap",
    "Filter comparison results by name, delta threshold, and phase",
  ],
  steps: [
    // =========================================================================
    // 1. Introduction
    // =========================================================================
    {
      id: "intro",
      title: "Beyond Waterfall and Flamechart",
      content: `The trace viewer started with three views — Waterfall, Flamechart, and Comparison. Now it has **seven additional capabilities** that transform it from a simple timeline viewer into a full observability workbench.

These features answer questions the original views couldn't:
- **Where is the money going?** → Token/cost overlay
- **What did the AI actually do?** → AI session drill-down
- **How do agents talk to each other?** → Agent communication graph
- **Can I step through the execution?** → Replay mode
- **Where am I in a large trace?** → Timeline minimap

Let's explore each one.`,
      tips: [
        "All features work with existing trace data — no new workflows needed",
        "Features are additive: they overlay on the existing waterfall and flamechart views",
      ],
      estimatedDuration: 1,
    },

    // =========================================================================
    // 2. Token/Cost Overlay
    // =========================================================================
    {
      id: "token-overlay-intro",
      title: "Token/Cost Heatmap Overlay",
      content: `The **Token Overlay** adds a color heatmap to the waterfall and flamechart views, tinting span bars based on their token consumption or cost.

Find the **"No token overlay"** dropdown in the toolbar (next to the Critical Path toggle). It has four options:
- **Off** — normal view, no heatmap
- **Input tokens** — red tint proportional to input tokens consumed
- **Output tokens** — red tint proportional to output tokens generated
- **Cost** — red tint proportional to estimated cost in cents

The redder a span bar appears, the more tokens (or cost) it consumed relative to the most expensive span in the trace.`,
      targetElement: {
        selector: '[data-tutorial-id="trace-view-mode-bar"]',
        highlightType: "border",
        position: "bottom",
      },
      tips: [
        "The heatmap uses a 0-1 scale normalized against the max value in the trace",
        "Cost is computed from token counts × model pricing (Claude, Gemini rates)",
        "Token data comes from execution_spans columns: input_tokens, output_tokens, cost_cents",
      ],
      details: `**How token data flows:**

1. During an AI session, the Claude CLI stream-json result message contains \`usage.input_tokens\` and \`usage.output_tokens\`
2. These are recorded on the tracing span as \`ai.input_tokens\` and \`ai.output_tokens\` attributes
3. The \`SqliteSpanLayer\` denormalizes them into dedicated columns on the \`execution_spans\` table
4. Cost is computed via \`ai_pricing::calculate_cost_cents(input, output, model_id)\`
5. The frontend fetches spans and computes heat values using \`getTokenHeat(span, mode, maxValue)\`

The overlay works on both Waterfall (rgba background on SpanRow) and Flamechart (blended fill color on canvas rects).`,
      estimatedDuration: 1.5,
    },

    {
      id: "token-overlay-footer",
      title: "Token Stats in Performance Insights",
      content: `When token data is available, the **Performance Insights** footer bar (at the bottom of the trace viewer) shows aggregate stats:

**Tokens: 12,450↑ 8,230↓** — total input tokens (up arrow) and output tokens (down arrow) across all spans in the trace.

**Est. cost: $0.1247** — total estimated cost computed from token counts and model pricing.

These appear alongside the existing stats (total duration, span count, error count, slowest span, critical path info, phase breakdown).`,
      targetElement: {
        selector: '[data-tutorial-id="trace-performance-insights"]',
        highlightType: "spotlight",
        position: "top",
      },
      tips: [
        "Cost only appears when > $0.00 — spans without token data contribute $0",
        "The phase breakdown still shows duration, not tokens (they're separate metrics)",
      ],
      estimatedDuration: 0.5,
    },

    // =========================================================================
    // 3. AI Session Drill-Down
    // =========================================================================
    {
      id: "ai-session-drilldown",
      title: "AI Session Drill-Down",
      content: `Click on any span named **\`qontinui.ai.session\`** in the waterfall. The **Span Detail Panel** (right sidebar) now shows three new sections:

**Token Usage** — A grid showing input/output token counts and estimated cost, with color-coded backgrounds (blue for input, green for output).

**Tool Calls** — A list of child \`tool_call\` spans showing what tools the AI invoked during this session (Read, Edit, Bash, etc.) with each call's duration.

**GenAI (OpenTelemetry)** — Industry-standard \`gen_ai.*\` attributes: system, model, temperature, max_tokens, usage.input_tokens, usage.output_tokens.

This turns a single opaque "AI session" span into a transparent view of what the AI actually did.`,
      targetElement: {
        selector: '[data-tutorial-id="trace-span-detail-panel"]',
        highlightType: "spotlight",
        position: "left",
      },
      tips: [
        "Tool call spans are matched by parent_span_id — they're children of the AI session span",
        "AI session spans also emit a prompt_build sub-span showing prompt construction time",
        "GenAI attributes are separated from generic attributes to avoid clutter",
      ],
      details: `**Sub-spans emitted during AI sessions:**

| Span Name | When | What it Shows |
|-----------|------|---------------|
| \`qontinui.ai.prompt_build\` | During prompt construction | Time spent building the prompt |
| \`qontinui.ai.tool_call\` | For each tool the AI invokes | Tool name, one span per invocation |

**GenAI semantic conventions** follow the OpenTelemetry GenAI specification:
- \`gen_ai.system\` = "anthropic"
- \`gen_ai.request.model\` = model name (from config.model_override)
- \`gen_ai.usage.input_tokens\` / \`gen_ai.usage.output_tokens\`
- \`gen_ai.request.temperature\` / \`gen_ai.request.max_tokens\`

These attributes enable interoperability with Phoenix, Langfuse, Grafana, and other observability tools that understand the OpenTelemetry GenAI conventions.`,
      estimatedDuration: 1.5,
    },

    // =========================================================================
    // 4. Agent Communication Graph
    // =========================================================================
    {
      id: "agent-graph",
      title: "Agent Communication Graph",
      content: `For workflows that use the **multi-agent pipeline** (spec analyst → locator → implementer → verifier), a fourth tab appears: **Agent Graph**.

This tab renders a **directed graph** using ReactFlow + dagre layout, showing:
- **Nodes** — one per agent phase, labeled with the agent's role (e.g., "spec_analyst", "implementer")
- **Edges** — data flow between agents (delegation, results)
- **Status** — green nodes for success, red for errors
- **Metrics** — duration and token count displayed on each node

The graph answers "how did the agents collaborate?" at a glance. Click any node to select the corresponding span in the detail panel.`,
      targetElement: {
        selector: '[data-tutorial-id="trace-view-tabs"]',
        highlightType: "border",
        position: "bottom",
      },
      tips: [
        "The Agent Graph tab only appears when agent pipeline spans exist in the trace",
        "If no explicit message spans exist, edges fall back to sequential ordering by start time",
        "Node colors use dark-mode-safe rgba (not bright pastels) for readability",
      ],
      details: `**Agent pipeline span hierarchy:**

\`\`\`
qontinui.agent.pipeline (root)
├── qontinui.agent.phase (agent.role="spec_analyst")
├── qontinui.agent.message (source→target, type="delegation")
├── qontinui.agent.phase (agent.role="locator")
├── qontinui.agent.message (source→target, type="delegation")
├── qontinui.agent.phase (agent.role="implementer")
│   ├── qontinui.agent.phase (subtree_orchestrator)
│   │   ├── qontinui.agent.phase (implementer, level=0)
│   │   └── qontinui.agent.phase (verifier, level=0)
│   └── ...
└── qontinui.agent.phase (agent.role="verifier")
\`\`\`

These spans are emitted from \`multi_agent_pipeline_loop.rs\` and persisted to the \`execution_spans\` table via \`SUMMARY_SPANS\`.`,
      estimatedDuration: 1.5,
    },

    // =========================================================================
    // 5. Step-Through Replay
    // =========================================================================
    {
      id: "replay-intro",
      title: "Step-Through Replay",
      content: `The **Replay** button in the toolbar activates a debugger-style mode for walking through execution decisions one step at a time.

When active, a **Replay Controller** bar appears with transport controls:
- ⏮ **Start** / ⏭ **End** — jump to first/last step
- ◀ **Back** / ▶ **Forward** — step one at a time (or use ← → arrow keys)
- ▶/⏸ **Play/Pause** — auto-advance at configurable speed (0.5x to 4x)
- 🎚 **Scrubber** — drag to any position in the step timeline

The current step's span is highlighted with a **pulsing blue ring** in the waterfall, and the right panel shows a **Replay Panel** with step context: type, timing, status, tokens, and state snapshot data.`,
      targetElement: {
        selector: '[data-tutorial-id="trace-view-mode-bar"]',
        highlightType: "border",
        position: "bottom",
      },
      tips: [
        "Press Space to toggle play/pause, arrow keys to step forward/back",
        "Keyboard shortcuts are disabled when typing in search/filter inputs",
        "Replay mode overlays on the waterfall view — you can see both the timeline and the current step",
        "Exit replay by clicking the button again or pressing the ✕ in the controller bar",
      ],
      estimatedDuration: 1,
    },

    {
      id: "replay-steps",
      title: "What Becomes a Replay Step?",
      content: `The replay system derives steps from span data. Four types of spans become replay steps:

| Step Type | Span Pattern | What It Represents |
|-----------|-------------|-------------------|
| **Phase Transition** | \`qontinui.workflow.phase.*\` | Entering setup, verification, agentic, or completion |
| **AI Session** | \`qontinui.ai.session\` | An AI conversation with token counts |
| **Tool Call** | \`qontinui.ai.tool_call\` | A specific tool invocation (Read, Edit, Bash) |
| **Agent Decision** | \`qontinui.agent.phase\` | An agent role executing within a pipeline |

Steps are sorted chronologically. The Replay Panel shows the current step's type (color-coded), title, description, timing, status, and any state snapshot context.`,
      tips: [
        "Steps without token data still appear — they just don't show the Tokens section",
        "State snapshots (from execution_state_snapshots table) add 'before/after' context",
        "Unrecognized span names (like python.startup) are skipped — they're not meaningful decision points",
      ],
      details: `**State snapshots** are emitted at key execution points:
- After each AI session completes (phase, step name, token counts, injected steps)
- At phase transitions (entering verification, agentic, etc.)
- After verification results (pass/fail counts, iteration number)
- When the AI injects new steps (step names and count)

These snapshots are stored in the \`execution_state_snapshots\` table and linked to the Replay Panel via span matching. They provide the "what changed?" context that makes replay useful for debugging.`,
      estimatedDuration: 1,
    },

    // =========================================================================
    // 6. Trace Timeline Minimap
    // =========================================================================
    {
      id: "minimap",
      title: "Trace Timeline Minimap",
      content: `For traces with **10 or more spans**, a thin **40px canvas bar** appears between the toolbar and the main view. This is the **Timeline Minimap** — a compressed overview of the entire trace.

**What you see:**
- Every span rendered as a colored bar, positioned proportional to its start time
- Bars are stacked by tree depth (deeper spans render lower)
- Phase colors match the waterfall: blue (setup), green (verification), purple (agentic), amber (completion), pink (AI), cyan (python)
- Error spans are red
- A **blue-bordered viewport rectangle** shows which portion of the trace is currently visible in the waterfall below

**Interaction:**
- **Click** anywhere on the minimap to jump to that position
- **Drag** the viewport to pan through the trace
- The minimap updates when you scroll the waterfall (bi-directional sync)`,
      targetElement: {
        selector: '[data-tutorial-id="trace-viewer-widget"]',
        highlightType: "border",
        position: "top",
      },
      tips: [
        "The minimap is hidden for small traces (< 10 spans) to avoid visual noise",
        "It appears for both Waterfall and Flamechart views, but not Comparison or Agent Graph",
        "Areas outside the viewport are dimmed with a 50% black overlay for contrast",
      ],
      estimatedDuration: 1,
    },

    // =========================================================================
    // 7. Comparison Filtering
    // =========================================================================
    {
      id: "comparison-filtering",
      title: "Comparison View Filters",
      content: `The **Comparison** view now has a **filter bar** between the status tabs and the match list. Three filters work together:

1. **Name search** — type to filter matches by span name (case-insensitive substring match)
2. **Min Δ (ms)** — only show matches where the duration delta exceeds this threshold
3. **Phase dropdown** — filter to a specific workflow phase (Setup, Verification, Agentic, Completion, AI)

These combine with the existing **status tabs** (All, Slower, Faster, Added, Removed, Status Changed, Unchanged) for precise regression hunting.

Example: Select "Slower" status tab + "agentic" phase + min delta 500ms to find agentic spans that regressed by more than half a second.`,
      tips: [
        "All three filters apply simultaneously (AND logic)",
        "The match count updates as you filter",
        "Phase is inferred from span names using the same inferPhase() logic as the waterfall",
      ],
      estimatedDuration: 0.5,
    },

    // =========================================================================
    // 8. GenAI Interoperability
    // =========================================================================
    {
      id: "genai-interop",
      title: "GenAI Attributes & OTLP Export",
      content: `Every AI session span now emits **dual attributes** — both \`qontinui.*\` (internal) and \`gen_ai.*\` (OpenTelemetry standard).

This means you can export traces to external observability tools:
- **Jaeger** — search by \`gen_ai.system = "anthropic"\`
- **Grafana + Tempo** — build dashboards with token usage time series
- **Langfuse** — import traces using the built-in Langfuse adapter
- **Phoenix/Arize** — compatible via standard OTLP export

Enable OTLP export in Settings → set the endpoint (default: \`localhost:4317\`), restart the runner, and your traces flow to any OpenTelemetry-compatible collector.`,
      tips: [
        "GenAI attributes appear in the SpanDetailPanel under a dedicated 'GenAI (OpenTelemetry)' section",
        "The prefix is stripped for readability: 'gen_ai.request.model' shows as 'request.model'",
        "A TraceImportAdapter interface + Langfuse adapter exist for importing external traces",
      ],
      details: `**GenAI attributes emitted on AI session spans:**

| Attribute | Example Value |
|-----------|--------------|
| \`gen_ai.system\` | "anthropic" |
| \`gen_ai.request.model\` | "claude-sonnet-4-20250514" |
| \`gen_ai.request.temperature\` | 0.7 |
| \`gen_ai.request.max_tokens\` | 4096 |
| \`gen_ai.usage.input_tokens\` | 12450 |
| \`gen_ai.usage.output_tokens\` | 8230 |

**OTLP export architecture:**
- Rust backend uses \`opentelemetry-otlp\` (v0.27) with gRPC transport
- Spans are exported via the \`tracing-opentelemetry\` layer (added first in the subscriber registry)
- Configuration: \`OtelConfig { enabled, endpoint, sampling_rate, service_name }\`
- Graceful degradation: if the collector is unreachable, the runner continues without telemetry`,
      estimatedDuration: 1,
    },

    // =========================================================================
    // 9. Putting It Together
    // =========================================================================
    {
      id: "mental-model",
      title: "The Observability Stack",
      content: `Here's the mental model for how all these features connect:

**Data flow:** Step execution → tracing spans → SQLite + JSONL + OTLP → trace viewer

**Three layers of insight:**
1. **Timeline** (Waterfall/Flamechart + Minimap) — *when* things happened
2. **Cost** (Token overlay + AI drill-down) — *what it cost* in tokens and dollars
3. **Structure** (Agent Graph + Replay) — *why* decisions were made

**Cross-tool interop:** GenAI attributes + OTLP export → Jaeger, Grafana, Langfuse, Phoenix

The trace viewer is no longer just a debugging tool — it's a window into the economics and decision-making of your autonomous workflows.`,
      tips: [
        "Start with the token overlay to find expensive spans, then drill into them",
        "Use replay to understand WHY an AI session made certain tool calls",
        "Use the agent graph to verify pipeline topology in multi-agent workflows",
        "Export to Grafana for long-term trend analysis across many workflow runs",
      ],
      resources: [
        {
          title: "OTLP Validation Guide",
          url: "qontinui-dev-notes/implementation-docs/trace-explorer-otlp-validation.md",
        },
        {
          title: "AgentPrism Evaluation",
          url: "qontinui-dev-notes/implementation-docs/agent-prism-evaluation.md",
        },
      ],
      estimatedDuration: 1,
    },
  ],
};
